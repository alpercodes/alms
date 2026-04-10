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
    CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus, RunStatusResponse,
    SessionId,
};
use alms_runtime::RuntimeEvent;
use alms_tools::message_sender::{ConversationEndReason, MessageSender as _};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

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
/// The DM context ID format is `dm:{first}:{second}` where the names are
/// alphabetically sorted (see [`alms_core::dm_context_id`]). The peer is
/// whichever name is NOT the current agent.
///
/// Returns `None` if the context ID does not match the expected format or
/// neither name matches `agent_name`.
///
/// Note: `split_once(':')` is safe because agent names are restricted to
/// `[a-z0-9-]` by `validate_agent_name` — colons cannot appear in names.
pub(super) fn extract_peer_from_dm_context(context_id: &str, agent_name: &str) -> Option<String> {
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

    // Spawn forwarder: converts RuntimeEvents -> SseEventData (and stores approvals).
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

    // System-triggered notification runs (subagent completions, DM-ended
    // notifications) that land on a user-facing session should NOT persist
    // the verbose notification input as a Role::User message — that would
    // show the internal LLM prompt as a "user" bubble on page reload.
    //
    // Instead, pre-persist the input as a Role::System message *without*
    // `synthetic` metadata (so get_session_messages filters it out on
    // reload — non-synthetic system messages are excluded) and then use
    // `run_on_session` to skip the default Role::User persistence in
    // `runtime.run()`.
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
        // Notification run rerouted to a user-facing session.
        // Persist the input as a non-synthetic Role::System message so:
        //  - The context builder includes it in the LLM's context window
        //  - get_session_messages filters it out (non-synthetic system
        //    messages are excluded), making it invisible on page reload
        // Then use run_on_session to skip the default Role::User
        // persistence that runtime.run() would do.
        //
        // Trade-off: the notification enters the LLM context as a
        // `system` message rather than `user`. Some models treat system
        // messages with lower priority, which could affect response
        // quality. If notification runs produce lower-quality responses,
        // consider switching to Role::User with a `hidden: true`
        // metadata flag and updating get_session_messages to filter it.
        //
        // Note: if run_on_session fails after the append below, the
        // orphaned Role::System message remains in the session. Since
        // get_session_messages filters it out, it is invisible to users
        // but still occupies tokens in the context window for future
        // runs. This is acceptable for MVP — the failure case is rare
        // and the impact is minor token waste.
        let sys_msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::System,
            content: alms_session::Content::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "type": "notification_input",
            })),
        };
        if let Err(e) = state.session_manager.append_message(session_id, sys_msg) {
            warn!("Failed to persist notification input: {e}");
        }
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
            if output.response == alms_core::MAX_ITERATIONS_SENTINEL {
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

            // -- ignore_message detection for DM conversations (#387) --
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

                                // NOTE: The sender's web-chat SSE marker is
                                // handled by the sender's self-notification
                                // run in `run_trigger_loop` (notifications.rs),
                                // which calls `notify_dm_ended_to_webchat` for
                                // every ConversationEnded trigger recipient.
                                // Calling it here as well would cause a
                                // duplicate marker for the sender. See #556.
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
