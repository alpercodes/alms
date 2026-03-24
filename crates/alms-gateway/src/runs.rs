//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md

use crate::api_error;
use crate::approvals::{ApprovalStore, PendingApproval};
use crate::cron_utils;
use crate::server::AppState;
use crate::sse::{RunEventStream, SseEventData, ToolInvocationId, event_channel};
use alms_core::{
    CreateRunRequest, CreateRunResponse, JobId, JobSchedule, JobStatus, Run, RunId, RunInput,
    RunStatus, RunStatusResponse, SessionId,
};
use alms_runtime::RuntimeEvent;
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
        debug!(
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
        debug!(
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
    } = params;
    // Track this run for graceful shutdown drain.
    state.run_manager.track_in_flight();

    // Early exit if already cancelled (queued-then-cancelled before execution started).
    if cancel_token.is_cancelled() {
        state
            .run_manager
            .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
            .await;
        state.run_manager.mark_run_as_cancelled(run_id);
        state.run_manager.remove_cancel_token(run_id);
        state.run_manager.remove_senders(run_id);
        state.run_manager.untrack_in_flight();
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
    let resolved = resolve_agent_config(
        agent_id,
        &state.session_manager,
        &state.agent_config,
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

    // Create a runtime event channel so we can forward tool events to SSE.
    // Clone before moving into `with_event_sender` so invoke_agent can forward
    // subagent events into the same stream.
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    let invoke_agent_tx = runtime_tx.clone();

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
            state.run_manager.untrack_in_flight();
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

    // Register invoke_agent + get_task_result tools.
    // Subagent events are forwarded into this run's SSE stream.
    // The cancel_token is passed to InvokeAgentTool so that cancelling the
    // parent run propagates to all subagents spawned during this run.
    {
        let dispatcher: std::sync::Arc<dyn alms_runtime::SubagentDispatcher> =
            state.coordinator.clone();
        let get_task_tool = alms_runtime::GetTaskResultTool::new(dispatcher.clone());
        // Separate channel for background subagent events → session stream.
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
                let sse = match &event {
                    alms_runtime::RuntimeEvent::ToolStart {
                        invocation_id,
                        tool,
                        params,
                        source_agent,
                    } => SseEventData::tool_start(
                        bg_run_id,
                        crate::sse::ToolInvocationId(*invocation_id),
                        tool,
                        params.clone(),
                        source_agent.clone(),
                    ),
                    alms_runtime::RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        source_agent,
                    } => SseEventData::tool_end(
                        bg_run_id,
                        crate::sse::ToolInvocationId(*invocation_id),
                        *ok,
                        result.clone(),
                        source_agent.clone(),
                    ),
                    alms_runtime::RuntimeEvent::ApprovalRequired { tool, .. } => {
                        warn!(
                            "Background subagent requested approval for '{}' — \
                             approvals are not supported for background subagents. \
                             The subagent will hang until timeout.",
                            tool
                        );
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

        let invoke_tool = alms_runtime::InvokeAgentTool::new(
            dispatcher,
            session_id,
            Some(run_id),
            Some(invoke_agent_tx),
        )
        .with_cancel_token(cancel_token)
        .with_background_event_tx(bg_event_tx);
        let read_session_tool =
            alms_runtime::ReadSubagentSessionTool::new(state.session_manager.clone(), session_id);
        runtime = runtime
            .with_invoke_agent(invoke_tool)
            .with_get_task_result(get_task_tool)
            .with_read_subagent_session(read_session_tool);
    }

    // Register peer messaging tools (Layer 2) when agent name is known.
    if let Some(ref name) = agent_name {
        let sender: std::sync::Arc<dyn alms_runtime::MessageSender> = state.message_bus.clone();
        let send_tool = alms_runtime::SendMessageTool::new(
            sender,
            agent_id,
            name.clone(),
            state.session_manager.clone(),
        );
        let list_tool =
            alms_runtime::ListAgentsTool::new(state.session_manager.clone(), name.clone());
        let read_tool =
            alms_runtime::ReadMessagesTool::new(state.session_manager.clone(), name.clone());
        let ignore_tool = alms_runtime::IgnoreMessageTool::new();
        runtime = runtime
            .with_send_message(send_tool)
            .with_list_agents(list_tool)
            .with_read_messages(read_tool)
            .with_ignore_message(ignore_tool);
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

    // Drop `runtime` explicitly to close `runtime_tx` and signal EOF to the
    // forwarder. Then await the forwarder so all buffered tool events are
    // forwarded before we send run_finished / run_error.
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

            // token_delta events already emitted during streaming in the agent loop
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::run_finished(run_id, true, output.usage),
                )
                .await;

            state
                .run_manager
                .mark_run_as_completed(run_id, output.response, output.usage);

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
    // Signal drain waiters that this run is done.
    state.run_manager.untrack_in_flight();
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

    // Find the agent's most recent user-facing session (exclude job_* and subagent_*).
    let all_sessions = state.session_manager.list_all();
    let user_session = all_sessions.iter().find(|s| {
        s.agent_id == agent_id
            && !s.context_id.starts_with("job_")
            && !s.context_id.starts_with("subagent_")
            && !s.context_id.starts_with("dm:")
    });

    let Some(target) = user_session else {
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
                },
            )
            .await;
        }),
    );

    run_id
}

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
            format!(
                "Use get_task_result(\"{}\") to retrieve the full result.",
                c.task_id.0
            ),
        ),
    };

    format!(
        "[Subagent notification] Background subagent {label} has {status}.\n\
         \n\
         Summary: {summary}\n\
         \n\
         {follow_up}",
        summary = c.summary,
    )
}

// ---------------------------------------------------------------------------
// RunTrigger loop (peer messaging)
// ---------------------------------------------------------------------------

/// Processes `RunTrigger` events from the `MessageBus`.
///
/// Each trigger creates a run on the target agent's session, reusing the
/// same `execute_run` path as user-initiated and notification runs.
pub(crate) async fn run_trigger_loop(
    mut rx: mpsc::UnboundedReceiver<alms_coordinator::message_bus::RunTrigger>,
    state: AppState,
) {
    use alms_coordinator::message_bus::MessageSource;

    while let Some(trigger) = rx.recv().await {
        let session_id = trigger.session_id;
        let agent_id = trigger.agent_id;
        let context_id = trigger.context_id;

        let source_label = match &trigger.source {
            MessageSource::Agent { from_name, .. } => format!("peer:{from_name}"),
            MessageSource::SubagentCompletion => "subagent".to_string(),
        };

        let is_peer = matches!(trigger.source, MessageSource::Agent { .. });

        info!(
            session_id = %session_id.0,
            agent_id = %agent_id.0,
            source = %source_label,
            "RunTrigger -> creating run"
        );

        // The message has already been persisted to the session by the
        // MessageBus. We still pass `input` to execute_run so the
        // agent loop knows what prompted this run (it reads from session
        // history, but the run record needs the input).
        enqueue_triggered_run(
            &state,
            agent_id,
            session_id,
            trigger.input,
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
            // Fall through to replay-only: the sender's rx will see
            // channel closed immediately, so stream_with_replay will
            // emit the replay events and then close.
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
    pub last_event_id: Option<u64>,
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
    let last_event_id = query.last_event_id.or_else(|| {
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
            max_tokens: 4096,
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
        assert_eq!(merged.agent_config.max_tokens, 4096);
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
        assert_eq!(merged.agent_config.max_tokens, 4096);
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
        assert_eq!(merged.agent_config.max_tokens, 4096); // unchanged
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
}
