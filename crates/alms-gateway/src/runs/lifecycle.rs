//! Run creation, execution, and completion — the core run lifecycle.

use super::tools::{RuntimeEventForwarder, forward_runtime_events};
use super::{
    RunOverrides, RunParams, apply_overrides, is_internal_context_id, resolve_agent_config,
    validate_provider,
};
use crate::api_error;
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_core::{
    AgentId, CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus,
    RunStatusResponse, SessionId, classify_session_type,
};
use alms_runtime::RuntimeEvent;
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

    // Validate provider override early so the user gets a clear 400 instead
    // of a confusing "invalid API key" error from a wrong provider.
    if let Some(ref p) = req.provider {
        validate_provider(p)?;
    }

    let overrides = RunOverrides {
        model: req.model.clone(),
        max_tokens: req.max_tokens,
        posture: req.posture.clone(),
        provider: req.provider.clone(),
        debug_mode: req.debug_mode,
    };
    let run = Run::new(session.id, session.agent_id, input_text);
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

    state.run_manager.insert_run(run.clone());

    // Notify session-level SSE subscribers that a new run was created.
    // Check how many items are already queued for this agent so the UI
    // can show "queued (N ahead)" instead of a misleading "Thinking...".
    let queued_behind = state.agent_queue.pending_count(&agent_id);
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
                    overrides,
                    context_id,
                    cancel_token,
                    is_peer_message: false,
                    is_system_triggered: false,
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

/// Execute a run in background, forwarding runtime events to SSE.
#[instrument(level = "info", skip(state, params), fields(run_id = %params.run_id.0, session_id = %params.session_id.0))]
pub(super) async fn execute_run(state: AppState, params: RunParams) {
    let RunParams {
        run_id,
        session_id,
        agent_id,
        input,
        overrides,
        context_id,
        cancel_token,
        is_peer_message,
        is_system_triggered,
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
        state
            .run_manager
            .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
            .await;
        state.run_manager.mark_run_as_cancelled(run_id);
        state.run_manager.remove_cancel_token(run_id);
        state.run_manager.remove_senders(run_id);
        info!("Run {} was cancelled before starting", run_id.0);
        return;
    }

    // Events are persisted to the event log regardless of whether an SSE
    // client is connected. The SSE client registers its own sender when it
    // calls GET /runs/{id}/events.

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started(run_id, session_id),
        )
        .await;

    state.run_manager.mark_run_as_running(run_id);

    // Resolve per-agent config (model, posture) from the agent registry,
    // then layer per-run overrides on top.
    let base_agent_config = state.agent_config.read().clone();
    let resolved = resolve_agent_config(
        agent_id,
        &state.session_manager,
        &base_agent_config,
        &state.llm,
        Some(&state.secrets.read()),
    );
    let agent_name = resolved.agent_name;
    if state.workspace_dir.is_some() && agent_name.is_none() {
        warn!(
            "Agent {} has no registry record — workspace and bootstrap skipped",
            agent_id.0
        );
    }

    // Apply per-run overrides (highest precedence) on top of the resolved config.
    // Uses `apply_overrides()` for the common fields (max_tokens, posture,
    // debug_mode) to avoid duplicating the logic that lives in runs/mod.rs.
    // Provider and model require LLM client mutations which `apply_overrides()`
    // does not handle, so those stay inline.
    let merged = apply_overrides(resolved.agent_config, None, &overrides);
    let mut agent_config = merged.agent_config;
    let mut llm = resolved.llm;
    if let Some(ref provider) = overrides.provider {
        info!("Run {} using provider override: {}", run_id.0, provider);
        llm = llm.with_provider_and_secrets(provider, &state.secrets.read());
    }
    if let Some(model) = merged.model_override {
        info!("Run {} using model override: {}", run_id.0, model);
        llm = llm.with_model(model);
    }

    // System-triggered runs (peer DMs, notifications, subagent completions)
    // have no human in the loop, so Guarded posture would hang forever
    // waiting for approval.  Force Autonomous posture for these runs.
    let resolved = resolve_posture_for_run(agent_config.posture, is_system_triggered);
    if resolved != agent_config.posture {
        info!(
            "Run {} is system-triggered — overriding {:?} posture to {:?}",
            run_id.0, agent_config.posture, resolved
        );
    }
    agent_config.posture = resolved;

    // Create a runtime event channel so we can forward tool events to SSE.
    // A second sender (`invoke_agent_tx`) is created for the InvokeAgentTool
    // so subagent events are forwarded into the same SSE stream.  It is moved
    // directly into the tool (not cloned) so no orphaned sender lingers in
    // this scope -- when the runtime drops its sender and the tool drops its
    // sender, the channel closes and the forwarder task completes.
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    let invoke_agent_fwd: std::sync::Arc<dyn alms_tools::EventForwarder> =
        std::sync::Arc::new(RuntimeEventForwarder::new(runtime_tx.clone()));

    // Override system prompt with bootstrap prompt for first-time agents.
    // Must come after per-agent overrides so bootstrap takes precedence.
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

    // Capture summary config before agent_config and llm are consumed.
    // C1 fix: resolve the summary model *from the per-agent LLM client* so
    // that when `summary_model` is None we fall back to the agent's configured
    // model, not the server default.  After this line `llm` is consumed by
    // `AgentRuntime::new` and no longer available.
    let run_summary_mode = agent_config.context_config.run_summary_mode.clone();
    let summary_max_tokens = agent_config.context_config.summary_max_tokens;
    let summary_model_resolved = agent_config
        .context_config
        .summary_model
        .clone()
        .or_else(|| Some(llm.default_model().to_string()));

    // S6 fix: clone the per-agent resolved LLM client *before* AgentRuntime::new
    // consumes it.  The summary task needs the agent's provider/base_url/api_key,
    // not the server-default `state.llm`.
    let llm_for_summary = llm.clone();

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
            state.run_manager.mark_run_as_failed(run_id, e.to_string());
            state.run_manager.remove_senders(run_id);
            state.run_manager.remove_cancel_token(run_id);
            state.approval_store.clear_for_run(run_id);
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

    // Attach workspace if configured — registers the workspace_write tool for this run
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

    // System-triggered notification runs (subagent completions, DM-ended
    // notifications) that land on a user-facing session should NOT persist
    // the verbose notification input as a normal Role::User message — that
    // would show the internal LLM prompt as a "user" bubble on page reload.
    //
    // Instead, pre-persist the input as a Role::User message with
    // `notification_input: true` metadata. This ensures:
    //
    //  1. The context builder includes it as a **user** message in the LLM
    //     context window. This is required across all providers:
    //
    //     - **Anthropic (direct)**: The Anthropic Messages API extracts
    //       system messages into the top-level `system` field. With the
    //       previous Role::System approach, the messages array ended with
    //       an assistant message from the prior conversation, causing API
    //       rejection. Role::User ensures a valid trailing user turn.
    //
    //     - **OpenRouter (all models)**: OpenRouter uses the OpenAI chat
    //       completions format. While the API technically accepts a
    //       trailing system message, models are not trained to generate
    //       responses to system messages without a subsequent user turn.
    //       When proxying to Claude, OpenRouter performs the same system
    //       message extraction as the direct Anthropic API, causing the
    //       identical failure. For non-Claude models, a trailing system
    //       message produces empty or confused responses. Role::User
    //       ensures the notification is a clear conversation turn that
    //       all models respond to naturally.
    //
    //  2. `get_session_messages` filters it out via the
    //     `notification_input` metadata flag, making it invisible on page
    //     reload.
    //
    // Then use `run_on_session` to skip the default Role::User persistence
    // in `runtime.run()`.
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
    } else if is_notification_on_user_session {
        // Notification run landing on a user-facing session.
        //
        // Pre-persist the input as Role::User with `notification_input`
        // metadata so the context builder sees it as a user message.
        // This is required for all providers — see the detailed comment
        // above `is_notification_on_user_session` for the full rationale.
        //
        // `get_session_messages` filters it out on reload so the internal
        // prompt never appears as a "user" bubble.
        //
        // Then use `run_on_session` to skip the default Role::User
        // persistence in `runtime.run()`.
        //
        // Note: if `run_on_session` fails after the append, the orphaned
        // message remains in the session. It is invisible on reload (the
        // API filter hides it) but occupies tokens in the context window
        // for future runs. This is acceptable — the failure case is rare
        // and the impact is minor token waste.
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

            // Capture this flag before mark_run_as_completed moves the response.
            let hit_max_iterations = output.response == alms_core::MAX_ITERATIONS_SENTINEL;

            // Detect max-iterations sentinel and emit a warning event so the
            // frontend can style it distinctly (yellow) instead of as a normal
            // agent message.
            if hit_max_iterations {
                state
                    .run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_warning(
                            run_id,
                            "MAX_ITERATIONS",
                            "Max iterations reached — the agent hit its iteration limit before finishing. You can continue the conversation to pick up where it left off.",
                            None,
                        ),
                    )
                    .await;
            }

            // token_delta events already emitted during streaming in the agent loop
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_finished(run_id, true, output.usage),
                )
                .await;

            // Persist a run-boundary marker so page reloads show "(run
            // completed)" separators. Only for user-facing sessions to
            // avoid cluttering internal sessions (jobs, subagents, DMs,
            // notifications).
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

            // Persist warning marker when MAX_ITERATIONS was hit, so it
            // survives page reloads.
            if hit_max_iterations && !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_warning",
                    "Max iterations reached — the agent hit its iteration limit before finishing."
                        .to_string(),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "code": "MAX_ITERATIONS",
                    }),
                );
            }

            // Clone the response before mark_run_as_completed consumes it.
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

            state
                .run_manager
                .mark_run_as_completed(run_id, output.response, output.usage);

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
            // Detect ignore_message or max-iterations, signal conversation
            // end, emit SSE events.
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
                    hit_max_iterations,
                },
            )
            .await;

            info!("Run {} completed successfully", run_id.0);
        }
        Err(alms_core::AlmsError::Cancelled) => {
            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;

            state.run_manager.mark_run_as_cancelled(run_id);

            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    "(run cancelled)".to_string(),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "cancelled",
                    }),
                );
            }

            info!("Run {} cancelled", run_id.0);
        }
        Err(alms_core::AlmsError::CancelledWithToolCalls { tool_calls }) => {
            // Persist partial tool call records even though the run was cancelled.
            persist_tool_calls(&tool_calls);

            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;

            state.run_manager.mark_run_as_cancelled(run_id);

            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    "(run cancelled)".to_string(),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "cancelled",
                    }),
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

            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &source.to_string()),
                )
                .await;

            state
                .run_manager
                .mark_run_as_failed(run_id, source.to_string());

            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    format!("(run failed) {source}"),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "failed",
                        "error": source.to_string(),
                    }),
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
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_error(run_id, &e.to_string()),
                )
                .await;

            state.run_manager.mark_run_as_failed(run_id, e.to_string());

            if !is_internal_context_id(&context_id) {
                super::markers::persist_lifecycle_marker(
                    &state.session_manager,
                    session_id,
                    "run_boundary",
                    format!("(run failed) {e}"),
                    serde_json::json!({
                        "run_id": run_id.0.to_string(),
                        "status": "failed",
                        "error": e.to_string(),
                    }),
                );
            }

            error!("Run {} failed: {}", run_id.0, e);
        }
    }

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
    // `_in_flight_guard` dropped here — signals drain waiters that this run is done.
}

/// GET /runs/{run_id} - Get run status
pub async fn get_run_status(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<RunStatusResponse>, (StatusCode, Json<serde_json::Value>)> {
    match state.run_manager.get_run(run_id) {
        Some(run) => {
            let mut resp = RunStatusResponse::from(run);
            // Attach tool call count if SQLite is available.
            if let Some(store) = state.session_manager.store() {
                resp.tool_call_count = store.count_tool_calls(run_id).ok();
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
}
