//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md

use crate::api_error;
use crate::approvals::{ApprovalStore, PendingApproval};
use crate::cron_utils;
use crate::server::AppState;
use crate::sse::{RunEventStream, SseEventData, ToolInvocationId, event_channel};
use alms_core::{
    CreateRunRequest, CreateRunResponse, JobId, JobSchedule, JobStatus, MAX_ITERATIONS_SENTINEL,
    Run, RunId, RunInput, RunStatus, RunStatusResponse, SessionId,
};
use alms_runtime::RuntimeEvent;
use alms_tools::message_sender::{ConversationEndReason, MessageSender as _};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

// ---------------------------------------------------------------------------
// RuntimeEventForwarder -- bridges alms-tools EventForwarder to RuntimeEvent
// ---------------------------------------------------------------------------

/// Concrete [`EventForwarder`](alms_tools::EventForwarder) that wraps a
/// `RuntimeEventSender` and maps each method to the corresponding
/// `RuntimeEvent` variant.
///
/// This is the bridge that lets tools in `alms-tools` emit events without
/// depending on `alms-runtime`'s `RuntimeEvent` enum.
#[derive(Debug, Clone)]
struct RuntimeEventForwarder {
    tx: alms_runtime::RuntimeEventSender,
}

impl RuntimeEventForwarder {
    fn new(tx: alms_runtime::RuntimeEventSender) -> Self {
        Self { tx }
    }
}

impl alms_tools::EventForwarder for RuntimeEventForwarder {
    fn forward_tool_start(
        &self,
        invocation_id: uuid::Uuid,
        tool: String,
        params: serde_json::Value,
        source_agent: Option<String>,
    ) {
        let _ = self.tx.send(RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            params,
            source_agent,
        });
    }

    fn forward_tool_end(
        &self,
        invocation_id: uuid::Uuid,
        ok: bool,
        result: serde_json::Value,
        source_agent: Option<String>,
    ) {
        let _ = self.tx.send(RuntimeEvent::ToolEnd {
            invocation_id,
            ok,
            result,
            source_agent,
        });
    }

    fn forward_token_delta(&self, delta: String, source_agent: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::TokenDelta {
            delta,
            source_agent,
        });
    }

    fn forward_status(&self, phase: String, detail: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::Status { phase, detail });
    }

    fn forward_warning(&self, code: String, message: String, source_agent: Option<String>) {
        let _ = self.tx.send(RuntimeEvent::Warning {
            code,
            message,
            source_agent,
        });
    }
}

/// Valid LLM provider identifiers accepted in per-run overrides.
///
/// This is intentionally separate from `alms_core::secrets::VALID_PROVIDERS`
/// which also includes non-LLM keys like `"telegram"`.
const VALID_LLM_PROVIDERS: &[&str] = &["openai", "anthropic", "openrouter"];

/// Validate that a provider string is a known LLM provider.
///
/// Returns `Ok(())` if valid, or an API error tuple suitable for returning
/// from an Axum handler if the provider is unrecognised.
fn validate_provider(provider: &str) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if VALID_LLM_PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::BAD_REQUEST,
            "INVALID_PROVIDER",
            format!(
                "Unknown provider '{}'. Valid providers: {}",
                provider,
                VALID_LLM_PROVIDERS.join(", ")
            ),
        ))
    }
}

/// Per-run overrides that can be sent by the client to customise a single run.
#[derive(Debug, Default)]
struct RunOverrides {
    model: Option<String>,
    max_tokens: Option<u32>,
    posture: Option<String>,
    provider: Option<String>,
}

/// Bundled parameters for [`execute_run`], avoiding a long positional argument list.
struct RunParams {
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
    overrides: RunOverrides,
    context_id: String,
    cancel_token: CancellationToken,
    /// When true, the input message has already been persisted to the session
    /// by the MessageBus. The agent loop uses `run_on_session` to look up the
    /// shared session by `SessionId` directly and skips re-persisting the input.
    is_peer_message: bool,
    /// When true, the run was created by the system (not a user-initiated HTTP
    /// request). All runs from `enqueue_triggered_run` are system-triggered:
    /// peer DM messages, notification runs, subagent completions. These runs
    /// have no human watching, so Guarded posture would hang forever waiting
    /// for approval -- the posture is overridden to Autonomous.
    is_system_triggered: bool,
}

/// Result of resolving per-agent config from the agent registry.
pub struct ResolvedAgentConfig {
    pub agent_config: alms_runtime::AgentConfig,
    pub llm: alms_runtime::LlmClient,
    /// Agent name from registry (None if record not found).
    pub agent_name: Option<String>,
}

/// Resolve per-agent config overrides from the agent registry.
///
/// Looks up the agent record by ID, applies model/posture overrides on top
/// of the base config. Returns the merged config, LLM client with model
/// override, and agent name for workspace resolution.
/// No per-run overrides are applied — callers layer those on top.
pub fn resolve_agent_config(
    agent_id: alms_core::AgentId,
    session_manager: &alms_session::SessionManager,
    base_config: &alms_runtime::AgentConfig,
    llm: &alms_runtime::LlmClient,
    secrets: Option<&alms_core::secrets::SecretsStore>,
) -> ResolvedAgentConfig {
    let agent_record =
        session_manager
            .store()
            .and_then(|store| match store.load_agent_by_id(agent_id) {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!(
                        "Failed to load agent record for {}, using server defaults: {}",
                        agent_id,
                        e
                    );
                    None
                }
            });

    let agent_name = agent_record.as_ref().map(|r| r.name.clone());

    let merged = apply_overrides(
        base_config.clone(),
        agent_record.as_ref(),
        &RunOverrides::default(),
    );

    // Apply per-agent provider override first (changes base_url + api_key),
    // then ALWAYS re-resolve the API key from secrets for the effective
    // provider. This ensures keys set at runtime (via UI or CLI) are picked
    // up even for the default agent which has no per-agent provider field.
    let mut llm = llm.clone();
    if let Some(ref record) = agent_record
        && let Some(ref provider) = record.provider
    {
        info!(
            agent_id = %agent_id,
            provider = %provider,
            "Applying per-agent provider override with secrets resolution"
        );
        llm = if let Some(s) = secrets {
            llm.with_provider_and_secrets(provider, s)
        } else {
            warn!(
                agent_id = %agent_id,
                provider = %provider,
                "No secrets store available for per-agent provider override — API key may be missing"
            );
            llm.with_provider(provider)
        };
    } else if let Some(s) = secrets {
        // No per-agent provider override — re-resolve the key for the
        // server-default provider from the live secrets store.
        info!(
            agent_id = %agent_id,
            provider = %llm.provider(),
            "Re-resolving API key from secrets for default provider"
        );
        llm = llm.with_secrets(s);
    } else {
        warn!(
            agent_id = %agent_id,
            "No secrets store and no per-agent provider — using base LLM client key as-is"
        );
    }
    if let Some(model) = merged.model_override {
        llm = llm.with_model(model);
    }

    ResolvedAgentConfig {
        agent_config: merged.agent_config,
        llm,
        agent_name,
    }
}

/// Result of merging server defaults + per-agent + per-run overrides.
struct MergedConfig {
    agent_config: alms_runtime::AgentConfig,
    /// If set, override the LLM client's default model.
    model_override: Option<String>,
}

/// Pure config merging: server defaults → per-agent overrides → per-run overrides.
///
/// Returns the merged `AgentConfig` and an optional model override string.
/// The caller is responsible for applying the model override to the `LlmClient`.
fn apply_overrides(
    base: alms_runtime::AgentConfig,
    agent_record: Option<&alms_core::AgentRecord>,
    overrides: &RunOverrides,
) -> MergedConfig {
    let mut cfg = base;

    // ── Model: per-run > per-agent (server default is in LlmClient) ──
    let model_override = if overrides.model.is_some() {
        overrides.model.clone()
    } else {
        agent_record.and_then(|r| r.model.clone())
    };

    // ── Per-agent overrides (middle layer) ──
    if let Some(record) = agent_record
        && let Some(ref p) = record.posture
        && let Ok(posture) = p.parse::<alms_runtime::Posture>()
    {
        cfg.posture = posture;
    }

    // ── Per-run overrides (highest precedence) ──
    if let Some(m) = overrides.max_tokens.filter(|&m| m > 0) {
        cfg.max_tokens = m;
    }
    if let Some(ref p) = overrides.posture
        && let Ok(posture) = p.parse::<alms_runtime::Posture>()
    {
        cfg.posture = posture;
    }

    MergedConfig {
        agent_config: cfg,
        model_override,
    }
}

/// GET /runs?session_id=<uuid>&limit=<n> — list runs for a session
pub async fn list_runs(
    State(state): State<AppState>,
    Query(params): Query<ListRunsQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50);
    let runs = state.run_manager.list_by_session(params.session_id, limit);
    let responses: Vec<RunStatusResponse> = runs.into_iter().map(RunStatusResponse::from).collect();
    Json(serde_json::json!({ "runs": responses }))
}

#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    pub session_id: SessionId,
    pub limit: Option<usize>,
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
fn resolve_posture_for_run(
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
/// The DM context ID format is `dm:{first}:{second}` where the names are
/// alphabetically sorted (see [`alms_core::dm_context_id`]). The peer is
/// whichever name is NOT the current agent.
///
/// Returns `None` if the context ID does not match the expected format or
/// neither name matches `agent_name`.
///
/// Note: `split_once(':')` is safe because agent names are restricted to
/// `[a-z0-9-]` by `validate_agent_name` — colons cannot appear in names.
fn extract_peer_from_dm_context(context_id: &str, agent_name: &str) -> Option<String> {
    let rest = context_id.strip_prefix("dm:")?;
    let (first, second) = rest.split_once(':')?;
    if first == agent_name {
        Some(second.to_string())
    } else if second == agent_name {
        Some(first.to_string())
    } else {
        None
    }
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
async fn execute_run(state: AppState, params: RunParams) {
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
    let mut agent_config = resolved.agent_config;
    let mut llm = resolved.llm;
    if let Some(m) = overrides.max_tokens.filter(|&m| m > 0) {
        agent_config.max_tokens = m;
    }
    if let Some(ref p) = overrides.posture
        && let Ok(posture) = p.parse::<alms_runtime::Posture>()
    {
        agent_config.posture = posture;
    }
    if let Some(ref provider) = overrides.provider {
        info!("Run {} using provider override: {}", run_id.0, provider);
        llm = llm.with_provider_and_secrets(provider, &state.secrets.read());
    }
    if let Some(ref model) = overrides.model {
        info!("Run {} using model override: {}", run_id.0, model);
        llm = llm.with_model(model.clone());
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

    // Capture summary config before agent_config and llm are consumed.
    // C1 fix: resolve the summary model *from the per-agent LLM client* so
    // that when `summary_model` is None we fall back to the agent's configured
    // model, not the server default.  After this line `llm` is consumed by
    // `AgentRuntime::new` and no longer available.
    let run_summary_mode = agent_config.context_config.run_summary_mode.clone();
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
                    } => SseEventData::tool_start(
                        bg_run_id,
                        crate::sse::ToolInvocationId(invocation_id),
                        &tool,
                        params,
                        source_agent,
                    ),
                    alms_runtime::RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        source_agent,
                    } => SseEventData::tool_end(
                        bg_run_id,
                        crate::sse::ToolInvocationId(invocation_id),
                        ok,
                        result,
                        source_agent,
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

    // Spawn forwarder: converts RuntimeEvents → SseEventData (and stores approvals).
    // We keep the handle so we can await it after the runtime finishes, ensuring
    // all tool events are flushed before we send run_finished.
    let forwarder_state = state.clone();
    let forwarder_handle = tokio::spawn(forward_runtime_events(
        runtime_rx,
        run_id,
        session_id,
        forwarder_state.run_manager.clone(),
        forwarder_state.approval_store.clone(),
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

    let result = if is_peer_message {
        // Peer-triggered run: the input message is already in the shared
        // session (written by MessageBus with from_agent metadata).
        // Use run_on_session to look up the session by ID directly and
        // skip re-persisting the input (fixes C1 session split + C2 double-write).
        runtime
            .run_on_session(&state.session_manager, session_id, &context_id, &input)
            .await
    } else {
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

            // Detect max-iterations sentinel and emit a warning event so the
            // frontend can style it distinctly (yellow) instead of as a normal
            // agent message.
            if output.response == MAX_ITERATIONS_SENTINEL {
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
                    && let Ok(history) = state.session_manager.get_history(session_id)
                    && let Some(last_own) = history.iter().rev().find(|m| {
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

            // ── ignore_message detection for DM conversations (#387) ──
            //
            // When a peer-message DM run contains an `ignore_message` tool call,
            // signal the end of the conversation to the MessageBus so the peer
            // gets notified and the depth counter resets.  See Phase 3 of #384.
            //
            // Previously this used `response_is_empty` as a proxy, but after the
            // Bug 1 fix in PR #412 (`should_terminate_after_dm_send`) the loop
            // also breaks with an empty response after `send_message`.  We now
            // inspect the actual tool-call records to distinguish `ignore_message`
            // from `send_message`.
            let ran_ignore_message = alms_core::ran_ignore_message_successfully(&output.tool_calls);
            if is_peer_message && ran_ignore_message && context_id.starts_with("dm:") {
                if let Some(ref name) = agent_name
                    && let Some(peer_name) = extract_peer_from_dm_context(&context_id, name)
                {
                    // Resolve the peer's AgentId from the agent registry.
                    let peer_agent_id = state
                        .session_manager
                        .store()
                        .and_then(|store| store.load_agent_by_name(&peer_name).ok())
                        .flatten()
                        .map(|record| record.id);

                    if let Some(peer_id) = peer_agent_id {
                        info!(
                            agent = %name,
                            peer = %peer_name,
                            "DM run ended with ignore_message — signalling conversation end"
                        );
                        let end_reason = ConversationEndReason::Ignored;
                        match state
                            .message_bus
                            .end_conversation(name, agent_id, &peer_name, peer_id, end_reason)
                            .await
                        {
                            Ok(()) => {
                                // Emit dm_conversation_ended SSE event on the
                                // DM session stream so the web UI can show a
                                // "conversation ended" indicator. Phase 6 of #384.
                                //
                                // NOTE: This code path only fires for the
                                // ignore_message reason.  The depth-exceeded
                                // reason emits this event from
                                // `run_trigger_loop` when processing the
                                // `ConversationEnded` trigger (#419).
                                //
                                // NOTE: If both agents ignore simultaneously,
                                // end_conversation returns Ok(()) for both
                                // callers (the second sees "already ended by
                                // peer" and returns Ok).  This means duplicate
                                // dm_conversation_ended SSE events may be
                                // emitted for the same session.  The frontend
                                // should be prepared to handle duplicates.
                                state
                                    .run_manager
                                    .send_session_event(
                                        session_id,
                                        run_id,
                                        SseEventData::dm_conversation_ended(
                                            session_id,
                                            name,
                                            &peer_name,
                                            &end_reason.to_string(),
                                            &context_id,
                                        ),
                                    )
                                    .await;

                                // Forward to the initiating agent's web-chat
                                // session so the user watching that session
                                // sees the notification (the DM session event
                                // above is invisible to the web-chat).
                                notify_dm_ended_to_webchat(
                                    &state,
                                    agent_id,
                                    &peer_name,
                                    &end_reason.to_string(),
                                    &context_id,
                                )
                                .await;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to signal conversation end for {name} -> {peer_name}: {e}"
                                );
                            }
                        }
                    } else {
                        warn!(
                            peer = %peer_name,
                            "Cannot signal conversation end — peer agent not found in registry"
                        );
                    }
                } else {
                    debug!(
                        "DM ignore_message detected but could not extract peer from context_id '{}'",
                        context_id
                    );
                }
            }

            info!("Run {} completed successfully", run_id.0);
        }
        Err(alms_core::AlmsError::Cancelled) => {
            state
                .run_manager
                .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
                .await;

            state.run_manager.mark_run_as_cancelled(run_id);

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

            error!("Run {} failed: {}", run_id.0, e);
        }
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

// ---------------------------------------------------------------------------
// Scheduler integration
// ---------------------------------------------------------------------------

/// Receives fired job IDs from the scheduler and dispatches agent runs.
///
/// Each fired job is handled in its own spawned task so a slow run does not
/// block the fire loop from processing subsequent firings.
pub(crate) async fn scheduler_fire_loop(mut rx: mpsc::UnboundedReceiver<JobId>, state: AppState) {
    while let Some(job_id) = rx.recv().await {
        // Resolve session for queue keying so jobs on the same session
        // don't race with each other or with interactive runs.
        let Some(job) = state.job_store.get(job_id) else {
            continue;
        };
        if job.status == JobStatus::Cancelled {
            continue;
        }
        let state_clone = state.clone();
        state.agent_queue.enqueue(
            job.agent_id,
            Box::pin(async move {
                if let Err(e) = fire_job_run(state_clone, job_id).await {
                    error!("Job {} run dispatch failed: {}", job_id, e);
                }
            }),
        );
    }
}

/// Create and execute an agent run triggered by a scheduled job.
#[instrument(level = "info", skip(state), fields(job_id = %job_id))]
async fn fire_job_run(state: AppState, job_id: JobId) -> alms_core::AlmsResult<()> {
    // Look up the job — it may have been cancelled between scheduling and firing.
    let Some(job) = state.job_store.get(job_id) else {
        info!("Skipping fired job — not found in store");
        return Ok(());
    };
    if job.status == JobStatus::Cancelled {
        info!("Skipping fired job — already cancelled");
        return Ok(());
    }

    // Use a stable context_id so each job accumulates session history across firings.
    let context_id = format!("job_{}", job_id.0);
    let session = state
        .session_manager
        .get_or_create(job.agent_id, &context_id);
    let session_id = session.id;

    let run = Run::for_job(session_id, job.agent_id, job.prompt.clone(), job_id);
    let run_id = run.run_id;
    state.run_manager.insert_run(run.clone());
    // Job runs execute inline (not via agent_queue) so queued_behind is 0.
    state
        .run_manager
        .send_session_event(
            session_id,
            run_id,
            SseEventData::run_created(run_id, session_id, true, Some("job".to_string()), 0),
        )
        .await;
    info!("Job fired → run {}", run_id.0);

    // Execute the run (awaits completion; errors are handled inside execute_run).
    // Register the token so scheduled job runs are cancellable via POST /runs/{id}/cancel
    // in addition to the job-level DELETE /jobs/{id} path.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    execute_run(
        state.clone(),
        RunParams {
            run_id,
            session_id,
            agent_id: job.agent_id,
            input: run.input,
            overrides: RunOverrides::default(),
            context_id,
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
        },
    )
    .await;

    // ── Job completion notification ──
    // Send a notification to the agent's most recent user-facing session
    // so the user can see that the job ran (even if they weren't watching
    // the hidden job_* session).
    notify_job_completion(&state, job.agent_id, &job.prompt, run_id).await;

    // Guard: if the job was cancelled while the run was in progress, do not
    // overwrite the Cancelled status or re-arm the scheduler.
    if state
        .job_store
        .get(job_id)
        .map(|j| j.status == JobStatus::Cancelled)
        .unwrap_or(true)
    {
        info!("Job was cancelled during run, skipping post-run update");
        return Ok(());
    }

    // Update job record after run completes.
    let now = Utc::now();
    let (new_status, next_run_at) = match &job.schedule {
        JobSchedule::Once { .. } => (JobStatus::Cancelled, None),
        JobSchedule::Recurring { cron } => {
            let next = cron_utils::next_after(cron, now);
            if next.is_none() {
                warn!("Recurring cron '{}' has no future occurrences", cron);
            }
            (JobStatus::Active, next)
        }
    };

    state
        .job_store
        .record_run(job_id, now, new_status, next_run_at)?;

    // Re-arm recurring jobs with the next computed fire time.
    if let Some(next) = next_run_at {
        let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job_id, instant).await;
        info!("Recurring job re-armed for {}", next);
    }

    Ok(())
}

/// Prefixes that identify internal (non-user-facing) sessions.
///
/// Sessions whose `context_id` starts with any of these prefixes are excluded
/// when searching for the user's web-chat session.  This list is the single
/// source of truth for [`find_user_facing_session`], [`notify_job_completion`],
/// and [`notify_dm_ended_to_webchat`].
const INTERNAL_SESSION_PREFIXES: &[&str] =
    &["job_", "subagent_", "dm:", "notifications:", "episodic:"];

/// Returns `true` if the given `context_id` belongs to an internal session
/// that should not be targeted by user-facing notifications.
fn is_internal_context_id(context_id: &str) -> bool {
    INTERNAL_SESSION_PREFIXES
        .iter()
        .any(|prefix| context_id.starts_with(prefix))
}

/// Find the most recent user-facing session for the given agent.
///
/// Returns `None` if the agent has no non-internal sessions.  Sessions are
/// returned in last-activity order by `list_all`, so the first match is the
/// most recently active one.
fn find_user_facing_session(
    session_manager: &alms_session::SessionManager,
    agent_id: alms_core::AgentId,
) -> Option<alms_session::Session> {
    session_manager
        .list_all()
        .into_iter()
        .find(|s| s.agent_id == agent_id && !is_internal_context_id(&s.context_id))
}

/// Send a job-completion notification to the agent's most recent user-facing
/// session. This makes job runs visible in the chat without creating a full
/// notification run (which would trigger another LLM call).
async fn notify_job_completion(
    state: &AppState,
    agent_id: alms_core::AgentId,
    job_prompt: &str,
    run_id: RunId,
) {
    // Determine outcome from the completed run.
    let (status, summary) = match state.run_manager.get_run(run_id) {
        Some(run) => match run.status {
            RunStatus::Completed => {
                let output = run.output.unwrap_or_default();
                let summary: String = if output.len() > 200 {
                    format!("{}...", output.chars().take(200).collect::<String>())
                } else {
                    output
                };
                ("success", summary)
            }
            RunStatus::Failed => {
                let err = run.error.unwrap_or_else(|| "unknown error".to_string());
                ("error", err)
            }
            RunStatus::Cancelled => ("cancelled", "run was cancelled".to_string()),
            RunStatus::Queued | RunStatus::Running => {
                // Shouldn't happen — execute_run already returned.
                ("unknown", "run still in progress".to_string())
            }
        },
        None => ("error", "run record not found".to_string()),
    };

    // Find the agent's most recent user-facing session (exclude internal sessions).
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        debug!(
            "No user-facing session for agent {} — skipping job notification",
            agent_id
        );
        return;
    };
    let target_session_id = target.id;

    // Truncate the prompt for display.
    let job_name: String = if job_prompt.len() > 60 {
        format!("{}...", job_prompt.chars().take(60).collect::<String>())
    } else {
        job_prompt.to_string()
    };

    // Send SSE event to the target session so connected UI clients see it.
    state
        .run_manager
        .send_session_event(
            target_session_id,
            alms_core::RunId::new(), // no associated run on this session
            SseEventData::job_completed(target_session_id, &job_name, status, &summary),
        )
        .await;

    // Persist a marker message to the session history so it appears on reload.
    let label = match status {
        "success" => "completed",
        "error" => "failed",
        _ => "finished",
    };
    let marker = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::System,
        content: alms_session::Content::Text(format!(
            "[Scheduled job {label}] {job_name}\n{summary}"
        )),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "synthetic": true,
            "type": "job_notification"
        })),
    };
    if let Err(e) = state
        .session_manager
        .append_message(target_session_id, marker)
    {
        warn!("Failed to persist job completion marker: {e}");
    }

    info!(
        "Job notification sent to session {} (status={status})",
        target_session_id.0
    );
}

/// Forward a `dm_conversation_ended` event to the agent's user-facing
/// web-chat session so the human watching that session sees a notification.
///
/// Without this, the `dm_conversation_ended` SSE event only lands on the DM
/// session's SSE stream (which the user is typically not watching) and the
/// notification run executes on a `notifications:` session (also invisible
/// to the web-chat).
///
/// This mirrors `notify_job_completion`: find the most recent user-facing
/// session, emit an SSE event, and persist a marker message so it survives
/// page reloads.
async fn notify_dm_ended_to_webchat(
    state: &AppState,
    agent_id: alms_core::AgentId,
    peer_name: &str,
    reason: &str,
    context_id: &str,
) {
    info!(
        agent_id = %agent_id,
        peer = %peer_name,
        reason = %reason,
        "notify_dm_ended_to_webchat called — looking for user-facing session"
    );

    // Find the agent's most recent user-facing session (exclude internal sessions).
    let Some(target) = find_user_facing_session(&state.session_manager, agent_id) else {
        info!(
            agent_id = %agent_id,
            "No user-facing session for agent — skipping DM ended web-chat notification"
        );
        return;
    };
    let target_session_id = target.id;

    // Emit SSE event on the web-chat session so connected UI clients see it.
    let dummy_run_id = RunId::new();
    state
        .run_manager
        .send_session_event(
            target_session_id,
            dummy_run_id,
            SseEventData::dm_conversation_ended(
                target_session_id,
                "system",
                peer_name,
                reason,
                context_id,
            ),
        )
        .await;

    // Persist a marker message so it appears on reload.
    let reason_text = match reason {
        "ignored" => "no further replies".to_string(),
        "depth_exceeded" => "message limit reached".to_string(),
        other => other.to_string(),
    };
    let marker = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::System,
        content: alms_session::Content::Text(format!(
            "[DM conversation ended] Conversation with {peer_name} ended ({reason_text})."
        )),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "synthetic": true,
            "type": "dm_ended_notification",
            "peer": peer_name,
            "reason": reason,
            "context_id": context_id,
        })),
    };
    if let Err(e) = state
        .session_manager
        .append_message(target_session_id, marker)
    {
        warn!("Failed to persist DM ended marker to web-chat session: {e}");
    }

    info!(
        "DM ended notification forwarded to web-chat session {} (peer={peer_name}, reason={reason})",
        target_session_id.0
    );
}

// ---------------------------------------------------------------------------
// Subagent completion notifications
// ---------------------------------------------------------------------------

/// Receives background subagent completion events and creates follow-up
/// runs on the parent agent's session so the parent is automatically notified.
///
/// This mirrors `scheduler_fire_loop`: each notification is enqueued via
/// `SessionQueue` to respect per-session FIFO ordering.
pub(crate) async fn completion_notification_loop(
    mut rx: mpsc::UnboundedReceiver<alms_coordinator::SubagentCompletion>,
    state: AppState,
) {
    while let Some(completion) = rx.recv().await {
        let session_id = completion.parent_session_id;
        let agent_id = completion.parent_agent_id;

        // Verify the parent session still exists.
        let context_id = match state.session_manager.get(session_id) {
            Ok(session) => session.context_id,
            Err(_) => {
                warn!(
                    session_id = %session_id.0,
                    task_id = %completion.task_id.0,
                    "Parent session not found for subagent completion notification — skipping"
                );
                continue;
            }
        };

        // Notify session subscribers that a subagent completed.
        // This updates the SubagentBar and shows a system message BEFORE
        // the notification run starts.
        let status_str = match completion.status {
            alms_coordinator::TaskStatus::Completed => "done",
            alms_coordinator::TaskStatus::Failed => "fail",
            alms_coordinator::TaskStatus::Cancelled => "cancelled",
            _ => "done",
        };
        state
            .run_manager
            .send_session_event(
                session_id,
                alms_core::RunId::new(), // no run yet
                SseEventData::subagent_completed(
                    session_id,
                    completion.subagent_name.clone(),
                    status_str,
                    &completion.summary,
                ),
            )
            .await;

        // Persist the subagent completion marker to session history so it
        // survives page refreshes and appears in the chat on reload.
        {
            let name = completion.subagent_name.as_deref().unwrap_or("subagent");
            let label = match status_str {
                "fail" => "failed",
                "cancelled" => "cancelled",
                _ => "completed",
            };
            let marker = alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::Assistant,
                content: alms_session::Content::Text(format!("[Subagent '{}' {}]", name, label)),
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            };
            if let Err(e) = state.session_manager.append_message(session_id, marker) {
                warn!("Failed to persist subagent completion marker: {e}");
            }
        }

        let notification = format_completion_notification(&completion);

        info!(
            session_id = %session_id.0,
            task_id = %completion.task_id.0,
            subagent = ?completion.subagent_name,
            "Subagent completion → creating notification run"
        );

        let run_id = enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            notification,
            context_id,
            "subagent".to_string(),
            false, // subagent completion — not a peer message
        )
        .await;

        debug!(
            run_id = %run_id.0,
            session_id = %session_id.0,
            task_id = %completion.task_id.0,
            "Notification run enqueued"
        );
    }
}

/// Creates a run, registers it, sends the SSE `run_created` event, and
/// enqueues the run at low priority for execution.
///
/// Shared helper for [`completion_notification_loop`] and [`run_trigger_loop`],
/// which both follow the same create-register-enqueue pattern.
async fn enqueue_triggered_run(
    state: &AppState,
    agent_id: alms_core::AgentId,
    session_id: SessionId,
    input: String,
    context_id: String,
    source_label: String,
    is_peer_message: bool,
) -> RunId {
    let run = Run::new(session_id, agent_id, input.clone());
    let run_id = run.run_id;
    state.run_manager.insert_run(run);

    let queued_behind = state.agent_queue.pending_count(&agent_id);
    state
        .run_manager
        .send_session_event(
            session_id,
            run_id,
            SseEventData::run_created(run_id, session_id, true, Some(source_label), queued_behind),
        )
        .await;

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let state_clone = state.clone();
    state.agent_queue.enqueue_low(
        agent_id,
        Box::pin(async move {
            execute_run(
                state_clone,
                RunParams {
                    run_id,
                    session_id,
                    agent_id,
                    input,
                    overrides: RunOverrides::default(),
                    context_id,
                    cancel_token,
                    is_peer_message,
                    // All runs via enqueue_triggered_run are system-triggered
                    // (no human watching), so Guarded posture is overridden.
                    is_system_triggered: true,
                },
            )
            .await;
        }),
    );

    run_id
}

/// Template for subagent completion notifications, loaded at compile time from
/// `crates/alms-runtime/prompts/subagent_completed.md`.
const SUBAGENT_COMPLETED_TEMPLATE: &str =
    include_str!("../../alms-runtime/prompts/subagent_completed.md");

/// Format a human-readable notification message for the parent agent.
fn format_completion_notification(c: &alms_coordinator::SubagentCompletion) -> String {
    let status = match c.status {
        alms_coordinator::TaskStatus::Completed => "completed",
        alms_coordinator::TaskStatus::Failed => "failed",
        alms_coordinator::TaskStatus::Cancelled => "cancelled",
        _ => "finished",
    };

    let (label, follow_up) = match &c.subagent_name {
        Some(name) => (
            format!("\"{name}\""),
            format!("Use read_subagent_session(\"{name}\") for the full conversation history."),
        ),
        None => (
            format!("(task {})", c.task_id.0),
            "The subagent result summary is included above.".to_string(),
        ),
    };

    SUBAGENT_COMPLETED_TEMPLATE
        .replace("{label}", &label)
        .replace("{status}", status)
        .replace("{summary}", &c.summary)
        .replace("{follow_up}", &follow_up)
}

/// Maximum character length for the formatted conversation transcript
/// included in DM-ended notifications. Very long conversations are
/// truncated from the beginning (keeping the most recent messages) so the
/// agent sees the tail of the discussion.
const DM_HISTORY_MAX_CHARS: usize = 4000;

/// Template for DM-ended notification with conversation history, loaded at
/// compile time from `crates/alms-runtime/prompts/dm_ended_with_history.md`.
const DM_ENDED_WITH_HISTORY_TEMPLATE: &str =
    include_str!("../../alms-runtime/prompts/dm_ended_with_history.md");

/// Template for DM-ended notification without history (fallback), loaded at
/// compile time from `crates/alms-runtime/prompts/dm_ended_no_history.md`.
const DM_ENDED_NO_HISTORY_TEMPLATE: &str =
    include_str!("../../alms-runtime/prompts/dm_ended_no_history.md");

/// Format a human-readable notification message for a DM conversation ending.
///
/// This is used by `run_trigger_loop` when it receives a
/// `MessageSource::ConversationEnded` trigger.  The notification tells the
/// peer agent that the DM conversation has ended, includes the reason, and
/// — when `conversation_history` is provided — embeds the full DM
/// transcript so the agent can act immediately without calling
/// `read_messages`.
fn format_dm_ended_notification(
    from_name: &str,
    reason: ConversationEndReason,
    conversation_history: Option<&str>,
) -> String {
    let reason_text = match reason {
        ConversationEndReason::Ignored => {
            format!("Agent \"{from_name}\" ended the conversation (chose not to reply).")
        }
        ConversationEndReason::DepthExceeded => {
            format!(
                "The conversation with agent \"{from_name}\" was terminated \
                 because the maximum message depth was reached."
            )
        }
    };

    match conversation_history {
        Some(history) if !history.is_empty() => DM_ENDED_WITH_HISTORY_TEMPLATE
            .replace("{reason}", &reason_text)
            .replace("{history}", history),
        _ => {
            // Fallback: no history available (session already cleaned up,
            // or error reading it). Point the agent at read_messages.
            DM_ENDED_NO_HISTORY_TEMPLATE
                .replace("{reason}", &reason_text)
                .replace("{from}", from_name)
        }
    }
}

/// Format a DM session's messages into a human-readable conversation
/// transcript suitable for embedding in a notification.
///
/// Only text messages are included (tool calls, tool results, images, and
/// system markers like `dm_ended` are skipped). Each message is formatted
/// as:
///
/// ```text
/// [HH:MM] agent_name: message text
/// ```
///
/// The output is truncated to [`DM_HISTORY_MAX_CHARS`] characters. When
/// truncation is needed, the oldest messages are dropped and a leading
/// note indicates how many messages were omitted.
fn format_dm_conversation_history(messages: &[alms_session::Message]) -> String {
    // Collect renderable lines (only text messages with content).
    let mut lines: Vec<String> = Vec::new();

    for msg in messages {
        // Skip non-text content (tool calls, tool results, images).
        let text = match &msg.content {
            alms_session::Content::Text(t) => t.as_str(),
            _ => continue,
        };

        // Skip empty text (e.g. dm_ended markers with empty body).
        if text.trim().is_empty() {
            continue;
        }

        // Skip system metadata markers (dm_ended, synthetic notifications).
        if let Some(ref meta) = msg.metadata {
            let msg_type = meta
                .get("message_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if msg_type == "dm_ended" {
                continue;
            }
            if meta.get("synthetic").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
        }

        // Extract sender name from metadata, or fall back to role.
        let sender = msg
            .metadata
            .as_ref()
            .and_then(|m| m.get("from_agent"))
            .and_then(|v| v.as_str())
            .unwrap_or(match msg.role {
                alms_session::Role::User => "user",
                alms_session::Role::Assistant => "assistant",
                alms_session::Role::System => "system",
                alms_session::Role::Tool => "tool",
            });

        let ts = msg.timestamp.0.format("%H:%M");
        lines.push(format!("[{ts}] {sender}: {text}"));
    }

    if lines.is_empty() {
        return String::new();
    }

    // Build the full transcript and truncate from the front if needed.
    let full = lines.join("\n");
    if full.len() <= DM_HISTORY_MAX_CHARS {
        return full;
    }

    // Walk from the end to find how many lines fit within the budget,
    // leaving room for the "[N earlier messages omitted]" prefix.
    let omitted_prefix_budget = 60; // generous estimate
    let budget = DM_HISTORY_MAX_CHARS.saturating_sub(omitted_prefix_budget);
    let mut included_start = lines.len();
    let mut accumulated = 0usize;
    for (i, line) in lines.iter().enumerate().rev() {
        // +1 for the newline separator
        let cost = line.len() + if i < lines.len() - 1 { 1 } else { 0 };
        if accumulated + cost > budget {
            break;
        }
        accumulated += cost;
        included_start = i;
    }

    let omitted = included_start;
    let truncated_lines = &lines[included_start..];
    format!(
        "[{omitted} earlier message(s) omitted]\n{}",
        truncated_lines.join("\n")
    )
}

// ---------------------------------------------------------------------------
// RunTrigger loop (peer messaging)
// ---------------------------------------------------------------------------

/// Processes `RunTrigger` events from the `MessageBus`.
///
/// Each trigger creates a run on the target agent's session, reusing the
/// same `execute_run` path as user-initiated and notification runs.
///
/// For `Agent` triggers (peer DMs), the message has already been persisted
/// to the shared DM session by the `MessageBus`; we pass `is_peer = true`
/// so `execute_run` uses `run_on_session` (no double-write).
///
/// For `ConversationEnded` triggers, the notification text has NOT been
/// persisted — the MessageBus only wrote a `dm_ended` marker to the DM
/// session, not to the notification session.  We format a richer
/// notification here and pass `is_peer = false` so `execute_run` uses
/// `runtime.run()`, which persists the input to the notification session.
pub(crate) async fn run_trigger_loop(
    mut rx: mpsc::UnboundedReceiver<alms_coordinator::message_bus::RunTrigger>,
    state: AppState,
) {
    use alms_coordinator::message_bus::MessageSource;

    while let Some(trigger) = rx.recv().await {
        let session_id = trigger.session_id;
        let agent_id = trigger.agent_id;
        let context_id = trigger.context_id;

        // Build a source label for SSE `run_created` events and determine
        // whether this is a peer DM run (which needs the DM addendum) or
        // a notification run (which must NOT get the DM addendum).
        let (source_label, is_peer, input) = match &trigger.source {
            MessageSource::Agent { from_name, .. } => (
                format!("peer:{from_name}"),
                true,
                // Peer DM: input already persisted by MessageBus — pass it
                // through so the Run record has a copy.
                trigger.input,
            ),
            MessageSource::SubagentCompletion => ("subagent".to_string(), false, trigger.input),
            MessageSource::ConversationEnded {
                from_name,
                reason,
                source_session_id,
                ..
            } => {
                // Resolve the peer (notification recipient) name for SSE
                // events and DM context reconstruction.
                let peer_name_resolved = state
                    .session_manager
                    .store()
                    .and_then(|store| store.load_agent_by_id(agent_id).ok())
                    .flatten()
                    .map(|r| r.name);

                // ── Emit dm_conversation_ended SSE for depth-exceeded (#419) ──
                //
                // The ignore_message path emits this event in execute_run
                // (line ~967). The depth-exceeded path calls
                // end_conversation deep inside MessageBus::send(), which
                // has no access to SSE infrastructure. We emit the event
                // here instead, since the ConversationEnded trigger
                // carries all the information we need.
                if *reason == ConversationEndReason::DepthExceeded {
                    if let Some(ref peer_name) = peer_name_resolved {
                        let dm_context = alms_core::dm_context_id(from_name, peer_name);
                        let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);

                        info!(
                            from = %from_name,
                            peer = %peer_name,
                            dm_session = %dm_session_id.0,
                            "Emitting dm_conversation_ended SSE for depth-exceeded"
                        );

                        // Use a dummy RunId because the notification run has
                        // not been created yet at this point.
                        let dummy_run_id = RunId::new();
                        state
                            .run_manager
                            .send_session_event(
                                dm_session_id,
                                dummy_run_id,
                                SseEventData::dm_conversation_ended(
                                    dm_session_id,
                                    from_name,
                                    peer_name,
                                    &reason.to_string(),
                                    &dm_context,
                                ),
                            )
                            .await;
                    } else {
                        warn!(
                            agent_id = %agent_id.0,
                            from = %from_name,
                            "Skipping dm_conversation_ended SSE for depth-exceeded: \
                             agent not found in registry, cannot resolve peer name"
                        );
                    }
                }

                // ── Forward notification to the agent's web-chat session ──
                //
                // When the notification run is routed to the source session
                // (the session where `send_message` was originally called),
                // the user is already watching that session -- no separate
                // forwarding needed. Only forward to web-chat when the
                // notification falls back to `notifications:` (invisible).
                if source_session_id.is_none()
                    && let Some(ref peer_name) = peer_name_resolved
                {
                    let dm_context = alms_core::dm_context_id(from_name, peer_name);
                    notify_dm_ended_to_webchat(
                        &state,
                        agent_id,
                        from_name,
                        &reason.to_string(),
                        &dm_context,
                    )
                    .await;
                }

                // ── Fetch DM conversation history (#429) ──
                //
                // Resolve the DM session and format its message history so
                // the notification includes the full transcript. This saves
                // the agent an LLM round-trip that would otherwise be spent
                // calling read_messages.
                let conversation_history = peer_name_resolved.as_ref().and_then(|peer_name| {
                    let dm_session_id = SessionId::deterministic_dm(from_name, peer_name);
                    match state.session_manager.get_history(dm_session_id) {
                        Ok(messages) => {
                            let formatted = format_dm_conversation_history(&messages);
                            if formatted.is_empty() {
                                None
                            } else {
                                Some(formatted)
                            }
                        }
                        Err(e) => {
                            warn!(
                                dm_session = %dm_session_id.0,
                                error = %e,
                                "Failed to fetch DM history for notification — \
                                 falling back to read_messages hint"
                            );
                            None
                        }
                    }
                });

                (
                    format!("notification:dm_ended:{from_name}"),
                    // NOT a peer message — the notification run should not get
                    // the DM addendum injected (it tells the agent to use
                    // send_message/ignore_message, which is wrong here).
                    false,
                    // Format a richer notification that includes the reason,
                    // the DM conversation transcript (when available), and a
                    // follow-up hint.
                    format_dm_ended_notification(
                        from_name,
                        *reason,
                        conversation_history.as_deref(),
                    ),
                )
            }
        };

        info!(
            session_id = %session_id.0,
            agent_id = %agent_id.0,
            source = %source_label,
            "RunTrigger -> creating run"
        );

        enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            input,
            context_id,
            source_label,
            is_peer,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Runtime event forwarding
// ---------------------------------------------------------------------------

/// Reads RuntimeEvents from the runtime and forwards them as SSE events.
/// Also stores ApprovalRequired events in the approval store so clients can resolve them.
async fn forward_runtime_events(
    mut rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    run_id: RunId,
    session_id: SessionId,
    run_manager: crate::server::RunManager,
    approval_store: ApprovalStore,
) {
    while let Some(event) = rx.recv().await {
        match event {
            RuntimeEvent::TokenDelta {
                delta,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::token_delta(run_id, &delta, source_agent),
                    )
                    .await;
            }
            RuntimeEvent::ToolStart {
                invocation_id,
                tool,
                params,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::tool_start(
                            run_id,
                            ToolInvocationId(invocation_id),
                            &tool,
                            params,
                            source_agent,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::ToolEnd {
                invocation_id,
                ok,
                result,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::tool_end(
                            run_id,
                            ToolInvocationId(invocation_id),
                            ok,
                            result,
                            source_agent,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::Status { phase, detail } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::status(run_id, &phase, detail),
                    )
                    .await;
            }
            RuntimeEvent::ApprovalRequired {
                approval_id,
                tool,
                params,
                decision_tx,
                source_agent,
            } => {
                let request = serde_json::json!({"tool": &tool, "params": &params});
                approval_store.insert(PendingApproval {
                    approval_id,
                    run_id,
                    tool: tool.clone(),
                    params: params.clone(),
                    requested_at: Utc::now(),
                    decision_tx,
                });
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::approval_required(
                            run_id,
                            &approval_id.to_string(),
                            &tool,
                            request,
                            source_agent,
                        ),
                    )
                    .await;
            }
            RuntimeEvent::Warning {
                code,
                message,
                source_agent,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::run_warning(run_id, &code, &message, source_agent),
                    )
                    .await;
            }
        }
    }
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

/// GET /runs/{run_id}/events - Stream events via SSE
///
/// Supports Last-Event-ID header for reconnect.
#[instrument(level = "info", skip(state, headers), fields(run_id = %run_id.0))]
pub async fn stream_run_events(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let from_id = last_event_id.map(|id| id + 1).unwrap_or(0);

    // Check run exists — return 404 for nonexistent runs instead of
    // leaking an orphaned sender entry.
    let run = state
        .run_manager
        .get_run(run_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Run not found"))?;

    let is_terminal = matches!(
        run.status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    );

    if is_terminal {
        // Run is done — replay historical events then close. No sender
        // registration since no new events will arrive.
        let logged_events = state.run_manager.events_from(run_id, from_id).await;
        let replay_events: Vec<SseEventData> = logged_events
            .into_iter()
            .map(|e| SseEventData {
                event_type: e.event_type,
                data: e.data,
                ts: e.ts,
                event_id: Some(e.event_id),
            })
            .collect();
        info!(
            "Run {} is {:?}, replaying {} events then closing",
            run_id.0,
            run.status,
            replay_events.len()
        );
        Ok(RunEventStream::stream_replay_only(replay_events).into_response())
    } else {
        // Run is active — register sender BEFORE snapshotting the event
        // log to close the race where events produced between snapshot
        // and registration would be lost. Overlap is deduplicated by
        // stream_with_replay.
        let (tx, rx) = event_channel();
        state.run_manager.register_sender(run_id, tx);

        // TOCTOU guard: the run may have completed between the time the
        // client initiated the SSE request and `register_sender`. In that
        // case `execute_run` already called `remove_senders` before we
        // registered, so our sender entry is orphaned. Re-check and clean
        // up if needed (fixes #149).
        let became_terminal = state
            .run_manager
            .get_run(run_id)
            .map(|r| {
                matches!(
                    r.status,
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                )
            })
            .unwrap_or(true);

        if became_terminal {
            state.run_manager.remove_senders(run_id);
            warn!(
                "Run {} became terminal during SSE subscription — cleaned up orphaned sender",
                run_id.0
            );
            let logged_events = state.run_manager.events_from(run_id, from_id).await;
            let replay_events: Vec<SseEventData> = logged_events
                .into_iter()
                .map(|e| SseEventData {
                    event_type: e.event_type,
                    data: e.data,
                    ts: e.ts,
                    event_id: Some(e.event_id),
                })
                .collect();
            if !replay_events.is_empty() {
                info!(
                    "Replaying {} events for terminal run {}",
                    replay_events.len(),
                    run_id.0
                );
            }
            return Ok(RunEventStream::stream_replay_only(replay_events).into_response());
        }

        let logged_events = state.run_manager.events_from(run_id, from_id).await;
        let replay_events: Vec<SseEventData> = logged_events
            .into_iter()
            .map(|e| SseEventData {
                event_type: e.event_type,
                data: e.data,
                ts: e.ts,
                event_id: Some(e.event_id),
            })
            .collect();
        if !replay_events.is_empty() {
            info!(
                "Replaying {} events for active run {}",
                replay_events.len(),
                run_id.0
            );
        }
        Ok(RunEventStream::stream_with_replay(rx, replay_events).into_response())
    }
}

/// Query parameters for the session-level SSE endpoint.
#[derive(Debug, Deserialize)]
pub struct SessionEventsQuery {
    /// Client-supplied last event ID — used when the browser's EventSource
    /// cannot send the `Last-Event-Id` header (i.e. the initial connection).
    /// Takes precedence over the header when both are present.
    ///
    /// Accepted as `String` (not `u64`) because ephemeral SSE events
    /// (token_delta, status) use random UUIDs as their `id` field.  If the
    /// last event the browser saw was ephemeral, the client may send a UUID
    /// here.  Using `String` prevents Axum from rejecting the request with
    /// a 422 deserialization error, which would break SSE reconnection and
    /// leave the run appearing stuck (see #465 follow-up).
    pub last_event_id: Option<String>,
}

/// GET /sessions/{session_id}/events — persistent session-level SSE stream.
///
/// Unlike the per-run endpoint, this stream stays open across runs.
/// All events from any run on this session are forwarded, including
/// notification runs from subagent completions.
///
/// Supports `Last-Event-Id` header (browser auto-reconnect) **and**
/// `?last_event_id=<n>` query parameter (initial connection after REST
/// history load). Query parameter takes precedence when both are present.
#[instrument(level = "info", skip(state, headers, query), fields(session_id = %session_id.0))]
pub async fn stream_session_events(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
    headers: HeaderMap,
    Query(query): Query<SessionEventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // Verify session exists
    state
        .session_manager
        .get(session_id)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found"))?;

    // Query parameter takes precedence over header (the header is only sent
    // by the browser on automatic reconnects, not on the initial connection).
    //
    // Both sources may contain non-numeric values (UUIDs from ephemeral
    // events), so we parse with `.ok()` to silently ignore unparseable
    // values and fall back to replaying all events (from_id=0).
    let last_event_id = query
        .last_event_id
        .as_deref()
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        });
    let from_id = last_event_id.map(|id| id + 1).unwrap_or(0);

    let (tx, rx) = event_channel();
    state.run_manager.register_session_sender(session_id, tx);

    // Replay missed events on reconnect
    let logged_events = state
        .run_manager
        .session_events_from(session_id, from_id)
        .await;
    let replay_events: Vec<SseEventData> = logged_events
        .into_iter()
        .map(|e| SseEventData {
            event_type: e.event_type,
            data: e.data,
            ts: e.ts,
            event_id: Some(e.event_id),
        })
        .collect();

    if !replay_events.is_empty() {
        info!(
            "Replaying {} session events for {}",
            replay_events.len(),
            session_id.0
        );
    }

    Ok(RunEventStream::stream_with_replay(rx, replay_events).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, AgentRecord};
    use alms_runtime::{AgentConfig, Posture};
    use chrono::Utc;

    fn base_config() -> AgentConfig {
        AgentConfig {
            system_prompt: "server default prompt".into(),
            max_tokens: 100_000,
            posture: Posture::FullControl,
            ..AgentConfig::default()
        }
    }

    fn test_agent(model: Option<&str>, posture: Option<&str>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "test-agent".into(),
            description: String::new(),
            model: model.map(String::from),
            posture: posture.map(String::from),
            provider: None,
            telegram_token: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_no_overrides() {
        let base = base_config();
        let merged = apply_overrides(base.clone(), None, &RunOverrides::default());
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
        assert_eq!(merged.agent_config.max_tokens, 100_000);
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
        assert!(merged.model_override.is_none());
    }

    #[test]
    fn test_per_agent_overrides() {
        let agent = test_agent(Some("custom-model"), Some("guarded"));
        let merged = apply_overrides(base_config(), Some(&agent), &RunOverrides::default());
        assert!(matches!(merged.agent_config.posture, Posture::Guarded));
        assert_eq!(merged.model_override.as_deref(), Some("custom-model"));
        // max_tokens not overridden by agent
        assert_eq!(merged.agent_config.max_tokens, 100_000);
        // system_prompt is never overridden by agent — always server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_per_run_overrides_beat_per_agent() {
        let agent = test_agent(Some("agent-model"), Some("guarded"));
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(8192),
            posture: Some("full_control".into()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), Some(&agent), &overrides);
        // Per-run model wins over per-agent
        assert_eq!(merged.model_override.as_deref(), Some("run-model"));
        // Per-run posture wins over per-agent
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
        // Per-run max_tokens applied
        assert_eq!(merged.agent_config.max_tokens, 8192);
        // system_prompt always stays as server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_per_run_only() {
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(256),
            posture: Some("guarded".into()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        assert_eq!(merged.model_override.as_deref(), Some("run-model"));
        assert_eq!(merged.agent_config.max_tokens, 256);
        assert!(matches!(merged.agent_config.posture, Posture::Guarded));
        // system_prompt stays as server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_max_tokens_zero_ignored() {
        let overrides = RunOverrides {
            max_tokens: Some(0),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        assert_eq!(merged.agent_config.max_tokens, 100_000); // unchanged
    }

    #[test]
    fn test_unknown_posture_ignored() {
        let agent = test_agent(None, Some("yolo"));
        let merged = apply_overrides(base_config(), Some(&agent), &RunOverrides::default());
        // Unknown posture keeps server default
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
    }

    #[test]
    fn test_unknown_posture_per_run_ignored() {
        let overrides = RunOverrides {
            posture: Some("yolo".to_string()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        // Unknown per-run posture keeps server default
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
    }

    #[test]
    fn test_validate_provider_accepts_valid_providers() {
        assert!(validate_provider("openai").is_ok());
        assert!(validate_provider("anthropic").is_ok());
        assert!(validate_provider("openrouter").is_ok());
    }

    #[test]
    fn test_validate_provider_rejects_unknown() {
        let err = validate_provider("anthrpoic").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let body = err.1.0;
        assert_eq!(body["error"]["code"], "INVALID_PROVIDER");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("anthrpoic"),
            "error message should mention the invalid provider"
        );
    }

    #[test]
    fn test_validate_provider_rejects_telegram() {
        // telegram is a valid secret key but NOT a valid LLM provider
        let err = validate_provider("telegram").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_system_triggered_overrides_guarded_to_autonomous() {
        let result = resolve_posture_for_run(Posture::Guarded, true);
        assert_eq!(
            result,
            Posture::Autonomous,
            "Guarded posture should be overridden to Autonomous for system-triggered runs"
        );
    }

    #[test]
    fn test_system_triggered_does_not_override_full_control() {
        let result = resolve_posture_for_run(Posture::FullControl, true);
        assert_eq!(
            result,
            Posture::FullControl,
            "FullControl posture should NOT be overridden for system-triggered runs"
        );
    }

    #[test]
    fn test_system_triggered_does_not_override_autonomous() {
        let result = resolve_posture_for_run(Posture::Autonomous, true);
        assert_eq!(
            result,
            Posture::Autonomous,
            "Autonomous posture should remain unchanged for system-triggered runs"
        );
    }

    #[test]
    fn test_user_initiated_keeps_guarded() {
        let result = resolve_posture_for_run(Posture::Guarded, false);
        assert_eq!(
            result,
            Posture::Guarded,
            "Guarded posture should be preserved for user-initiated runs"
        );
    }

    #[test]
    fn test_per_run_provider_override_wires_through() {
        // Verify that setting provider in RunOverrides is carried through
        // to execute_run's LlmClient reconfiguration. We test the building
        // block (with_provider_and_secrets) since execute_run requires full
        // AppState; the wiring in execute_run is:
        //   if let Some(ref provider) = overrides.provider {
        //       llm = llm.with_provider_and_secrets(provider, &secrets);
        //   }
        use alms_runtime::LlmClient;
        use alms_runtime::llm_types::LlmConfig;

        let config = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.provider(), "openai");

        // Simulate what execute_run does when overrides.provider is Some
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let mut secrets = alms_core::secrets::SecretsStore::load(secrets_path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-override").unwrap();

        let overrides = RunOverrides {
            provider: Some("anthropic".into()),
            ..RunOverrides::default()
        };

        // Apply provider override the same way execute_run does
        let mut llm = client;
        if let Some(ref provider) = overrides.provider {
            llm = llm.with_provider_and_secrets(provider, &secrets);
        }

        assert_eq!(llm.provider(), "anthropic");
    }

    // -----------------------------------------------------------------------
    // extract_peer_from_dm_context tests (#387)
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_peer_agent_name_is_first() {
        // Context: dm:alice:bob, agent is alice -> peer is bob
        let peer = extract_peer_from_dm_context("dm:alice:bob", "alice");
        assert_eq!(peer.as_deref(), Some("bob"));
    }

    #[test]
    fn test_extract_peer_agent_name_is_second() {
        // Context: dm:alice:bob, agent is bob -> peer is alice
        let peer = extract_peer_from_dm_context("dm:alice:bob", "bob");
        assert_eq!(peer.as_deref(), Some("alice"));
    }

    #[test]
    fn test_extract_peer_agent_name_not_found() {
        // Agent name not in the context_id at all
        let peer = extract_peer_from_dm_context("dm:alice:bob", "charlie");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_non_dm_context() {
        // Not a DM context ID
        let peer = extract_peer_from_dm_context("notifications:alice", "alice");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_malformed_context() {
        // Missing second name
        let peer = extract_peer_from_dm_context("dm:alice", "alice");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_empty_context() {
        let peer = extract_peer_from_dm_context("", "alice");
        assert!(peer.is_none());
    }

    // -----------------------------------------------------------------------
    // ignore_message detection integration tests (#387)
    //
    // These test the detection conditions that determine whether
    // end_conversation should be called. Since execute_run requires
    // a full AppState, we test the condition logic directly.
    // -----------------------------------------------------------------------

    /// Build a minimal `ToolCallRecord` with the given role, tool name,
    /// tool_id, and optional result.
    fn make_tool_record(
        role: alms_core::ToolCallRole,
        tool_name: &str,
        tool_id: &str,
        result: Option<&str>,
    ) -> alms_core::ToolCallRecord {
        alms_core::ToolCallRecord {
            seq: 0,
            role,
            tool_name: Some(tool_name.to_string()),
            tool_id: Some(tool_id.to_string()),
            params: None,
            result: result.map(String::from),
            timestamp: Utc::now(),
        }
    }

    /// Helper: evaluates the three-way condition for ignore_message detection.
    ///
    /// Mirrors the production logic in `execute_run()`: the condition is
    /// `is_peer_message && ran_ignore_message && context_id.starts_with("dm:")`.
    ///
    /// Uses `alms_core::ran_ignore_message_successfully` which requires a
    /// matching non-conflict `Tool`-role result for each `Assistant`-role
    /// `ignore_message` record.
    fn should_signal_ignore(
        is_peer_message: bool,
        tool_calls: &[alms_core::ToolCallRecord],
        context_id: &str,
    ) -> bool {
        let ran_ignore_message = alms_core::ran_ignore_message_successfully(tool_calls);
        is_peer_message && ran_ignore_message && context_id.starts_with("dm:")
    }

    #[test]
    fn test_ignore_in_dm_context_triggers_end_conversation() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            should_signal_ignore(true, &records, "dm:alice:bob"),
            "ignore_message in DM context should trigger end_conversation"
        );
    }

    #[test]
    fn test_ignore_in_non_dm_context_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "session:abc123"),
            "ignore_message outside DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_send_message_in_dm_does_not_trigger() {
        // A DM run that ends with send_message should NOT trigger
        // end_conversation -- this was the false-positive bug introduced
        // by the Bug 1 fix in PR #412.
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "send_message in DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_no_tool_calls_in_dm_does_not_trigger() {
        // Empty tool_calls (degenerate LLM response) should NOT trigger
        // end_conversation -- there was no explicit ignore_message call.
        let records: Vec<alms_core::ToolCallRecord> = vec![];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "empty tool_calls in DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_non_peer_message_in_dm_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "dm:alice:bob"),
            "non-peer-message run should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_notification_context_does_not_trigger() {
        // Notification sessions have context_id = "notifications:agent_name"
        // which does NOT start with "dm:", so no false positive.
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "notifications:bob"),
            "notification session should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_job_context_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "job_some-uuid"),
            "job session should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_tool_role_result_does_not_trigger() {
        // A Tool-role record (tool result) with name "ignore_message" should
        // NOT be counted -- only Assistant-role records (the actual call)
        // paired with a successful Tool-role result.
        let records = vec![make_tool_record(
            alms_core::ToolCallRole::Tool,
            "ignore_message",
            "call_1",
            Some(r#"{"ok":true}"#),
        )];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "Tool-role ignore_message record should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_conflict_batch_does_not_trigger_end_conversation() {
        // Both send_message and ignore_message were called in the same batch.
        // Both are blocked with DM conflict errors.
        // Agent retries with just send_message and succeeds.
        // The old blocked ignore_message should NOT trigger end_conversation.
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            // First batch: conflict -- both tools blocked
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just send_message -- success
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_2",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "conflict-batch followed by clean send_message should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_conflict_batch_then_clean_ignore_triggers() {
        // Both tools called in conflict batch (both blocked), then agent
        // retries with just ignore_message and succeeds.
        // The clean ignore_message SHOULD trigger end_conversation.
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            // First batch: conflict -- both tools blocked
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just ignore_message -- success
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_2",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            should_signal_ignore(true, &records, "dm:alice:bob"),
            "conflict-batch followed by clean ignore_message SHOULD trigger end_conversation"
        );
    }

    #[test]
    fn test_ignore_without_tool_result_does_not_trigger() {
        // Assistant-role ignore_message record exists but no corresponding
        // Tool-role result -- should not trigger.
        let records = vec![make_tool_record(
            alms_core::ToolCallRole::Assistant,
            "ignore_message",
            "call_1",
            None,
        )];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "ignore_message without matching Tool result should NOT trigger end_conversation"
        );
    }

    // -----------------------------------------------------------------------
    // format_dm_ended_notification tests (#388, #429)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dm_ended_notification_ignored_no_history() {
        let msg = format_dm_ended_notification("alice", ConversationEndReason::Ignored, None);
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("alice"),
            "notification should mention the agent who ended the conversation"
        );
        assert!(
            msg.contains("chose not to reply"),
            "Ignored reason should explain the agent chose not to reply"
        );
        assert!(
            msg.contains("read_messages"),
            "fallback (no history) should hint at read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_depth_exceeded_no_history() {
        let msg = format_dm_ended_notification("bob", ConversationEndReason::DepthExceeded, None);
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("bob"),
            "notification should mention the peer agent"
        );
        assert!(
            msg.contains("maximum message depth"),
            "DepthExceeded reason should mention the depth limit"
        );
        assert!(
            msg.contains("read_messages"),
            "fallback (no history) should hint at read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_is_not_empty() {
        let msg = format_dm_ended_notification("x", ConversationEndReason::Ignored, None);
        assert!(
            msg.len() > 50,
            "notification should be a substantive message, not a stub"
        );
    }

    #[test]
    fn test_dm_ended_notification_with_history() {
        let history = "[10:00] alice: Hello Bob\n[10:01] bob: Hi Alice, what's up?";
        let msg =
            format_dm_ended_notification("alice", ConversationEndReason::Ignored, Some(history));
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("Hello Bob"),
            "notification should include conversation content"
        );
        assert!(
            msg.contains("Hi Alice"),
            "notification should include both sides of the conversation"
        );
        assert!(
            msg.contains("full conversation history"),
            "notification with history should mention it contains the transcript"
        );
        assert!(
            !msg.contains("read_messages"),
            "notification with history should NOT suggest read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_empty_history_falls_back() {
        let msg = format_dm_ended_notification("alice", ConversationEndReason::Ignored, Some(""));
        assert!(
            msg.contains("read_messages"),
            "empty history string should fall back to read_messages hint"
        );
    }

    // -----------------------------------------------------------------------
    // format_dm_conversation_history tests (#429)
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_dm_history_basic() {
        use alms_session::{Content, Message, Role};

        let messages = vec![
            Message {
                id: "1".into(),
                role: Role::User,
                content: Content::Text("Hello from alice".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            },
            Message {
                id: "2".into(),
                role: Role::User,
                content: Content::Text("Hi alice, I got your message".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "bob"})),
            },
        ];

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.contains("alice: Hello from alice"),
            "should include alice's message with sender label"
        );
        assert!(
            result.contains("bob: Hi alice"),
            "should include bob's message with sender label"
        );
    }

    #[test]
    fn test_format_dm_history_skips_empty_and_markers() {
        use alms_session::{Content, Message, Role};

        let messages = vec![
            Message {
                id: "1".into(),
                role: Role::User,
                content: Content::Text("Real message".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            },
            // Empty text (dm_ended marker body)
            Message {
                id: "2".into(),
                role: Role::User,
                content: Content::Text(String::new()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"message_type": "dm_ended"})),
            },
            // Tool call
            Message {
                id: "3".into(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "t1".into(),
                    result: serde_json::json!({"ok": true}),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
            // Synthetic notification
            Message {
                id: "4".into(),
                role: Role::System,
                content: Content::Text("synthetic marker".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"synthetic": true})),
            },
        ];

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.contains("Real message"),
            "should include the real message"
        );
        assert!(!result.contains("dm_ended"), "should skip dm_ended markers");
        assert!(!result.contains("Tool result"), "should skip tool results");
        assert!(
            !result.contains("synthetic marker"),
            "should skip synthetic markers"
        );
    }

    #[test]
    fn test_format_dm_history_empty_session() {
        let result = format_dm_conversation_history(&[]);
        assert!(
            result.is_empty(),
            "empty session should produce empty string"
        );
    }

    #[test]
    fn test_format_dm_history_truncation() {
        use alms_session::{Content, Message, Role};

        // Create enough messages to exceed DM_HISTORY_MAX_CHARS
        let messages: Vec<Message> = (0..200)
            .map(|i| Message {
                id: format!("msg_{i}"),
                role: Role::User,
                content: Content::Text(format!("Message number {i} with some padding text to make it longer and ensure truncation kicks in properly")),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            })
            .collect();

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.len() <= DM_HISTORY_MAX_CHARS,
            "truncated output should be within budget (got {} chars, max {})",
            result.len(),
            DM_HISTORY_MAX_CHARS,
        );
        assert!(
            result.contains("earlier message(s) omitted"),
            "truncated output should indicate messages were omitted"
        );
        // The last message should always be present (most recent)
        assert!(
            result.contains("Message number 199"),
            "truncated output should include the most recent message"
        );
    }

    // -----------------------------------------------------------------------
    // find_user_facing_session / is_internal_context_id tests (#428)
    // -----------------------------------------------------------------------

    #[test]
    fn test_internal_prefixes_detected() {
        for prefix in &["job_", "subagent_", "dm:", "notifications:", "episodic:"] {
            let context_id = format!("{prefix}something");
            assert!(
                is_internal_context_id(&context_id),
                "context_id '{context_id}' should be classified as internal"
            );
        }
    }

    #[test]
    fn test_user_facing_context_ids() {
        // Plain context IDs (web chat, telegram, etc.) are user-facing.
        for ctx in &["web", "default", "telegram:123", "my-custom-context"] {
            assert!(
                !is_internal_context_id(ctx),
                "context_id '{ctx}' should NOT be classified as internal"
            );
        }
    }

    #[test]
    fn test_find_user_facing_session_excludes_internal() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_id = AgentId::new();

        // Create several internal sessions
        mgr.get_or_create(agent_id, "dm:alice:bob");
        mgr.get_or_create(agent_id, "subagent_task_1");
        mgr.get_or_create(agent_id, "job_abc");
        mgr.get_or_create(agent_id, "notifications:alice");
        mgr.get_or_create(agent_id, "episodic:main");

        // No user-facing session yet
        assert!(
            find_user_facing_session(&mgr, agent_id).is_none(),
            "should return None when only internal sessions exist"
        );

        // Create a user-facing session
        let user_session = mgr.get_or_create(agent_id, "web");

        let found = find_user_facing_session(&mgr, agent_id);
        assert!(found.is_some(), "should find the user-facing session");
        assert_eq!(found.unwrap().id, user_session.id);
    }

    #[test]
    fn test_find_user_facing_session_ignores_other_agents() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        // Create a user-facing session for agent B only
        mgr.get_or_create(agent_b, "web");

        assert!(
            find_user_facing_session(&mgr, agent_a).is_none(),
            "should not return sessions belonging to a different agent"
        );
    }

    #[test]
    fn test_find_user_facing_session_no_sessions() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_id = AgentId::new();

        assert!(
            find_user_facing_session(&mgr, agent_id).is_none(),
            "should return None when no sessions exist at all"
        );
    }
}
