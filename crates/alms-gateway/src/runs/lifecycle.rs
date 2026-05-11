//! Run creation, execution, and completion — the core run lifecycle.

use super::tools::{RuntimeEventForwarder, forward_runtime_events};
use super::{RunParams, is_internal_context_id, resolve_agent_config};
use crate::api_error;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{
    AgentId, AlmsError, CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus,
    RunStatusResponse, SessionId, classify_session_type, sanitize_error_for_session,
};
use alms_runtime::RuntimeEvent;
use alms_tools::message_sender::ConversationEndReason;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

/// GET /runs?session_id=<uuid>&limit=<n> — list runs for a session (existing)
/// GET /runs?agent_id=<uuid>&limit=<n> — list runs across all sessions for an agent
///
/// Exactly one of `session_id` or `agent_id` must be provided. Providing both
/// returns 400 BAD_REQUEST.
/// When `agent_id` is provided, the response includes enriched run entries
/// with `session_type`, `trigger`, `context_id`, and `duration_ms` fields
/// for the agent run log panel.
#[instrument(level = "info", skip(state, params))]
pub async fn list_runs(
    State(state): State<AppState>,
    Query(params): Query<ListRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = params.limit.unwrap_or(50);

    // Reject ambiguous requests that supply both filters.
    if params.session_id.is_some() && params.agent_id.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "AMBIGUOUS_FILTER",
            "Provide either `session_id` or `agent_id`, not both",
        ));
    }

    if let Some(agent_id) = params.agent_id {
        // Agent-level listing: cross-session runs with enriched metadata.
        let runs = state.run_manager.list_by_agent(agent_id, limit);
        let entries: Vec<AgentRunEntry> = runs
            .into_iter()
            .map(|run| enrich_run(&state, run))
            .collect();
        Ok(Json(serde_json::json!({ "runs": entries })))
    } else if let Some(session_id) = params.session_id {
        // Session-level listing: original behaviour (backwards-compatible).
        let runs = state.run_manager.list_by_session(session_id, limit);
        let responses: Vec<RunStatusResponse> =
            runs.into_iter().map(RunStatusResponse::from).collect();
        Ok(Json(serde_json::json!({ "runs": responses })))
    } else {
        Err(api_error(
            StatusCode::BAD_REQUEST,
            "MISSING_FILTER",
            "At least one of `session_id` or `agent_id` must be provided",
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    pub session_id: Option<SessionId>,
    pub agent_id: Option<AgentId>,
    pub limit: Option<usize>,
}

/// Enriched run entry for the agent run log panel.
///
/// Extends `RunStatusResponse` with session type classification, trigger
/// derivation, context ID, and computed duration.
#[derive(Debug, Serialize)]
pub struct AgentRunEntry {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub status: RunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub usage: Option<alms_core::TokenUsage>,
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<alms_core::JobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<u32>,
    /// Session type: "chat", "dm", "notification", "job", "subagent",
    /// "telegram", "episodic"
    pub session_type: String,
    /// Trigger derivation: "user", "scheduled", "subagent", "dm",
    /// "notification", "telegram"
    pub trigger: String,
    /// The session's context_id (for display and navigation).
    pub context_id: String,
    /// Run duration in milliseconds (None if run has not ended).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// Enrich a `Run` with session type, trigger, context_id, and duration.
fn enrich_run(state: &AppState, run: Run) -> AgentRunEntry {
    // Look up the session to get context_id for classification.
    let (context_id, session_type) = match state.session_manager.get(run.session_id) {
        Ok(session) => {
            let st = classify_session_type(&session.context_id).to_string();
            (session.context_id.clone(), st)
        }
        Err(_) => ("unknown".to_string(), "chat".to_string()),
    };

    // Derive trigger from run metadata and session context.
    let trigger = derive_trigger(&run, &context_id);

    // Compute duration if both started_at and ended_at are present.
    let duration_ms = match (run.started_at, run.ended_at) {
        (Some(start), Some(end)) => Some((end - start).num_milliseconds()),
        _ => None,
    };

    let ts = run
        .ended_at
        .unwrap_or_else(|| run.started_at.unwrap_or(run.created_at));

    // Attach tool call count if SQLite is available.
    let tool_call_count = state
        .session_manager
        .store()
        .and_then(|store| store.count_tool_calls(run.run_id).ok());

    /// Max characters to include in the `response` field of listing entries.
    /// Full text is available via `GET /runs/{run_id}`.
    const RESPONSE_TRUNCATE_LEN: usize = 200;

    let response = run.output.map(|s| {
        if s.len() > RESPONSE_TRUNCATE_LEN {
            let mut truncated = s[..RESPONSE_TRUNCATE_LEN].to_string();
            truncated.push_str("...");
            truncated
        } else {
            s
        }
    });

    AgentRunEntry {
        run_id: run.run_id,
        session_id: run.session_id,
        agent_id: run.agent_id,
        status: run.status,
        response,
        error: run.error,
        started_at: run.started_at,
        ended_at: run.ended_at,
        usage: run.usage,
        ts,
        job_id: run.job_id,
        parent_run_id: run.parent_run_id,
        tool_call_count,
        session_type,
        trigger,
        context_id,
        duration_ms,
    }
}

/// Derive the trigger type for a run based on its metadata and session context.
///
/// Priority:
/// 1. `job_id` present -> "scheduled"
/// 2. `parent_run_id` present -> "subagent"
/// 3. Delegate to [`classify_session_type`] for context_id-based classification,
///    mapping session types to trigger names (dm, notification, telegram).
/// 4. Default -> "user"
fn derive_trigger(run: &Run, context_id: &str) -> String {
    if run.job_id.is_some() {
        return "scheduled".to_string();
    }
    if run.parent_run_id.is_some() {
        return "subagent".to_string();
    }
    // Delegate to the canonical session type classifier for context_id-based
    // triggers, avoiding duplicated prefix checks.
    match classify_session_type(context_id) {
        "dm" => "dm".to_string(),
        "notification" => "notification".to_string(),
        "telegram" => "telegram".to_string(),
        _ => "user".to_string(),
    }
}

/// GET /runs/{run_id}/tool-calls — list tool call records for a run.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn get_run_tool_calls(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Verify the run exists.
    state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    let records = state
        .session_manager
        .store()
        .map(|store| store.load_tool_calls(run_id))
        .transpose()
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                format!("Failed to load tool calls: {e}"),
            )
        })?
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "tool_calls": records,
    })))
}

/// POST /runs/{run_id}/cancel — cancel a running or queued run.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn cancel_run(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let already_finished = || {
        api_error(
            StatusCode::CONFLICT,
            "ALREADY_FINISHED",
            "Run already finished",
        )
    };

    let run = state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    match run.status {
        RunStatus::Queued | RunStatus::Running => {}
        _ => {
            return Err(already_finished());
        }
    }

    let found = state.run_manager.cancel_run(run_id);
    if !found {
        return Err(already_finished());
    }

    info!("Cancel requested for run {}", run_id.0);

    Ok(Json(serde_json::json!({
        "run_id": run_id.0.to_string(),
        "status": "cancelled",
    })))
}

/// Cross-validate the resolved per-run `(provider, model, max_input_tokens,
/// max_tokens)` quadruple against the provider's published context window
/// (#919).
///
/// Returns:
/// - `Ok(())` when the budget fits, when the table doesn't know the
///   `(provider, model)` pair, when the agent is on the mock LLM, or when
///   the validator is in `warn` mode (in which case the overshoot is
///   downgraded to a structured WARN log).
/// - `Err(TokenBudgetError)` when strict mode is active and the
///   table-known cap is exceeded. The caller (HTTP `create_run` or non-HTTP
///   `execute_run`) maps the structured error onto its own surface — a
///   400 JSON body for the synchronous request path or a `run_error` SSE
///   event + `mark_run_as_failed` for the queued path.
///
/// Centralising the mock-skip, env-mode dispatch, and structured-WARN log
/// in one helper means every run-creation path — HTTP `POST /runs`,
/// scheduler triggers, peer DMs, notification runs, and subagent
/// completion runs — sees the same enforcement contract. The Codex P2
/// follow-up that motivated the `execute_run` call site flagged that the
/// HTTP-only guard left the non-HTTP paths exposed to the same opaque
/// downstream 4xx the validator is meant to prevent.
fn evaluate_pre_flight_token_budget(
    agent_id: AgentId,
    agent_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
    rejection_log_message: &str,
) -> Result<(), alms_core::config::TokenBudgetError> {
    // Mock mode: never enforce the cap. Mirrors `AlmsConfig::validate`'s
    // boot-time skip — the mock client does not enforce a real provider's
    // cap, so refusing an intentionally-overshoot budget here would block
    // local/dev mock runs that pass at boot. Same policy in both skip
    // paths (Codex P2 #1 follow-up on PR #1020).
    if llm.is_mock() {
        return Ok(());
    }

    let provider = llm.provider();
    let model = llm.default_model();
    let max_input_tokens = agent_config.context_config.max_input_tokens;
    let max_tokens = agent_config.max_tokens;

    let Err(err) =
        alms_core::config::validate_token_budget(provider, model, max_input_tokens, max_tokens)
    else {
        return Ok(());
    };

    match alms_core::config::ValidationMode::from_env() {
        alms_core::config::ValidationMode::Strict => {
            warn!(
                target: "alms.config",
                agent_id = %agent_id,
                provider = %err.provider,
                model = %err.model,
                max_input_tokens = err.max_input_tokens,
                max_tokens = err.max_tokens,
                effective_total = err.effective_total,
                provider_cap = err.provider_cap,
                "{}",
                rejection_log_message,
            );
            Err(err)
        }
        alms_core::config::ValidationMode::Warn => {
            warn!(
                target: "alms.config",
                agent_id = %agent_id,
                provider = %err.provider,
                model = %err.model,
                max_input_tokens = err.max_input_tokens,
                max_tokens = err.max_tokens,
                effective_total = err.effective_total,
                provider_cap = err.provider_cap,
                "{}",
                err.message()
            );
            Ok(())
        }
    }
}

/// HTTP-shaped wrapper around [`evaluate_pre_flight_token_budget`].
///
/// Mirrors the `MissingModelAfterProviderSwitch` 400 error envelope so
/// every gateway-side budget rejection lands with a consistent shape on
/// the wire. Used only by the synchronous `POST /runs` handler; the
/// queued / non-HTTP path uses
/// [`fail_run_with_token_budget_error`] instead.
fn pre_flight_token_budget(
    agent_id: AgentId,
    agent_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
) -> Result<(), (StatusCode, axum::Json<serde_json::Value>)> {
    match evaluate_pre_flight_token_budget(
        agent_id,
        agent_config,
        llm,
        "Rejecting POST /runs with INVALID_TOKEN_BUDGET_FOR_PROVIDER (#919)",
    ) {
        Ok(()) => Ok(()),
        Err(err) => Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
                "message": err.message(),
                "agent_id": agent_id.0.to_string(),
                "provider": err.provider,
                "model": err.model,
                "max_input_tokens": err.max_input_tokens,
                "max_tokens": err.max_tokens,
                "effective_total": err.effective_total,
                "provider_cap": err.provider_cap,
            })),
        )),
    }
}

/// POST /runs - Create a new run
///
/// Per API spec: Returns 201 Created with { run_id, session_id, status: "queued", ts }
#[instrument(level = "info", skip(state, req), fields(session_id = %req.session_id.0))]
pub async fn create_run(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<CreateRunResponse>), (StatusCode, Json<serde_json::Value>)> {
    let session = match state.session_manager.get(req.session_id) {
        Ok(session) => session,
        Err(_) => {
            return Err(api_error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Session not found",
            ));
        }
    };

    let input_text = match req.input {
        RunInput::Text { text } => text,
    };

    let session_agent_id = session.agent_id;
    let is_shared_session = session_agent_id.is_nil();
    let agent_id = match req.agent_id {
        Some(requested) if requested != session_agent_id && !is_shared_session => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "AGENT_SESSION_MISMATCH",
                "Session belongs to a different agent",
            ));
        }
        Some(requested) => requested,
        None if is_shared_session => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "AGENT_ID_REQUIRED",
                "Shared sessions require agent_id so per-agent config can be resolved",
            ));
        }
        None => session_agent_id,
    };

    let run = Run::new(session.id, agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;
    let context_id = session.context_id.clone();

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    // Reject new runs during shutdown.
    if state.shutdown_token.is_cancelled() {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SHUTTING_DOWN",
            "Server is shutting down",
        ));
    }

    // #863: pre-flight per-agent config resolution so a per-agent provider
    // override with no model on any layer is rejected with a clean 400
    // BEFORE any LLM call rather than producing an opaque downstream 4xx
    // (e.g. Anthropic 404 on `model: ""`). The resolve is pure (no
    // side-effects) and `execute_run` will resolve again under the live
    // secrets / agent_config locks at run time — this pre-flight only
    // catches the deterministic config-shape failure mode.
    //
    // #919: when the resolve succeeds, also cross-validate the resulting
    // `(provider, model, max_input_tokens, max_tokens)` quadruple against
    // the provider's published context window. Per-agent overrides
    // (model, provider) can change the effective cap relative to the
    // load-time default, so re-running the validator here catches
    // configurations that pass at boot but blow the cap on a specific
    // agent's resolved shape.
    {
        let base_agent_config = state.agent_config.read().clone();
        match super::resolve_agent_config(
            agent_id,
            &state.session_manager,
            &base_agent_config,
            &state.llm,
            Some(&state.secrets.read()),
        ) {
            Err(super::ResolveAgentConfigError::MissingModelAfterProviderSwitch {
                agent_id: ag,
                new_provider,
                prev_provider,
            }) => {
                warn!(
                    agent_id = %ag,
                    new_provider = %new_provider,
                    prev_provider = %prev_provider,
                    "Rejecting POST /runs with MISSING_MODEL_AFTER_PROVIDER_SWITCH (#863)"
                );
                return Err((
                    StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "error_code": "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
                        "message": format!(
                            "agent {ag} overrides provider to {new_provider} but no \
                             model was supplied; previous provider {prev_provider}'s \
                             default cannot be reused"
                        ),
                        "agent_id": ag.0.to_string(),
                        "new_provider": new_provider,
                        "prev_provider": prev_provider,
                    })),
                ));
            }
            Ok(resolved) => {
                // #919 per-run token-budget validation.
                pre_flight_token_budget(agent_id, &resolved.agent_config, &resolved.llm)?;
            }
        }
    }

    state.run_manager.insert_run(run.clone());

    // Pre-persist the user's input message to the session BEFORE enqueueing
    // the run.  Previously the input was only persisted inside
    // `runtime.run()` (when the agent loop actually starts), which meant a
    // page reload during the queued-wait window would find no trace of the
    // user's message -- the UI had optimistically rendered it but the
    // backend had never stored it.
    //
    // The stored message carries `pending_input: true` metadata so
    // `execute_run` knows to skip re-persistence (via the new
    // `input_pre_persisted` RunParams flag, which routes the run through
    // `run_on_session` instead of `run`).  The metadata is informational
    // only -- the frontend does NOT filter these out, because the user's
    // message should be visible in the chat immediately after sending
    // regardless of run status (queued, running, or completed).
    let user_msg = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::Text(run.input.clone()),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({ "pending_input": true })),
    };
    let input_pre_persisted = match state.session_manager.append_message(session_id, user_msg) {
        Ok(()) => true,
        Err(e) => {
            // Non-fatal: fall back to the runtime's legacy persistence path
            // so the input is not lost entirely.  The page-reload window is
            // the only scenario where this matters, so the fallback is
            // acceptable.
            warn!(
                "Failed to pre-persist user input for run {}: {}",
                run_id.0, e
            );
            false
        }
    };

    // Notify session-level SSE subscribers that a new run was created.
    //
    // `queued_behind` tells the UI how many runs are ahead of this one so
    // it can show "Queued -- waiting for agent..." instead of a misleading
    // "Thinking...".
    //
    // `SessionQueue::pending_count` counts items still *waiting* to be
    // picked up by the handler -- the currently-executing work item has
    // already been dequeued and its counter decremented.  So we also add 1
    // if the agent has a `Running` run, to catch the common case of a busy
    // agent with nothing else queued behind it yet.
    //
    // Known residual race: there is a narrow sub-millisecond TOCTOU window
    // between `pending.fetch_sub(1)` inside the queue handler and
    // `mark_run_as_running` in `execute_run` (lifecycle.rs:~495). During
    // that window both `pending_count` and `agent_has_running_run` read
    // false, so a concurrent `create_run` can report `queued_behind = 0`
    // and render "Thinking" instead of "Queued". The window is bounded by
    // executor dispatch latency. Closing it would require a separate
    // in-flight counter inside `SessionQueue` that is incremented on
    // dequeue (before `work.await`) and decremented after the work
    // future resolves; considered low priority.
    let agent_running = state.run_manager.agent_has_running_run(agent_id);
    let queued_behind = state.agent_queue.pending_count(&agent_id) + usize::from(agent_running);
    state
        .run_manager
        .send_session_event(
            session_id,
            run_id,
            SseEventData::run_created(
                run_id,
                session_id,
                false,
                Some("user".to_string()),
                queued_behind,
            ),
        )
        .await;

    // Create per-run cancellation token BEFORE enqueue so cancelling a
    // queued-but-not-yet-started run works.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let state_clone = state.clone();
    state.agent_queue.enqueue(
        agent_id,
        Box::pin(async move {
            execute_run(
                state_clone,
                RunParams {
                    run_id,
                    session_id,
                    agent_id,
                    input: run.input,
                    context_id,
                    cancel_token,
                    is_peer_message: false,
                    is_system_triggered: false,
                    input_pre_persisted,
                },
            )
            .await;
        }),
    );

    let response = CreateRunResponse {
        run_id,
        session_id,
        status: RunStatus::Queued,
        ts: Utc::now(),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// Resolve the effective posture for a run.
///
/// System-triggered runs (peer DMs, notification runs, subagent completions)
/// have no human in the loop, so Guarded posture would hang forever waiting
/// for approval. This function overrides Guarded to Autonomous for those
/// runs while leaving all other postures unchanged.
pub(super) fn resolve_posture_for_run(
    posture: alms_runtime::Posture,
    is_system_triggered: bool,
) -> alms_runtime::Posture {
    if is_system_triggered && posture == alms_runtime::Posture::Guarded {
        alms_runtime::Posture::Autonomous
    } else {
        posture
    }
}

/// Extract the peer agent name from a `dm:{name1}:{name2}` context ID.
///
/// Delegates to [`alms_core::dm_peer`].  Returns `None` if the context ID
/// does not match the expected format or neither name matches `agent_name`.
pub(super) fn extract_peer_from_dm_context(context_id: &str, agent_name: &str) -> Option<String> {
    alms_core::dm_peer(context_id, agent_name).map(|s| s.to_string())
}

/// RAII guard that calls [`RunManager::untrack_in_flight`] on drop.
///
/// This ensures the in-flight counter is always decremented even when the
/// run task panics, preventing `wait_drain` from blocking indefinitely.
struct InFlightGuard {
    run_manager: crate::server::RunManager,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.run_manager.untrack_in_flight();
    }
}

/// Build the LLM client used for the per-run summary task (#866 + #871).
///
/// Thin wrapper around `alms_runtime::build_summary_client` that adds the
/// gateway-side contextual `info!` log (the runtime helper is shared with the
/// coordinator subagent path and stays log-agnostic so each caller can label
/// with its own ID — `agent_id` here, `task_id` in the coordinator). All the
/// leak-guard logic lives in the runtime helper so future drift is impossible.
///
/// See `alms_runtime::summary_client` for the full rule set.
fn build_summary_client(
    llm: &alms_runtime::LlmClient,
    summary_provider: Option<&str>,
    summary_model: Option<&str>,
    secrets: &alms_core::secrets::SecretsStore,
    agent_id: AgentId,
) -> alms_runtime::LlmClient {
    let switched =
        alms_runtime::build_summary_client(llm, summary_provider, summary_model, Some(secrets));

    if let Some(provider) = summary_provider {
        info!(
            agent_id = %agent_id,
            agent_provider = %llm.provider(),
            summary_provider = %provider,
            summary_model = %summary_model.unwrap_or("<inherit>"),
            "Using dedicated provider for summary task (#866)"
        );
    }

    switched
}

/// Maximum bytes of an `AlmsError` Display string to embed in the
/// `ConversationEndReason::Errored { message }` payload.
///
/// Bounds the size of the peer-side notification text so a verbose error
/// (e.g. an LLM JSON dump) does not balloon the `dm_ended` marker / SSE
/// frame. Truncation is done on a UTF-8 char boundary.
const PEER_ERROR_MESSAGE_MAX_LEN: usize = 300;

/// Build the human-readable error string carried in
/// `ConversationEndReason::Errored { message }` for the peer-side
/// notification.
///
/// Routes the error through [`sanitize_error_for_session`] first so any
/// raw provider details (URLs, API keys, response bodies) are collapsed
/// to a category label before reaching the peer agent's notification
/// context. This closes the same threat surface that #911 / #930 closed
/// on the failing agent's own session — see issue #931. Sanitisation is
/// inlined (rather than bolted onto the call sites) so future callers
/// of this helper cannot accidentally skip it.
///
/// After sanitisation the result is truncated to
/// [`PEER_ERROR_MESSAGE_MAX_LEN`] on a UTF-8 char boundary and an
/// ellipsis is appended when truncated. In practice the sanitiser
/// already collapses to a short fixed label, so the truncation step is
/// effectively a no-op — but it remains a safety net in case the
/// sanitiser's contract widens in the future.
fn truncate_error_for_peer(err: &AlmsError) -> String {
    let s = sanitize_error_for_session(err);
    if s.len() <= PEER_ERROR_MESSAGE_MAX_LEN {
        return s;
    }
    // Walk back from PEER_ERROR_MESSAGE_MAX_LEN to a char boundary.
    let mut end = PEER_ERROR_MESSAGE_MAX_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = s[..end].to_string();
    truncated.push_str("...");
    truncated
}

/// Broadcast a `run_queue_position` SSE event for every still-queued run on
/// the given agent (#831).
///
/// Called when the head of the per-agent queue is about to advance — i.e. at
/// every terminal exit of [`execute_run`]. For each remaining `Queued` run
/// (FIFO-sorted), assigns a 1-indexed position matching the same semantic as
/// `run_created.queued_behind` and fans the event out on both the per-run and
/// per-session SSE feeds.
///
/// Position numbering: if any `Running` run still exists for the agent, the
/// first queued run is position 1 (next up); otherwise the first queued run
/// is position 0 and is **skipped** — `run_started` will fire for it shortly.
/// Subsequent queued runs are numbered sequentially.
///
/// The broadcast also tolerates the narrow TOCTOU window between
/// `pending.fetch_sub(1)` inside the queue handler and `mark_run_as_running`:
/// in either ordering the FIFO-sorted Queued runs still produce monotonically
/// decremented positions matching what a fresh `create_run` would compute via
/// `pending_count + agent_has_running_run`.
async fn broadcast_queue_advance(state: &AppState, agent_id: AgentId) {
    let queued = state.run_manager.list_queued_for_agent(agent_id);
    if queued.is_empty() {
        return;
    }
    let running_offset = usize::from(state.run_manager.agent_has_running_run(agent_id));
    for (idx, run) in queued.iter().enumerate() {
        let position = idx + running_offset;
        if position == 0 {
            // First queued run is about to be picked up by the queue handler;
            // `run_started` will fire for it shortly, so we don't emit a
            // misleading position-zero event.
            continue;
        }
        state
            .run_manager
            .send_event(
                run.run_id,
                run.session_id,
                SseEventData::run_queue_position(run.run_id, run.session_id, agent_id, position),
            )
            .await;
    }
}

/// Execute a run in background, forwarding runtime events to SSE.
#[instrument(level = "info", skip(state, params), fields(run_id = %params.run_id.0, session_id = %params.session_id.0))]
pub(super) async fn execute_run(state: AppState, params: RunParams) {
    let RunParams {
        run_id,
        session_id,
        agent_id,
        input,
        context_id,
        cancel_token,
        is_peer_message,
        is_system_triggered,
        input_pre_persisted,
    } = params;
    // Track this run for graceful shutdown drain.  The guard ensures the
    // counter is decremented even if this function panics.
    state.run_manager.track_in_flight();
    let _in_flight_guard = InFlightGuard {
        run_manager: state.run_manager.clone(),
    };

    // Early exit if already cancelled (queued-then-cancelled before execution
    // started) or if the server is shutting down.  The shutdown_token check
    // prevents the SessionQueue drain from starting NEW runs during graceful
    // shutdown -- they would increment the in-flight counter and potentially
    // outlive the drain timeout.
    if cancel_token.is_cancelled() || state.shutdown_token.is_cancelled() {
        // #895: flip the run state BEFORE broadcasting the SSE event.
        // `RunManager::get_session_runs()` uses `RunState` membership to
        // compute `has_active_run` for `GET /sessions` (#890 / #892). If we
        // broadcast first and flip second, a concurrent client opening
        // `GET /sessions` between the broadcast and the state flip observes
        // `has_active_run: true` while the SSE feed has already moved past
        // the `ended` event — a subsequent `last_event_id`-based reconnect
        // won't replay it, and the sidebar's "active" indicator stays stuck
        // until the next unrelated event clears it. Flipping first makes
        // "a client that sees `has_active_run: true`" a strict prefix of
        // "a client that has the started event but not the ended event,"
        // closing the race. The change is internal to the gateway lock
        // window — the SSE event ordering visible to clients is identical.
        state.run_manager.mark_run_as_cancelled(run_id);
        state
            .run_manager
            .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
            .await;
        // Emit a synthetic `session_activity_ended` on the agent-scoped
        // feed (#888). The pre-cancel branch never emits a paired
        // `session_activity_started`, so this is asymmetric — but the run
        // was visible as `Queued` between insertion and cancellation, so
        // any concurrent `GET /sessions` snapshot will have observed
        // `has_active_run: true` and lit the sidebar's "active" indicator.
        // Without this `ended` event, that indicator stays stuck until the
        // page reloads. The consumer treats the snapshot as the source of
        // truth for "indicator on" and `ended` as the universal "indicator
        // off" signal, so the missing `started` is harmless: clients with
        // no indicator simply ignore the event.
        state
            .run_manager
            .send_agent_event(
                agent_id,
                run_id,
                session_id,
                SseEventData::session_activity_ended(session_id, run_id, agent_id),
            )
            .await;
        state.run_manager.remove_cancel_token(run_id);
        state.run_manager.remove_senders(run_id);
        info!("Run {} was cancelled before starting", run_id.0);
        // The queue head is advancing — fan out updated positions to any
        // remaining queued runs on this agent (#831).
        broadcast_queue_advance(&state, agent_id).await;
        return;
    }

    // Events are persisted to the event log regardless of whether an SSE
    // client is connected. The SSE client registers its own sender when it
    // calls GET /runs/{id}/events.

    // ---------------------------------------------------------------------
    // Resolve the layered run config UP FRONT (#837).
    //
    // The per-agent > server-default merge — including the bootstrap-prompt
    // swap and the system-triggered notification debug-flip — happens here,
    // **before** the `Queued` → `Running` transition fires. Two reasons:
    //
    // 1. Triage: the resolved snapshot is persisted alongside the
    //    `Running` state and broadcast on the `run_started` SSE so live
    //    observers and post-hoc `GET /runs/{id}` calls can confirm the
    //    provider / model / posture / budgets the run actually committed
    //    to. This makes "I set model X but Y was used" reports falsifiable
    //    from stored data alone (#833).
    // 2. Atomicity: marking the run `Running` and attaching its snapshot
    //    in a single `mark_run_as_running_with_config` call means SQLite
    //    never observes a torn state where status is `Running` but the
    //    snapshot is absent.
    //
    // Per-run config overrides were removed in the #941 pivot — the run
    // config is determined entirely by `resolve_agent_config` (per-agent
    // > server default). Operators change agent config via `PATCH
    // /agents/{id}` (or server defaults via `PATCH /settings`) before
    // starting the run; `POST /runs` carries no config knobs. Removing
    // the per-run path closes the leak family at #833 / #860 / #863 /
    // #939 by deleting the pathway that produced them.
    // ---------------------------------------------------------------------

    // Resolve per-agent config (model, posture, provider, reasoning
    // budgets, summary provider/model) from the agent registry on top of
    // the server default. The returned `LlmClient` already carries the
    // per-agent provider switch (with secrets re-resolved) and any
    // per-agent model override.
    //
    // `resolve_agent_config` can fail with #863 `MissingModelAfterProviderSwitch`
    // when a per-agent provider override is set with no model on any layer.
    // The HTTP `create_run` handler runs the same resolve up-front and
    // rejects with `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`, so this path
    // is only reachable for non-HTTP triggers (Telegram / scheduler / peer
    // DMs / subagent completions). Mark the run as failed with the same
    // structured message so the run record carries an actionable error.
    let base_agent_config = state.agent_config.read().clone();
    let resolve_outcome = {
        let secrets_guard = state.secrets.read();
        resolve_agent_config(
            agent_id,
            &state.session_manager,
            &base_agent_config,
            &state.llm,
            Some(&secrets_guard),
        )
    };
    let resolved = match resolve_outcome {
        Ok(resolved) => resolved,
        Err(e) => {
            error!("Run {} failed to resolve agent config: {}", run_id.0, e);
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &e.to_string()),
                )
                .await;
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;
            state.run_manager.mark_run_as_failed(run_id, e.to_string());
            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    };
    let agent_name = resolved.agent_name;
    if state.workspace_dir.is_some() && agent_name.is_none() {
        warn!(
            "Agent {} has no registry record — workspace and bootstrap skipped",
            agent_id.0
        );
    }

    let mut agent_config = resolved.agent_config;
    let llm = resolved.llm;

    // #919 per-run token-budget validation — non-HTTP path mirror of the
    // `create_run` pre-flight check above (Codex P2 follow-up on PR #1020).
    // The HTTP path validates at `POST /runs` time and returns a structured
    // 400 to the caller. Runs enqueued via `enqueue_triggered_run` (peer
    // DMs, scheduler triggers, notification runs, subagent completion runs)
    // skip that path entirely and land here. We also re-validate HTTP runs
    // that were created earlier and sat in the queue while `PATCH /settings`
    // or `PATCH /agents/{id}` mutated their effective budget — the
    // create-time pre-flight is no longer authoritative by the time
    // `execute_run` resolves the live snapshot, and without this second
    // check we would still reach the provider with an over-budget request
    // (the exact opaque-downstream-4xx symptom #919 is meant to prevent).
    //
    // On a strict-mode reject we emit a structured `run_error` SSE event
    // with the same `INVALID_TOKEN_BUDGET_FOR_PROVIDER` code the HTTP 400
    // body carries (clients branch on the same code regardless of which
    // surface delivered the failure), mark the run as `Failed` with the
    // human-readable message, and broadcast queue advance so any
    // queued-behind runs see their positions decrement. The `Queued` →
    // `Failed` transition fires here BEFORE `mark_run_as_running_with_config`,
    // so the run never enters the running set — same shape as the
    // `MissingModelAfterProviderSwitch` failure arm immediately above.
    if let Err(budget_err) = evaluate_pre_flight_token_budget(
        agent_id,
        &agent_config,
        &llm,
        "Failing queued run with INVALID_TOKEN_BUDGET_FOR_PROVIDER (#919)",
    ) {
        let message = budget_err.message();
        error!(
            "Run {} rejected by token-budget validator before LLM call: {}",
            run_id.0, message
        );
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_error_with_code(
                    run_id,
                    "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
                    &message,
                ),
            )
            .await;
        state
            .run_manager
            .send_agent_event(
                agent_id,
                run_id,
                session_id,
                SseEventData::session_activity_ended(session_id, run_id, agent_id),
            )
            .await;
        state.run_manager.mark_run_as_failed(run_id, message);
        state.run_manager.remove_senders(run_id);
        state.run_manager.remove_cancel_token(run_id);
        state.approval_store.clear_for_run(run_id);
        broadcast_queue_advance(&state, agent_id).await;
        return;
    }

    // System-triggered runs (peer DMs, notifications, subagent completions)
    // have no human in the loop, so Guarded posture would hang forever
    // waiting for approval.  Force Autonomous posture for these runs.
    let posture_resolved = resolve_posture_for_run(agent_config.posture, is_system_triggered);
    if posture_resolved != agent_config.posture {
        info!(
            "Run {} is system-triggered — overriding {:?} posture to {:?}",
            run_id.0, agent_config.posture, posture_resolved
        );
    }
    agent_config.posture = posture_resolved;

    // Override system prompt with bootstrap prompt for first-time agents.
    // Must come after per-agent overrides so bootstrap takes precedence.
    // Only mutates `system_prompt`; the layered fields the snapshot tracks
    // (provider/model/posture/budgets/debug) are unaffected.
    let agent_config =
        if let (Some(workspace_dir), Some(name)) = (&state.workspace_dir, &agent_name) {
            let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, name);
            if workspace.needs_bootstrap() {
                info!(
                    "Agent {} ({}) has no personality.md — using bootstrap prompt",
                    name, agent_id.0
                );
                let mut cfg = agent_config;
                cfg.system_prompt = alms_runtime::AgentWorkspace::bootstrap_prompt().to_string();
                cfg
            } else {
                agent_config
            }
        } else {
            agent_config
        };

    // Enable debug_mode for system-triggered notification runs that land on
    // a user-facing session.  The context_debug SSE event is ephemeral (not
    // persisted), so the cost is negligible — and it lets users inspect the
    // LLM context for notification runs without special client-side plumbing.
    // (#546 — debug_mode for notification runs)
    //
    // The flip happens before the snapshot is taken so the persisted
    // `resolved_config.debug_mode` reflects the value the runtime
    // actually uses, including this notification-flip — not just the
    // raw per-agent record value. (Per-run overrides for `debug_mode`
    // were removed in the #941 pivot.)
    let agent_config = if is_system_triggered
        && !is_peer_message
        && !is_internal_context_id(&context_id)
        && !agent_config.debug_mode
    {
        let mut cfg = agent_config;
        cfg.debug_mode = true;
        debug!(
            "Run {} is a notification on user-facing session — enabling debug_mode",
            run_id.0
        );
        cfg
    } else {
        agent_config
    };

    // Snapshot the fully-layered config now that all post-resolution
    // transforms have settled. Fed into both the persisted `Run` row and
    // the `run_started` SSE payload below (#837).
    let resolved_config = super::build_resolved_config(&agent_config, &llm);

    // #895: flip the run state BEFORE broadcasting `run_started`. See the
    // pre-cancel branch above for the full rationale — broadcasting first
    // leaves a narrow window where a concurrent `GET /sessions` observes
    // `has_active_run: false` (the run hasn't been added to the running
    // set yet) while the `started` event has already been delivered, so
    // the sidebar misses the activity. Flipping first guarantees that
    // any `has_active_run: true` observation is followed by an `ended`
    // event the client will see.
    //
    // Atomic-with-snapshot variant (#837): the layered config is stored
    // alongside the status flip in a single SQLite upsert.
    state
        .run_manager
        .mark_run_as_running_with_config(run_id, resolved_config.clone());

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started_with_config(run_id, session_id, resolved_config),
        )
        .await;

    // Mirror onto the agent-scoped session-activity feed (#856) so the
    // web UI sidebar can surface activity on sessions other than the
    // currently-viewed one. Covers both regular runs and DM runs because
    // every run goes through this point.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_started(session_id, run_id, agent_id),
        )
        .await;

    // Create a runtime event channel so we can forward tool events to SSE.
    // A second sender (`invoke_agent_tx`) is created for the InvokeAgentTool
    // so subagent events are forwarded into the same SSE stream.  It is moved
    // directly into the tool (not cloned) so no orphaned sender lingers in
    // this scope -- when the runtime drops its sender and the tool drops its
    // sender, the channel closes and the forwarder task completes.
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    let invoke_agent_fwd: std::sync::Arc<dyn alms_tools::EventForwarder> =
        std::sync::Arc::new(RuntimeEventForwarder::new(runtime_tx.clone()));

    // Capture summary config before agent_config and llm are consumed.
    // C1 fix: resolve the summary model *from the per-agent LLM client* so
    // that when `summary_model` is None we fall back to the agent's configured
    // model, not the server default.  After this line `llm` is consumed by
    // `AgentRuntime::new` and no longer available.
    //
    // #871 leak guard (mirror of #860/#861 on the summary path): when
    // `summary_provider` is configured AND `summary_model` is None, the
    // agent's model name belongs to the AGENT's provider, not the summary
    // provider. Falling back to it would leak the agent's model slug onto
    // the summary provider's wire and produce a confusing 404 (the symptom
    // #861 was meant to prevent, on the summary task this time). Pass `None`
    // through to `generate_and_persist_summary` instead so it falls back to
    // `llm_for_summary.default_model()` — which the leak guard below clears
    // to "" so the wire fails fast with a clean missing-model error.
    let run_summary_mode = agent_config.context_config.run_summary_mode.clone();
    let summary_max_tokens = agent_config.context_config.summary_max_tokens;
    let summary_provider_cfg = agent_config.context_config.summary_provider.clone();
    let summary_model_cfg = agent_config.context_config.summary_model.clone();
    let summary_model_resolved = if summary_provider_cfg.is_some() {
        // Dedicated summary provider: only use an explicit summary_model.
        // Never inherit the agent's resolved model — it belongs to a
        // different provider's namespace. None falls through to the
        // summary client's default_model in `generate_session_summary`,
        // which the leak guard below has already cleared to "".
        summary_model_cfg.clone()
    } else {
        // No provider switch: agent and summary share a wire, so the
        // agent's resolved model is a safe fallback (pre-#871 behaviour).
        summary_model_cfg
            .clone()
            .or_else(|| Some(llm.default_model().to_string()))
    };

    // S6 fix: clone the per-agent resolved LLM client *before* AgentRuntime::new
    // consumes it.  The summary task needs the agent's provider/base_url/api_key,
    // not the server-default `state.llm`.
    //
    // #866: when `summary_provider` is configured, re-target the summary
    // client at that provider via `with_provider_and_secrets`. This lets the
    // summary task hit a different provider than the agent (e.g. agent on
    // Anthropic, summary on OpenRouter). When `summary_provider` is None the
    // client is byte-identical to the agent's `llm`, preserving pre-#866
    // behaviour.
    //
    // The construction logic (including the #871 leak guard) lives in
    // `build_summary_client` so it can be unit-tested without spinning up a
    // full AppState. Here we just borrow `state.secrets` and forward it.
    let llm_for_summary = build_summary_client(
        &llm,
        summary_provider_cfg.as_deref(),
        summary_model_cfg.as_deref(),
        &state.secrets.read(),
        agent_id,
    );

    let mut runtime = match alms_runtime::AgentRuntime::new(agent_id, agent_config, llm) {
        Ok(rt) => rt,
        Err(e) => {
            error!("Run {} failed to create runtime: {}", run_id.0, e);
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &e.to_string()),
                )
                .await;
            // Pair the earlier `session_activity_started` so the agent feed
            // sees a clean started/ended cycle even when the runtime fails
            // to initialise (#856).
            state
                .run_manager
                .send_agent_event(
                    agent_id,
                    run_id,
                    session_id,
                    SseEventData::session_activity_ended(session_id, run_id, agent_id),
                )
                .await;
            state.run_manager.mark_run_as_failed(run_id, e.to_string());

            // Persist the runtime-construction failure so a follow-up
            // turn ("why did that fail?") gives the agent the error in
            // context (#874). Skipped for internal context IDs to mirror
            // the run-boundary marker policy.
            //
            // The error text is run through `sanitize_error_for_session`
            // before persistence so raw provider response bodies — which
            // can contain API keys, internal hostnames, or request URLs —
            // never reach session history or the LLM context on follow-up
            // turns (#911). The same sanitiser already guards the
            // runtime-layer fallback at `alms-runtime::agent::mod`.
            if !is_internal_context_id(&context_id) {
                let safe_reason = sanitize_error_for_session(&e);
                super::markers::persist_error_marker(
                    &state.session_manager,
                    session_id,
                    "runtime_init_error",
                    format!("(runtime initialization failed) {safe_reason}"),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "failed",
                        "error": safe_reason,
                        "error_kind": "runtime_init",
                    }),
                );
            }

            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
            // The queue head is advancing — fan out updated positions to any
            // remaining queued runs on this agent (#831).
            broadcast_queue_advance(&state, agent_id).await;
            return;
        }
    }
    .with_event_sender(runtime_tx)
    .with_run_id(run_id)
    .with_cancel_token(cancel_token.clone());

    // Set agent name for perspective mapping in DM sessions.
    if let Some(ref name) = agent_name {
        runtime = runtime.with_agent_name(name.clone());
    }

    // #866: when a separate summary provider is configured, wire the
    // re-targeted client into the runtime so the in-loop compact-strategy
    // path (`maybe_summarize`, formerly known as sliding-summary; renamed
    // in #869) hits the configured provider rather than the agent's. When
    // `summary_provider` is None we leave the runtime's `summary_llm` as
    // None — the summarizer transparently falls back to `self.llm`
    // (pre-#866 behaviour).
    if summary_provider_cfg.is_some() {
        runtime = runtime.with_summary_llm(llm_for_summary.clone());
    }

    // Inject ALMS_DATA_DIR and ALMS_WORKSPACE_DIR into shell_exec processes
    // so that CLI commands invoked by agents find the correct database and
    // workspace regardless of the sandboxed cwd.
    {
        let shell_env = alms_core::build_shell_default_env(
            Some(&state.data_dir),
            state.workspace_dir.as_deref(),
        );
        if !shell_env.is_empty() {
            runtime = runtime.with_shell_default_env(shell_env);
        }
    }

    // Wire the shell-output spill policy (issue #756) before `with_workspace`
    // so the per-run spill directory is included in the agent's
    // `fs_read`/`fs_list`/`fs_grep`/`fs_glob` extra_read_roots at run start.
    // The directory itself is created lazily on first spill. Reads use the
    // live `tools_config.shell_spill` so operators can tweak the TOML and
    // restart, but values are NOT PATCH-mutable (consistent with
    // shell_permissions).
    {
        let spill_cfg = state.tools_config.read().shell_spill.clone();
        let run_dir = state
            .data_dir
            .join(alms_runtime::spill::SPILL_DIR_NAME)
            .join(run_id.0.to_string());
        runtime = runtime.with_shell_spill(run_dir, spill_cfg.enabled);
    }

    // Wire the shared in-loop tool-output truncation policy (issue #851).
    // Mirrors `with_shell_spill` above — same lifecycle, same retention
    // model, same fs_* read-root widening, but applied to *every* tool's
    // output (not just shell). Must come before `with_project_root` so
    // the per-run spill dir is included in the agent's extra_read_roots.
    {
        let trunc_cfg = state.tools_config.read().tool_output_truncate.clone();
        let run_dir = state
            .data_dir
            .join(alms_runtime::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
            .join(run_id.0.to_string());
        runtime = runtime.with_tool_output_truncate(
            run_dir,
            trunc_cfg.enabled,
            trunc_cfg.max_bytes,
            trunc_cfg.max_lines,
        );
    }

    // Pin the agent's filesystem-sandbox boundary at the project root
    // (#945) — or, when `worktree_mode = "git"` (#946), at the agent's
    // dedicated worktree under `<project>/.alms/worktrees/<name>/`. After
    // this call `fs_*` and `shell` enforce against the same root and the
    // shell's persistent cwd defaults to that root. Must come AFTER the
    // spill / tool-output-truncate builders so the accumulated
    // `extra_fs_read_roots` are reflected in the read-family fs_* tool
    // registrations.
    //
    // Precedence (highest wins):
    // 1. `[security].allow_full_os_access` (#947) — when the agent's
    //    name is on the list, skip the sandbox pin entirely and call
    //    `with_unrestricted_filesystem`. Worktree mode is silently
    //    ignored at runtime per the documented precedence; the
    //    worktree itself stays on disk because the operator may flip
    //    the security knob off later.
    // 2. `worktree_mode = "git"` (#946) — pin the sandbox at the
    //    worktree path. Push `<project>/.alms/agents/` onto the
    //    extra-read-roots list FIRST so the parent agent can still
    //    `fs_read('.alms/agents/<sibling>/personality.md')` from
    //    outside its worktree (matches the issue's sibling-read
    //    acceptance criterion).
    // 3. Default — `with_project_root(state.project_root)` as #945
    //    intended.
    //
    // `shell_permissions` (#717) and the destructive-command
    // classifier (#745) apply at every level — they are independent
    // operator policy, not part of the sandbox.
    let full_os_access = agent_name
        .as_deref()
        .map(|n| state.security_config.is_full_os_access_agent(n))
        .unwrap_or(false);
    let resolved_worktree_mode = resolved.worktree_mode;
    if full_os_access {
        // Defensive `unwrap` is fine — `full_os_access` is only true when
        // `agent_name` is `Some`.
        let name = agent_name.as_deref().unwrap_or("");
        warn!(
            target: "alms.security",
            agent_name = %name,
            run_id = %run_id.0,
            allow_full_os_access = true,
            worktree_mode = %resolved_worktree_mode.as_wire_str(),
            "Run starting for agent '{}' WITHOUT project-root filesystem sandbox \
             (allow_full_os_access). shell_permissions and the destructive-command \
             classifier still apply. Worktree-mode is silently ignored at runtime \
             when the agent is on the security list.",
            name,
        );
        runtime = runtime.with_unrestricted_filesystem();
    } else if resolved_worktree_mode == alms_core::WorktreeMode::Git
        && let Some(ref name) = agent_name
    {
        // Worktree-mode-git path. The worktree was provisioned at
        // agent-create time (or on a `mode: off → git` PATCH); we
        // just compute the path again here and pin the sandbox at
        // it. If the directory has been removed externally,
        // `with_project_root` will fall back to the as-is path
        // with a WARN — the run continues with whatever fs sandbox
        // semantics the missing directory produces.
        let worktree_dir = alms_core::worktree::worktree_path(&state.project_root, name);
        // Sibling-read root: the project's `.alms/agents/` tree,
        // read-only. This lets a parent agent in worktree mode still
        // read sibling personality metadata from outside its
        // worktree. Scoped tightly — NOT the entire project root,
        // which would defeat the worktree's filesystem isolation.
        let sibling_read_root = state.project_root.join(".alms").join("agents");
        runtime = runtime
            .with_extra_fs_read_root(sibling_read_root)
            .with_project_root(worktree_dir.clone());
        info!(
            target: "alms.worktree",
            agent_name = %name,
            run_id = %run_id.0,
            worktree_dir = %worktree_dir.display(),
            "Run starting under per-agent git worktree (#946)"
        );
    } else {
        runtime = runtime.with_project_root(state.project_root.clone());
    }

    // Attach workspace if configured — registers the workspace_write tool for this run.
    // After #945 this no longer changes the sandbox root or default cwd —
    // it only ensures the agent's metadata directory exists and binds the
    // `workspace_write` tool. The metadata lives at
    // `<project_root>/.alms/agents/<name>/`, naturally inside the
    // project-root sandbox.
    if let (Some(workspace_dir), Some(name)) = (&state.workspace_dir, &agent_name) {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, name);
        runtime = runtime.with_workspace(workspace);
    }

    // Register invoke_agent tool.
    // Subagent events are forwarded into this run's SSE stream.
    // The cancel_token is passed to InvokeAgentTool so that cancelling the
    // parent run propagates to all subagents spawned during this run.
    {
        let dispatcher: std::sync::Arc<dyn alms_tools::SubagentDispatcher> =
            state.coordinator.clone();
        // Separate channel for background subagent events -> session stream.
        // This is independent of the parent's runtime_tx, so it doesn't
        // block the parent run from finishing.
        // Note: bg_run_id uses the parent's run_id. These events may arrive
        // after the parent run has finished. The frontend uses source_agent
        // (not run_id) for SubagentBar routing, so this is acceptable.
        let (bg_event_tx, bg_event_rx) = mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();
        let bg_state = state.clone();
        let bg_session_id = session_id;
        let bg_run_id = run_id;
        tokio::spawn(async move {
            let mut rx = bg_event_rx;
            while let Some(event) = rx.recv().await {
                let sse = match event {
                    alms_runtime::RuntimeEvent::ToolStart {
                        invocation_id,
                        tool,
                        params,
                        source_agent,
                        task_id,
                    } => SseEventData::tool_start(
                        bg_run_id,
                        crate::sse::ToolInvocationId(invocation_id),
                        &tool,
                        params,
                        source_agent,
                        task_id,
                    ),
                    alms_runtime::RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        source_agent,
                        task_id,
                    } => SseEventData::tool_end(
                        bg_run_id,
                        crate::sse::ToolInvocationId(invocation_id),
                        ok,
                        result,
                        source_agent,
                        task_id,
                    ),
                    alms_runtime::RuntimeEvent::ApprovalRequired {
                        tool, decision_tx, ..
                    } => {
                        warn!(
                            tool = %tool,
                            "Background subagent requested approval -- \
                             not supported, auto-denying"
                        );
                        let _ = decision_tx.send(false);
                        continue;
                    }
                    _ => continue,
                };
                bg_state
                    .run_manager
                    .send_session_event(bg_session_id, bg_run_id, sse)
                    .await;
            }
        });

        let bg_event_fwd: std::sync::Arc<dyn alms_tools::EventForwarder> =
            std::sync::Arc::new(RuntimeEventForwarder::new(bg_event_tx));
        let invoke_tool = alms_tools::InvokeAgentTool::new(
            dispatcher,
            session_id,
            Some(run_id),
            Some(invoke_agent_fwd),
        )
        .with_cancel_token(cancel_token)
        .with_background_event_fwd(bg_event_fwd);
        let read_session_tool =
            alms_tools::ReadSubagentSessionTool::new(state.session_manager.clone(), session_id);
        runtime.tools().register(std::sync::Arc::new(invoke_tool));
        runtime
            .tools()
            .register(std::sync::Arc::new(read_session_tool));
    }

    // Register read_session tool (on-demand session recall for the agent's own sessions).
    {
        let read_own_session_tool = alms_tools::ReadSessionTool::new(
            state.session_manager.clone(),
            agent_id,
            agent_name.clone(),
        );
        runtime
            .tools()
            .register(std::sync::Arc::new(read_own_session_tool));
    }

    // Register peer messaging tools (Layer 2) when agent name is known.
    if let Some(ref name) = agent_name {
        let sender: std::sync::Arc<dyn alms_tools::MessageSender> = state.message_bus.clone();
        let send_tool = alms_tools::SendMessageTool::new(
            sender,
            agent_id,
            name.clone(),
            state.session_manager.clone(),
            session_id,
        );
        let list_tool =
            alms_tools::ListAgentsTool::new(state.session_manager.clone(), name.clone());
        let read_tool =
            alms_tools::ReadMessagesTool::new(state.session_manager.clone(), name.clone());
        let list_sessions_tool = alms_tools::ListMySessionsTool::new(
            state.session_manager.clone(),
            agent_id,
            session_id,
            name.clone(),
        );
        runtime.tools().register(std::sync::Arc::new(send_tool));
        runtime.tools().register(std::sync::Arc::new(list_tool));
        runtime.tools().register(std::sync::Arc::new(read_tool));
        runtime
            .tools()
            .register(std::sync::Arc::new(list_sessions_tool));

        // Only register `ignore_message` in DM sessions -- the tool is
        // meaningless outside DM context and would confuse the LLM into
        // calling it in web-chat or job runs.  The runtime guard in
        // IgnoreMessageTool::execute() remains as defense-in-depth.
        if context_id.starts_with("dm:") {
            let ignore_tool = alms_tools::IgnoreMessageTool::new(context_id.clone());
            runtime.tools().register(std::sync::Arc::new(ignore_tool));
        }
    }

    // Spawn forwarder: converts RuntimeEvents -> SseEventData (and stores approvals).
    // We keep the handle so we can await it after the runtime finishes, ensuring
    // all tool events are flushed before we send run_finished.
    let forwarder_state = state.clone();

    // Build cross-session DM info when the run is on a DM session so that
    // status phases are echoed to the agent's webchat stream (#651, #688).
    let dm_peer_for_webchat = agent_name
        .as_deref()
        .and_then(|name| extract_peer_from_dm_context(&context_id, name));

    let dm_cross_session =
        dm_peer_for_webchat
            .as_deref()
            .map(|peer_name| super::tools::DmCrossSessionInfo {
                agent_id,
                peer_name: peer_name.to_string(),
            });

    let forwarder_handle = tokio::spawn(forward_runtime_events(
        runtime_rx,
        run_id,
        session_id,
        forwarder_state.run_manager.clone(),
        forwarder_state.approval_store.clone(),
        forwarder_state.session_manager.clone(),
        context_id.clone(),
        dm_cross_session,
    ));

    // Save input for episodic summary generation (input is consumed by run()).
    // S3 optimisation: only clone when summary mode is enabled AND the session
    // type is eligible (not a subagent or episodic session).
    // DM sessions are included — the agent_name is needed to derive the peer.
    let agent_name_for_summary = agent_name.clone().unwrap_or_default();
    let should_summarize = run_summary_mode != alms_core::config::RunSummaryMode::Off
        && alms_core::derive_source_label(&context_id, &agent_name_for_summary).is_some();

    let run_input_for_summary = if should_summarize {
        Some(input.clone())
    } else {
        None
    };

    // Capture a timestamp *before* the run starts so we can scope the DM
    // outbound-message lookup to only messages written during this run.
    // Without this, an `ignore_message` run would pick up a stale outbound
    // message from a prior run (#434, Bug 1).
    let run_start_ts = alms_core::Timestamp::now();

    // System-triggered notification runs (subagent completions, DM-ended)
    // that land on a user-facing session pre-persist the notification as
    // a `Role::User` message with `notification_input: true` metadata.
    // The `notification_input` flag drives `get_session_messages` filtering
    // so the internal prompt never shows as a "user" bubble on reload.
    //
    // The `Role::User` choice is now a consequence of the canonical
    // message-shape invariant enforced in `ContextBuilder::normalize_for_llm`
    // (see #586): the invariant guarantees a trailing user turn regardless
    // of this message's presence, so synthesising a placeholder would be
    // redundant. Persisting as user keeps the real payload in history.
    // Applies to Anthropic direct and OpenRouter-to-Claude proxying, which
    // performs the same system-message extraction as the Anthropic API.
    let is_notification_on_user_session =
        is_system_triggered && !is_peer_message && !is_internal_context_id(&context_id);

    let result = if is_peer_message {
        // Peer-triggered run: the input message is already in the shared
        // session (written by MessageBus with from_agent metadata).
        // Use run_on_session to look up the session by ID directly and
        // skip re-persisting the input (fixes C1 session split + C2 double-write).
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else if input_pre_persisted {
        // User-initiated run where `create_run` already pre-persisted the
        // input to the session (so it survives a page reload during the
        // queued-wait window). Use `run_on_session` to skip the default
        // Role::User persistence in `runtime.run()` and avoid duplicating
        // the message.
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else if is_notification_on_user_session {
        // Notification run on a user-facing session: pre-persist the
        // input as Role::User + `notification_input` metadata (see the
        // comment above `is_notification_on_user_session` for why), then
        // use `run_on_session` to skip `runtime.run()`'s default
        // Role::User persistence and avoid a double-write.
        let notif_msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "notification_input": true,
            })),
        };
        if let Err(e) = state.session_manager.append_message(session_id, notif_msg) {
            warn!("Failed to persist notification input: {e}");
        }
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else {
        // Pre-create the session with the trigger's session_id so that
        // `runtime.run()` -> `get_or_create(agent_id, context_id)` finds
        // the existing session instead of generating a new random UUID.
        //
        // Without this, system-triggered runs on internal sessions (e.g.
        // `notifications:{agent}`) hit a session ID mismatch: the Run
        // record carries the deterministic SessionId from the trigger, but
        // `get_or_create` inside `runtime.run()` creates a session with a
        // random UUID because no session with that `(agent_id, context_id)`
        // key exists yet.  Fixes #585.
        if is_system_triggered {
            state
                .session_manager
                .get_or_create_with_id(session_id, agent_id, &context_id);
        }
        runtime
            .run(&state.session_manager, &context_id, input)
            .await
    };

    // Drop `runtime` to close the last sender on `runtime_tx`.
    // The `invoke_agent_fwd` Arc was moved (not cloned) into `InvokeAgentTool`,
    // which lives inside the runtime's tool registry, so dropping the runtime
    // also drops the forwarder's sender.  Once all senders are gone the channel
    // closes and `forwarder_handle` completes.
    drop(runtime);
    forwarder_handle.await.ok();

    // Helper: persist tool call records (used by all outcome branches).
    let persist_tool_calls = |records: &[alms_core::ToolCallRecord]| {
        if !records.is_empty()
            && let Some(store) = state.session_manager.store()
            && let Err(e) = store.save_tool_calls(run_id, records)
        {
            warn!(
                "Failed to persist {} tool call records for run {}: {}",
                records.len(),
                run_id.0,
                e
            );
        }
    };

    match result {
        Ok(output) => {
            persist_tool_calls(&output.tool_calls);

            // Persist a run-boundary marker so page reloads show "(run
            // completed)" separators. Only for user-facing sessions to
            // avoid cluttering internal sessions (jobs, subagents, DMs,
            // notifications).
            //
            // Note: this writes to the session store, not to the SSE feed,
            // so its position relative to the state-flip / broadcast
            // ordering below is not load-bearing. Kept above
            // `mark_run_as_completed` for the same reason as before — it's
            // a synchronous SQLite write that sequences naturally before
            // both branches.
            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    "(run completed)".to_string(),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "completed",
                    }),
                );
            }

            // Compute the summary text BEFORE `mark_run_as_completed`
            // consumes `output.response` (#927 reorder preserves the
            // original consume-then-clone shape, just rolls it earlier).
            //
            // For DM runs the agent's actual outbound reply is sent via the
            // `send_message` tool and persisted to the shared DM session —
            // `output.response` is typically empty or a brief LLM
            // acknowledgment.  Read the last outbound message from the DM
            // session to capture the real content for episodic summaries
            // (#421).
            //
            // All DM messages are stored as `Role::User` with `from_agent`
            // metadata — perspective mapping to `Role::Assistant` only
            // happens at context assembly time.  So we match on
            // `from_agent` + `message_type == "dm"` instead of role.
            let run_output_for_summary = run_input_for_summary.as_ref().map(|_| {
                if context_id.starts_with("dm:")
                    && let Some(ref name) = agent_name
                    && let Some(last_own) =
                        state.session_manager.find_last_message(session_id, |m| {
                            // Scope to messages written *during this run* to avoid
                            // picking up stale outbound messages from prior runs
                            // (e.g. when ignore_message was called in the current
                            // run — #434 Bug 1).
                            m.timestamp.0 >= run_start_ts.0
                                && m.metadata.as_ref().is_some_and(|meta| {
                                    meta.get("from_agent").and_then(|v| v.as_str()) == Some(name)
                                        && meta.get("message_type").and_then(|v| v.as_str())
                                            == Some("dm")
                                })
                                && matches!(m.content, alms_session::Content::Text(_))
                        })
                    && let alms_session::Content::Text(ref text) = last_own.content
                    && !text.is_empty()
                {
                    return text.clone();
                }
                // Non-DM runs or fallback when no outbound message was found.
                output.response.clone()
            });

            // #927 (extends #895): flip the run state to `Completed`
            // BEFORE broadcasting `run_finished`. Pre-fix, a concurrent
            // `GET /sessions` snapshot taken between the broadcast and
            // the flip could observe `has_active_run: true` while the SSE
            // feed had already moved past the terminal event —
            // `last_event_id`-based reconnect would never replay it,
            // leaving the sidebar's activity indicator stuck.
            //
            // The usage payload is `Copy` (TokenUsage is a small struct
            // of u32/u64), so the broadcast can read it after the flip
            // consumes the original via `mark_run_as_completed`. We
            // capture it here for clarity and to avoid relying on a
            // post-move value.
            let usage_for_broadcast = output.usage;

            state
                .run_manager
                .mark_run_as_completed(run_id, output.response, output.usage);

            // token_delta events already emitted during streaming in the agent loop
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_finished(run_id, true, usage_for_broadcast),
                )
                .await;

            // Fire-and-forget episodic summary generation.
            // Runs in a separate task so it never blocks the SSE cleanup path.
            // S4: tracked by `in_flight` so graceful shutdown waits for it.
            if let (Some(run_input), Some(run_output)) =
                (run_input_for_summary, run_output_for_summary)
            {
                let sm = state.session_manager.clone();
                let llm_clone = llm_for_summary.clone();
                let ctx_id = context_id.clone();
                let run_mgr = state.run_manager.clone();
                let req = alms_runtime::episodic::PersistSummaryRequest {
                    mode: run_summary_mode.clone(),
                    agent_id,
                    session_id,
                    run_id,
                    run_input,
                    run_output,
                    context_id: ctx_id,
                    summary_model: summary_model_resolved.clone(),
                    agent_name: agent_name_for_summary.clone(),
                    summary_max_tokens,
                };
                run_mgr.track_in_flight();
                tokio::spawn(async move {
                    let _guard = InFlightGuard {
                        run_manager: run_mgr,
                    };
                    alms_runtime::episodic::generate_and_persist_summary(&sm, &llm_clone, req)
                        .await;
                });
            }

            // -- DM post-run lifecycle (consolidated in #628) --
            //
            // Detect ignore_message, signal conversation end, emit SSE events.
            // All logic lives in `dm_lifecycle::handle_dm_run_completion()`.
            super::dm_lifecycle::handle_dm_run_completion(
                super::dm_lifecycle::DmRunCompletionContext {
                    state: &state,
                    run_id,
                    session_id,
                    agent_id,
                    agent_name: agent_name.as_deref(),
                    context_id: &context_id,
                    is_peer_message,
                    tool_calls: &output.tool_calls,
                },
            )
            .await;

            info!("Run {} completed successfully", run_id.0);
        }
        Err(alms_core::AlmsError::Cancelled) => {
            // #895: flip the run state BEFORE broadcasting `run_cancelled`.
            // See the pre-cancel branch at the top of this function for the
            // full rationale; the same race applies to every started/ended
            // boundary that uses `mark_run_as_*` to drive `has_active_run`.
            state.run_manager.mark_run_as_cancelled(run_id);

            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;

            // Issue #912 — DO NOT persist a lifecycle-layer
            // `(run cancelled)` error marker here.  The runtime layer
            // (`alms_runtime::AgentRuntime::finish_run`) already wrote a
            // `[Run cancelled by user]` text message at
            // `Role::Assistant` (or `Role::User` with `from_agent`
            // metadata in DM sessions).  That runtime-layer record is
            // the canonical source of truth: it survives
            // `strip_mid_history_system_markers` natively and reaches
            // the next-turn LLM context as a regular conversation turn,
            // satisfying the #874 follow-up-visibility requirement.
            // Persisting a second `kind: "error"` marker here would
            // duplicate the same conceptual event in both chat history
            // and LLM context — see Atlas + Alper's decision recorded
            // on issue #912.

            // Best-effort: notify the DM peer that the conversation ended
            // because this run was cancelled mid-flight. Without this, the
            // peer thinks the DM is still open until the 1800s
            // `DEPTH_EXPIRY_SECS` sweep clears the depth counter.
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                ConversationEndReason::UserCancelled,
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            info!("Run {} cancelled", run_id.0);
        }
        Err(alms_core::AlmsError::CancelledWithToolCalls { tool_calls }) => {
            // Persist partial tool call records even though the run was cancelled.
            persist_tool_calls(&tool_calls);

            // #895: flip the run state BEFORE broadcasting `run_cancelled`.
            // See the pre-cancel branch at the top of this function for the
            // full rationale.
            state.run_manager.mark_run_as_cancelled(run_id);

            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;

            // Issue #912 — see the `Cancelled` arm above.  The
            // runtime-layer `[Run cancelled by user]` write is the
            // canonical record; no lifecycle-layer marker is persisted.

            // Best-effort: see comment in the `Cancelled` arm.
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                ConversationEndReason::UserCancelled,
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            info!(
                "Run {} cancelled ({} tool calls persisted)",
                run_id.0,
                tool_calls.len()
            );
        }
        Err(alms_core::AlmsError::FailedWithToolCalls { source, tool_calls }) => {
            // Persist partial tool call records even though the run failed.
            persist_tool_calls(&tool_calls);

            // #927 (extends #895): flip the run state to `Failed` BEFORE
            // broadcasting `run_error`. See the `Ok` arm above and the
            // pre-cancel branch at the top of this function for the full
            // rationale; the same race applies to every started/ended
            // boundary that uses `mark_run_as_*` to drive `has_active_run`.
            state
                .run_manager
                .mark_run_as_failed(run_id, source.to_string());

            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &source.to_string()),
                )
                .await;

            // Issue #912 — DO NOT persist a lifecycle-layer
            // `(run failed) ...` error marker here.  The runtime layer
            // (`alms_runtime::AgentRuntime::finish_run`) already wrote a
            // `[Run failed: <safe_reason>]` text message at
            // `Role::Assistant` (or `Role::User` with `from_agent`
            // metadata in DM sessions), already passed through
            // `sanitize_error_for_session` (#911).  That runtime-layer
            // record is the canonical source of truth: it survives
            // `strip_mid_history_system_markers` natively and reaches
            // the next-turn LLM context as a regular conversation turn,
            // satisfying the #874 follow-up-visibility requirement.
            // Persisting a second `kind: "error"` marker here would
            // duplicate the same conceptual event in both chat history
            // and LLM context — see Atlas + Alper's decision recorded
            // on issue #912.

            // Best-effort: notify the DM peer that the conversation ended
            // due to a runtime error. The truncated `source` string is
            // surfaced in the peer's `dm_ended` notification so the peer
            // (and human user watching the DM) sees a useful reason instead
            // of a stale "in-flight" indicator until the 1800s sweep.
            if let Err(e) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                ConversationEndReason::Errored {
                    message: truncate_error_for_peer(&source),
                },
            )
            .await
            {
                warn!(
                    error = %e,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            error!(
                "Run {} failed ({} tool calls persisted): {}",
                run_id.0,
                tool_calls.len(),
                source
            );
        }
        Err(e) => {
            // #927 (extends #895): flip the run state to `Failed` BEFORE
            // broadcasting `run_error`. See the `Ok` and
            // `FailedWithToolCalls` arms for the full rationale.
            state.run_manager.mark_run_as_failed(run_id, e.to_string());

            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &e.to_string()),
                )
                .await;

            // Issue #912 — see the `FailedWithToolCalls` arm above.
            // This generic-error arm covers LLM 4xx/5xx, rate-limit,
            // content-policy reject, and runtime errors (context budget
            // exceeded, summary generation failed) — all of which the
            // runtime layer has already persisted as a sanitised
            // `[Run failed: <safe_reason>]` text message via
            // `finish_run`'s `Err(e)` branch.  The lifecycle-layer
            // marker would be a duplicate.

            // Best-effort: see comment in the `FailedWithToolCalls` arm.
            if let Err(end_err) = super::dm_lifecycle::handle_dm_run_failure(
                &state,
                &run_id,
                &session_id,
                agent_id,
                agent_name.as_deref(),
                &context_id,
                is_peer_message,
                ConversationEndReason::Errored {
                    message: truncate_error_for_peer(&e),
                },
            )
            .await
            {
                warn!(
                    error = %end_err,
                    "handle_dm_run_failure emitted error — DM peer state may be stale"
                );
            }

            error!("Run {} failed: {}", run_id.0, e);
        }
    }

    // Mirror the run's terminal transition onto the agent-scoped
    // session-activity feed (#856) — paired with the
    // `session_activity_started` emitted before the run executed. Fires
    // exactly once per run regardless of which terminal arm the result
    // took (Ok / Cancelled / CancelledWithToolCalls / FailedWithToolCalls
    // / generic Err). Pre-execution cancellation and runtime-construction
    // failure paths handle their own emissions inline above and return
    // early, so this site is reached only when the run actually started.
    state
        .run_manager
        .send_agent_event(
            agent_id,
            run_id,
            session_id,
            SseEventData::session_activity_ended(session_id, run_id, agent_id),
        )
        .await;

    // Forward a `dm_activity_ended` event to the agent's webchat session
    // so the frontend can update the status bar (#688).  This is distinct
    // from `dm_conversation_ended` (which signals the entire DM conversation
    // is over) — `dm_activity_ended` signals that a single DM run finished,
    // allowing the frontend to keep "Chatting with..." visible if more DM
    // runs are expected.
    if let Some(ref peer_name) = dm_peer_for_webchat
        && let Some(target) = super::find_user_facing_session(&state.session_manager, agent_id)
    {
        let dummy_run_id = RunId::new();
        state
            .run_manager
            .send_session_event(
                target.id,
                dummy_run_id,
                SseEventData::dm_activity_ended(target.id, peer_name),
            )
            .await;
    }

    // Update last_active on the agent record (non-fatal).
    if let Some(store) = state.session_manager.store()
        && let Err(e) = store.touch_agent(agent_id)
    {
        warn!("Failed to update last_active for agent {}: {}", agent_id, e);
    }

    state.run_manager.remove_senders(run_id);
    // Defense-in-depth: sweep any other orphaned sender entries for runs
    // that reached terminal state (covers the TOCTOU window in #149).
    state.run_manager.purge_terminal_senders();
    state.run_manager.remove_cancel_token(run_id);
    // Clean up any stale pending approvals for this run
    state.approval_store.clear_for_run(run_id);
    // The queue head is advancing — fan out updated positions to any
    // remaining queued runs on this agent (#831). Emitted once per run
    // regardless of which terminal arm was taken (Ok / Cancelled /
    // CancelledWithToolCalls / FailedWithToolCalls / generic Err).
    broadcast_queue_advance(&state, agent_id).await;
    // `_in_flight_guard` dropped here — signals drain waiters that this run is done.
}

/// GET /runs/{run_id} - Get run status
pub async fn get_run_status(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<RunStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    match state.run_manager.get_run(run_id) {
        Some(run) => {
            let agent_id = run.agent_id;
            let is_queued = matches!(run.status, alms_core::RunStatus::Queued);
            let mut resp = RunStatusResponse::from(run);
            // Attach tool call count if SQLite is available.
            if let Some(store) = state.session_manager.store() {
                resp.tool_call_count = store.count_tool_calls(run_id).ok();
            }
            // Attach the live 1-indexed queue position so a late-joining
            // client (page reload, polling fallback) can render "Queued —
            // position N" without waiting for the next SSE decrement (#831).
            //
            // Position uses the same FIFO-rank-among-Queued + running-offset
            // formulation as the SSE `run_queue_position` broadcast, so the
            // two surfaces always agree. `None` is returned when the run is
            // running/terminal, or when the run somehow isn't in the queued
            // set (defensive: shouldn't happen for status == Queued but the
            // FIFO sort tolerates it gracefully).
            if is_queued {
                let queued = state.run_manager.list_queued_for_agent(agent_id);
                let running_offset = usize::from(state.run_manager.agent_has_running_run(agent_id));
                if let Some(idx) = queued.iter().position(|r| r.run_id == run_id) {
                    let pos = idx + running_offset;
                    if pos > 0 {
                        resp.queue_position = Some(pos);
                    }
                }
            }
            Ok(Json(resp))
        }
        None => Err(api_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Run not found",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, SessionId, job::JobId};

    /// Helper to create a basic run for testing.
    fn test_run() -> Run {
        Run::new(SessionId::new(), AgentId::new(), "test".into())
    }

    #[test]
    fn test_derive_trigger_user() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "web"), "user");
        assert_eq!(derive_trigger(&run, "default"), "user");
        assert_eq!(derive_trigger(&run, "my-context"), "user");
    }

    #[test]
    fn test_derive_trigger_scheduled() {
        let run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "job task".into(),
            JobId::new(),
        );
        assert_eq!(derive_trigger(&run, "job_abc"), "scheduled");
        // job_id takes priority even if context_id looks like DM
        assert_eq!(derive_trigger(&run, "dm:alice:bob"), "scheduled");
    }

    #[test]
    fn test_derive_trigger_subagent() {
        let run = Run::for_subagent(
            SessionId::new(),
            AgentId::new(),
            "subtask".into(),
            RunId::new(),
        );
        assert_eq!(derive_trigger(&run, "subagent_task_1"), "subagent");
        // parent_run_id takes priority over context_id prefix
        assert_eq!(derive_trigger(&run, "web"), "subagent");
    }

    #[test]
    fn test_derive_trigger_dm() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "dm:alice:bob"), "dm");
    }

    #[test]
    fn test_derive_trigger_notification() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "notifications:alice"), "notification");
    }

    #[test]
    fn test_derive_trigger_telegram() {
        let run = test_run();
        assert_eq!(derive_trigger(&run, "telegram_123"), "telegram");
    }

    #[test]
    fn test_derive_trigger_priority_job_over_context() {
        // job_id should win over parent_run_id and context_id
        let mut run = Run::for_job(
            SessionId::new(),
            AgentId::new(),
            "job+sub".into(),
            JobId::new(),
        );
        run.parent_run_id = Some(RunId::new());
        assert_eq!(derive_trigger(&run, "dm:a:b"), "scheduled");
    }

    #[test]
    fn test_derive_trigger_priority_subagent_over_context() {
        // parent_run_id should win over context_id prefix
        let run = Run::for_subagent(SessionId::new(), AgentId::new(), "sub".into(), RunId::new());
        assert_eq!(derive_trigger(&run, "dm:a:b"), "subagent");
    }

    #[test]
    fn test_truncate_error_for_peer_short_label_unchanged() {
        // The sanitiser collapses Runtime errors to short fixed category
        // labels well under PEER_ERROR_MESSAGE_MAX_LEN — the helper passes
        // them through with no truncation and no ellipsis.
        let err = AlmsError::Runtime("429 Too Many Requests".into());
        let out = truncate_error_for_peer(&err);
        assert_eq!(out, "LLM rate limit exceeded");
        assert!(!out.ends_with("..."));
        assert!(out.len() <= PEER_ERROR_MESSAGE_MAX_LEN);
    }

    #[test]
    fn test_truncate_error_for_peer_cancelled_passes_through() {
        // The Cancelled label is also bounded and passes through unchanged.
        let err = AlmsError::Cancelled;
        let out = truncate_error_for_peer(&err);
        assert_eq!(out, "Run cancelled by user");
    }

    /// Regression test for #931: a `Runtime` error whose Display string
    /// embeds an API key, hostname, header, or bearer token must NOT
    /// reach the peer agent's notification context. Before the fix,
    /// `truncate_error_for_peer` was a length-only UTF-8 truncator and
    /// the raw provider error would land verbatim in the peer's
    /// `dm_ended` notification body.
    ///
    /// Mirrors the `sanitize_runtime_auth_strips_url_and_keys` test in
    /// `alms-core/src/error.rs` but at the gateway-layer helper boundary
    /// — so a future change that swaps the sanitiser call out (or wires
    /// the helper to a non-sanitising path) is caught here.
    #[test]
    fn test_truncate_error_for_peer_strips_secrets_for_dm_peer() {
        let err = AlmsError::Runtime(
            "HTTP 401 Unauthorized at https://api.example.com (authorization: Bearer sk-test-12345)"
                .into(),
        );
        let out = truncate_error_for_peer(&err);
        // The sanitiser collapses 401/403 Runtime errors to a fixed label.
        assert_eq!(out, "LLM authentication error");
        for needle in [
            "sk-test-",
            "api.example.com",
            "Bearer",
            "authorization",
            "Unauthorized",
        ] {
            assert!(
                !out.contains(needle),
                "DM peer notification text must not contain {needle:?}, got {out:?}"
            );
        }
    }

    /// Regression test for #931: also exercise the
    /// `AlmsError::FailedWithToolCalls` shape used by one of the two
    /// production call sites — the inner `source` is what reaches
    /// `truncate_error_for_peer` after a `&Box<AlmsError>` deref-coerce.
    /// A raw 500-internal-error body containing leaked secrets must not
    /// survive sanitisation through that path either.
    #[test]
    fn test_truncate_error_for_peer_failed_with_tool_calls_path_strips_secrets() {
        let inner = AlmsError::Runtime(
            "provider 500 internal error: secret-key=sk-test-12345 leaked in body".into(),
        );
        // Mirror the production call site shape: `&source` where
        // `source: Box<AlmsError>` deref-coerces to `&AlmsError`.
        let source: Box<AlmsError> = Box::new(inner);
        let out = truncate_error_for_peer(&source);
        assert_eq!(out, "Runtime error");
        assert!(
            !out.contains("sk-test-12345"),
            "API key from inner Runtime body must not survive sanitisation: got {out:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Truncation + UTF-8 char-boundary walkback path (#931 / #936 follow-up)
    //
    // The sanitiser collapses every error variant *except* `ToolExecution`
    // to a short fixed-length category label, so in practice the truncation
    // step is a no-op for those variants. `ToolExecution` is the one
    // variant whose sanitised output length scales with caller input
    // (`format!("Tool execution failed: {}", msg.split(':').next())`), so
    // it is the only path through which the truncation + char-boundary
    // walkback can fire on production data. These tests pin that path so a
    // future "this code is dead, remove it" refactor either preserves the
    // behaviour or notices it has live callers.
    // ─────────────────────────────────────────────────────────────────────

    /// Pins the truncation arm on the only sanitiser output that can
    /// exceed `PEER_ERROR_MESSAGE_MAX_LEN`: a `ToolExecution` error whose
    /// pre-`:` segment is large enough that `"Tool execution failed: <name>"`
    /// overflows the cap. The truncated body must end with `"..."` and the
    /// total length must be exactly `PEER_ERROR_MESSAGE_MAX_LEN + 3`
    /// because the input is pure ASCII (no walkback required).
    #[test]
    fn test_truncate_error_for_peer_oversize_ascii_tool_truncates_with_ellipsis() {
        // 400 ASCII chars with no ':' means the sanitiser keeps the full
        // tool name, producing `"Tool execution failed: aaaa..."` of total
        // length 22 + 400 = 422 > PEER_ERROR_MESSAGE_MAX_LEN (300).
        let long_name = "a".repeat(400);
        let err = AlmsError::ToolExecution(long_name);
        let out = truncate_error_for_peer(&err);
        assert!(
            out.ends_with("..."),
            "oversize sanitiser output must be truncated and ellipsis-terminated, got {out:?}"
        );
        // Body length is exactly PEER_ERROR_MESSAGE_MAX_LEN for pure ASCII
        // (char-boundary walkback is a no-op); total = max + 3 ("...").
        assert_eq!(out.len(), PEER_ERROR_MESSAGE_MAX_LEN + 3);
        assert!(out.starts_with("Tool execution failed: "));
    }

    /// Pins the UTF-8 char-boundary walkback inside the truncation arm.
    /// Constructs a `ToolExecution` error whose sanitised output places a
    /// multibyte codepoint *across* byte offset `PEER_ERROR_MESSAGE_MAX_LEN`,
    /// so a naive `&s[..PEER_ERROR_MESSAGE_MAX_LEN]` would panic. The
    /// helper must walk back to the nearest char boundary at or below the
    /// cap and append `"..."`.
    ///
    /// Without the walkback, slicing `String` on a non-boundary panics —
    /// so this test would fail by panicking, not by producing wrong output.
    #[test]
    fn test_truncate_error_for_peer_multibyte_walks_back_to_char_boundary() {
        // The sanitiser prefix `"Tool execution failed: "` is 23 ASCII
        // bytes. Pad the start of the tool name so that the cap (300)
        // lands inside a 3-byte codepoint — concretely: 23 prefix bytes
        // + N filler ASCII bytes + a run of 3-byte CJK chars, with N
        // chosen so that 300 - (23 + N) is not a multiple of 3.
        //
        // Use N = 1 ("x"): prefix runs 23..24, then "あ" (0xE3 0x81 0x82,
        // 3 bytes each) starts at byte 24. Codepoint boundaries after
        // byte 24 are at 24 + 3k. 300 - 24 = 276, which IS a multiple
        // of 3, so 300 is on a boundary. Use N = 2 instead: 23 + 2 = 25,
        // boundaries at 25 + 3k, 300 - 25 = 275, not a multiple of 3.
        // Walkback must land at the nearest boundary <= 300, which is
        // 25 + 3*91 = 298.
        let mut tool_name = "xx".to_string();
        tool_name.push_str(&"あ".repeat(200)); // 600 bytes; total >> 300
        let err = AlmsError::ToolExecution(tool_name);
        let out = truncate_error_for_peer(&err);
        assert!(out.ends_with("..."));
        let body = out.trim_end_matches("...");
        // Body must end at a UTF-8 char boundary (otherwise `&s[..end]`
        // panics inside the helper, never reaching this assertion).
        assert!(
            body.is_char_boundary(body.len()),
            "truncated body must end on a UTF-8 char boundary"
        );
        assert!(body.len() <= PEER_ERROR_MESSAGE_MAX_LEN);
        // The walkback should land ON the largest valid boundary <= 300,
        // not arbitrarily earlier. With our padding, that boundary is 298.
        assert_eq!(body.len(), 298);
    }

    // ─────────────────────────────────────────────────────────────────────
    // build_summary_client (#866 + #871) — mirror of #860/#861 leak-guard
    // tests in `runs/mod.rs::tests` but on the summary path.
    // ─────────────────────────────────────────────────────────────────────

    /// Build an `LlmConfig` whose `[llm.providers.<name>]` map contains
    /// (anthropic, openrouter) and an in-memory `SecretsStore` with keys
    /// for both. Mirrors the fixture shape used by the #860/#861 tests in
    /// `runs/mod.rs::tests`.
    fn summary_test_fixtures(
        anthropic_entry_model: Option<&str>,
        openrouter_entry_model: Option<&str>,
    ) -> (alms_runtime::LlmClient, alms_core::secrets::SecretsStore) {
        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: openrouter_entry_model.map(str::to_string),
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: anthropic_entry_model.map(str::to_string),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let cfg = LlmConfig {
            // Agent's resolved client is on Anthropic with a sonnet model.
            provider: "anthropic".into(),
            api_key: "sk-ant-runtime".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            default_model: "claude-sonnet-4-20250514".into(),
            providers,
            ..LlmConfig::default()
        };
        let llm = alms_runtime::LlmClient::new(cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        secrets.set_key("openrouter", "sk-or-runtime").unwrap();
        std::mem::forget(dir);
        (llm, secrets)
    }

    /// Pre-#866 behaviour: when `summary_provider` is `None`, the helper
    /// returns a clone of the agent's `llm` byte-identical to pre-#866
    /// behaviour. Provider, default_model, and base_url all match.
    #[test]
    fn build_summary_client_no_provider_returns_agent_llm_clone() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(&llm, None, None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), llm.provider());
        assert_eq!(summary.default_model(), llm.default_model());
    }

    /// #866 happy path: provider AND model both set. Both reach the wire
    /// regardless of any provider-entry model field.
    #[test]
    fn build_summary_client_provider_and_model_set_both_apply() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("minimax/minimax-m2.7"),
            &secrets,
            AgentId::new(),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "minimax/minimax-m2.7");
    }

    /// #866 + #871: provider set, no model, AND the new provider entry has
    /// its own `model` field. `apply_provider` rewrites `default_model` to
    /// the entry's model — that's the (provider, model) pair the user
    /// configured, so the leak guard does NOT clear it. This shape is
    /// rejected by the PATCH validator (`SUMMARY_PROVIDER_REQUIRES_MODEL`
    /// fires when `summary_model` is None) but a hand-edited TOML can still
    /// reach this branch — keep the property pinned.
    #[test]
    fn build_summary_client_provider_only_with_entry_model_uses_entry_model() {
        let (llm, secrets) = summary_test_fixtures(None, Some("provider-default-model"));
        let summary =
            build_summary_client(&llm, Some("openrouter"), None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(
            summary.default_model(),
            "provider-default-model",
            "provider-entry model should reach the wire when no \
             summary_model is configured (#866)"
        );
    }

    /// #871 leak guard (the bug Tim flagged): provider set, no model, AND
    /// the new provider entry has NO `model` field. `apply_provider` leaves
    /// `default_model` unchanged, so the cloned client carries the AGENT's
    /// model slug ("claude-sonnet-4-...") into the new (openrouter) wire.
    /// Pre-fix: the agent's slug reaches openrouter and produces a 404 (the
    /// exact symptom #861 was meant to prevent on the agent path). Post-fix:
    /// the leak guard clears `default_model` so the wire request fails fast
    /// with a missing-model error rather than 404'ing on the wrong slug.
    #[test]
    fn build_summary_client_provider_only_no_entry_model_clears_to_fail_fast() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let agent_model = llm.default_model().to_string();
        let summary =
            build_summary_client(&llm, Some("openrouter"), None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "openrouter");
        assert_ne!(
            summary.default_model(),
            agent_model,
            "summary_provider switch must NOT leak the agent's model slug \
             onto the summary provider's wire (#871, mirror of #860/#861)"
        );
        assert_eq!(
            summary.default_model(),
            "",
            "leak guard clears default_model when summary_provider is set, \
             summary_model is None, and the provider entry has no model \
             field — wire fails fast with a missing-model error (#871)"
        );
    }

    /// #866 + #871: provider set, model set, and the new provider entry
    /// also has its own `model` field. The explicit `summary_model`
    /// override must win — same property as the per-agent #861 test
    /// (`test_per_agent_provider_with_entry_model_does_not_drop_per_agent_model`).
    #[test]
    fn build_summary_client_explicit_model_wins_over_entry_model() {
        let (llm, secrets) = summary_test_fixtures(None, Some("provider-default-model"));
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("user-chosen-summary-model"),
            &secrets,
            AgentId::new(),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(
            summary.default_model(),
            "user-chosen-summary-model",
            "explicit summary_model must reach the wire even when the \
             provider entry has its own model field"
        );
    }

    /// Back-compat: when both fields are unset and the agent has a normal
    /// resolved client, the helper short-circuits to a clone with the same
    /// provider+model+base_url. This is the byte-identical-to-pre-#866
    /// guarantee.
    #[test]
    fn build_summary_client_back_compat_when_provider_and_model_both_none() {
        let (llm, secrets) = summary_test_fixtures(None, None);
        let summary = build_summary_client(&llm, None, None, &secrets, AgentId::new());
        assert_eq!(summary.provider(), "anthropic");
        assert_eq!(summary.default_model(), "claude-sonnet-4-20250514");
    }
}
