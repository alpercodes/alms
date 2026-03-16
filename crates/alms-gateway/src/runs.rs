//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md

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
use tracing::{error, info, instrument, warn};

/// Per-run overrides that can be sent by the client to customise a single run.
#[derive(Debug, Default)]
struct RunOverrides {
    model: Option<String>,
    max_tokens: Option<u32>,
    posture: Option<String>,
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
    if let Some(record) = agent_record {
        if let Some(ref sp) = record.system_prompt {
            cfg.system_prompt = sp.clone();
        }
        if let Some(ref p) = record.posture {
            match p.as_str() {
                "guarded" => cfg.posture = alms_runtime::Posture::Guarded,
                "full_control" => cfg.posture = alms_runtime::Posture::FullControl,
                _ => {} // unknown posture — keep server default
            }
        }
    }

    // ── Per-run overrides (highest precedence) ──
    if let Some(m) = overrides.max_tokens.filter(|&m| m > 0) {
        cfg.max_tokens = m;
    }
    if let Some(ref p) = overrides.posture {
        match p.as_str() {
            "guarded" => cfg.posture = alms_runtime::Posture::Guarded,
            "full_control" => cfg.posture = alms_runtime::Posture::FullControl,
            _ => {} // unknown posture — keep current
        }
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

/// POST /runs/{run_id}/cancel — cancel a running or queued run.
#[instrument(level = "info", skip(state), fields(run_id = %run_id.0))]
pub async fn cancel_run(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let run = state.run_manager.get_run(run_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "Run not found" }
            })),
        )
    })?;

    match run.status {
        RunStatus::Queued | RunStatus::Running => {}
        _ => {
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": { "code": "ALREADY_FINISHED", "message": "Run already finished" }
                })),
            ));
        }
    }

    let found = state.run_manager.cancel_run(run_id);
    if !found {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": { "code": "ALREADY_FINISHED", "message": "Run already finished" }
            })),
        ));
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
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": { "code": "NOT_FOUND", "message": "Session not found" }
                })),
            ));
        }
    };

    let input_text = match req.input {
        RunInput::Text { text } => text,
    };

    let overrides = RunOverrides {
        model: req.model.clone(),
        max_tokens: req.max_tokens,
        posture: req.posture.clone(),
    };
    let run = Run::new(session.id, session.agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;
    let context_id = session.context_id.clone();

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    // Reject new runs during shutdown.
    if state.shutdown_token.is_cancelled() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": { "code": "SHUTTING_DOWN", "message": "Server is shutting down" }
            })),
        ));
    }

    state.run_manager.insert_run(run.clone());

    // Create per-run cancellation token BEFORE enqueue so cancelling a
    // queued-but-not-yet-started run works.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let state_clone = state.clone();
    state.session_queue.enqueue(
        session_id,
        Box::pin(async move {
            execute_run(
                state_clone,
                run_id,
                session_id,
                agent_id,
                run.input,
                overrides,
                context_id,
                cancel_token,
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
#[instrument(level = "info", skip(state, input, overrides), fields(run_id = %run_id.0, session_id = %session_id.0))]
#[allow(clippy::too_many_arguments)]
async fn execute_run(
    state: AppState,
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
    overrides: RunOverrides,
    context_id: String,
    cancel_token: CancellationToken,
) {
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

    // Look up per-agent config overrides from the agent registry.
    // Errors are absorbed — agent lookup failure should not block the run.
    let agent_record =
        state
            .session_manager
            .store()
            .and_then(|store| match store.load_agent_by_id(agent_id) {
                Ok(record) => record,
                Err(e) => {
                    warn!(
                        "Failed to load agent record for {}, using server defaults: {}",
                        agent_id, e
                    );
                    None
                }
            });

    // Agent name for workspace path resolution (name-based, not UUID-based).
    let agent_name = agent_record.as_ref().map(|r| r.name.clone());

    // Use AppState snapshots — no gateway lock needed.
    let merged = apply_overrides(
        state.agent_config.clone(),
        agent_record.as_ref(),
        &overrides,
    );
    let agent_config = merged.agent_config;

    let llm = {
        let llm = state.llm.clone();
        if let Some(model) = merged.model_override {
            info!("Run {} using model override: {}", run_id.0, model);
            llm.with_model(model)
        } else {
            llm
        }
    };

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
    .with_cancel_token(cancel_token);

    // Attach workspace if configured — registers the workspace_write tool for this run
    if let (Some(workspace_dir), Some(name)) = (&state.workspace_dir, &agent_name) {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, name);
        runtime = runtime.with_workspace(workspace);
    }

    // Register invoke_agent + get_task_result tools.
    // Subagent events are forwarded into this run's SSE stream.
    {
        let dispatcher: std::sync::Arc<dyn alms_runtime::SubagentDispatcher> =
            state.coordinator.clone();
        let get_task_tool = alms_runtime::GetTaskResultTool::new(dispatcher.clone());
        let invoke_tool = alms_runtime::InvokeAgentTool::new(
            dispatcher,
            session_id,
            Some(run_id),
            Some(invoke_agent_tx),
        );
        let read_session_tool =
            alms_runtime::ReadSubagentSessionTool::new(state.session_manager.clone(), session_id);
        runtime = runtime
            .with_invoke_agent(invoke_tool)
            .with_get_task_result(get_task_tool)
            .with_read_subagent_session(read_session_tool);
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

    let result = runtime
        .run(&state.session_manager, &context_id, input)
        .await;

    // Drop `runtime` explicitly to close `runtime_tx` and signal EOF to the
    // forwarder. Then await the forwarder so all buffered tool events are
    // forwarded before we send run_finished / run_error.
    drop(runtime);
    forwarder_handle.await.ok();

    match result {
        Ok(output) => {
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
        let context_id = format!("job_{}", job_id.0);
        let session = state
            .session_manager
            .get_or_create(job.agent_id, &context_id);
        let state_clone = state.clone();
        state.session_queue.enqueue(
            session.id,
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
    info!("Job fired → run {}", run_id.0);

    // Execute the run (awaits completion; errors are handled inside execute_run).
    // Scheduled jobs use a fresh token — cancellation is via job-level cancel, not run-level.
    let cancel_token = CancellationToken::new();
    execute_run(
        state.clone(),
        run_id,
        session_id,
        job.agent_id,
        run.input,
        RunOverrides::default(),
        context_id,
        cancel_token,
    )
    .await;

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
            RuntimeEvent::TokenDelta { delta } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::token_delta(run_id, &delta),
                    )
                    .await;
            }
            RuntimeEvent::ToolStart {
                invocation_id,
                tool,
                params,
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
                        ),
                    )
                    .await;
            }
            RuntimeEvent::ToolEnd {
                invocation_id,
                ok,
                result,
            } => {
                run_manager
                    .send_event(
                        run_id,
                        session_id,
                        SseEventData::tool_end(run_id, ToolInvocationId(invocation_id), ok, result),
                    )
                    .await;
            }
            RuntimeEvent::ApprovalRequired {
                approval_id,
                tool,
                params,
                decision_tx,
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
        Some(run) => Ok(Json(RunStatusResponse::from(run))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "Run not found" }
            })),
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

    // Register the live channel BEFORE snapshotting the event log.
    // This closes the race where events produced between snapshot and
    // registration would be lost. The overlap (events in both replay
    // and live channel) is deduplicated by stream_with_replay.
    let (tx, rx) = event_channel();
    state.run_manager.register_sender(run_id, tx);

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
            "Replaying {} events for run {}",
            replay_events.len(),
            run_id.0
        );
    }

    Ok(RunEventStream::stream_with_replay(rx, replay_events))
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

    fn test_agent(
        model: Option<&str>,
        system_prompt: Option<&str>,
        posture: Option<&str>,
    ) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "test-agent".into(),
            description: String::new(),
            model: model.map(String::from),
            system_prompt: system_prompt.map(String::from),
            posture: posture.map(String::from),
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
        let agent = test_agent(Some("custom-model"), Some("agent prompt"), Some("guarded"));
        let merged = apply_overrides(base_config(), Some(&agent), &RunOverrides::default());
        assert_eq!(merged.agent_config.system_prompt, "agent prompt");
        assert!(matches!(merged.agent_config.posture, Posture::Guarded));
        assert_eq!(merged.model_override.as_deref(), Some("custom-model"));
        // max_tokens not overridden by agent
        assert_eq!(merged.agent_config.max_tokens, 4096);
    }

    #[test]
    fn test_per_run_overrides_beat_per_agent() {
        let agent = test_agent(Some("agent-model"), Some("agent prompt"), Some("guarded"));
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(8192),
            posture: Some("full_control".into()),
        };
        let merged = apply_overrides(base_config(), Some(&agent), &overrides);
        // Per-run model wins over per-agent
        assert_eq!(merged.model_override.as_deref(), Some("run-model"));
        // Per-run posture wins over per-agent
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
        // Per-run max_tokens applied
        assert_eq!(merged.agent_config.max_tokens, 8192);
        // system_prompt still comes from agent (no per-run system_prompt override)
        assert_eq!(merged.agent_config.system_prompt, "agent prompt");
    }

    #[test]
    fn test_per_run_only() {
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(256),
            posture: Some("guarded".into()),
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
        let agent = test_agent(None, None, Some("yolo"));
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
}
