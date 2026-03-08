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
use serde::Deserialize;
use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{error, info, instrument, warn};

/// Per-run overrides that can be sent by the client to customise a single run.
#[derive(Debug, Default)]
struct RunOverrides {
    model:       Option<String>,
    temperature: Option<f32>,
    max_tokens:  Option<u32>,
    posture:     Option<String>,
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
        model:       req.model.clone(),
        temperature: req.temperature,
        max_tokens:  req.max_tokens,
        posture:     req.posture.clone(),
    };
    let run = Run::new(session.id, session.agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    state.run_manager.insert_run(run.clone());

    let state_clone = state.clone();
    tokio::spawn(async move {
        execute_run(state_clone, run_id, session_id, agent_id, run.input, overrides).await;
    });

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
async fn execute_run(
    state: AppState,
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
    overrides: RunOverrides,
) {
    // Events are persisted to the event log regardless of whether an SSE
    // client is connected. The SSE client registers its own sender when it
    // calls GET /runs/{id}/events (or via the legacy stream endpoint which
    // pre-registers before spawning). No placeholder sender here — avoids
    // overwriting a real sender from stream_run_legacy.

    state
        .run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::run_started(run_id, session_id),
        )
        .await;

    state.run_manager.mark_run_as_running(run_id);

    // Build runtime — drop gateway lock before running to avoid blocking other requests
    let (agent_config, llm) = {
        let gateway = state.gateway.lock().await;
        let llm = gateway.llm().clone();
        let llm = if let Some(model) = overrides.model {
            info!("Run {} using model override: {}", run_id.0, model);
            llm.with_model(model)
        } else {
            llm
        };
        (gateway.agent_config().clone(), llm)
    };

    // Create a runtime event channel so we can forward tool events to SSE
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();

    // Apply per-run overrides (temperature, max_tokens, posture).
    let agent_config = {
        let mut cfg = agent_config;
        if let Some(t) = overrides.temperature { cfg.temperature = t; }
        if let Some(m) = overrides.max_tokens  { cfg.max_tokens = m; }
        if let Some(ref p) = overrides.posture {
            cfg.posture = match p.as_str() {
                "guarded" => alms_runtime::Posture::Guarded,
                _ => alms_runtime::Posture::FullControl,
            };
        }
        cfg
    };

    // Override system prompt with bootstrap prompt for first-time agents
    let agent_config = if let Some(ref workspace_dir) = state.workspace_dir {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, agent_id);
        if workspace.needs_bootstrap() {
            info!("Agent {} has no personality.md — using bootstrap prompt", agent_id.0);
            let mut cfg = agent_config;
            cfg.system_prompt = alms_runtime::AgentWorkspace::bootstrap_prompt().to_string();
            cfg
        } else {
            agent_config
        }
    } else {
        agent_config
    };

    let mut runtime = alms_runtime::AgentRuntime::new(agent_id, agent_config, llm)
        .with_event_sender(runtime_tx)
        .with_run_id(run_id);

    // Attach workspace if configured — registers the workspace_write tool for this run
    if let Some(ref workspace_dir) = state.workspace_dir {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, agent_id);
        runtime = runtime.with_workspace(workspace);
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
        .run(&state.session_manager, &session_id.0.to_string(), input)
        .await;

    // Drop `runtime` explicitly to close `runtime_tx` and signal EOF to the
    // forwarder. Then await the forwarder so all buffered tool events are
    // forwarded before we send run_finished / run_error.
    drop(runtime);
    forwarder_handle.await.ok();

    match result {
        Ok(output) => {
            state
                .run_manager
                .send_event(
                    run_id,
                    session_id,
                    SseEventData::token_delta(run_id, &output.response),
                )
                .await;
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

    state.run_manager.remove_sender(run_id);
    // Clean up any stale pending approvals for this run
    state.approval_store.clear_for_run(run_id);
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
        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = fire_job_run(state_clone, job_id).await {
                error!("Job {} run dispatch failed: {}", job_id, e);
            }
        });
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
    execute_run(state.clone(), run_id, session_id, job.agent_id, run.input, RunOverrides::default()).await;

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

    let (tx, rx) = event_channel();
    state.run_manager.register_sender(run_id, tx);

    Ok(RunEventStream::stream_with_replay(rx, replay_events))
}

/// POST /agent/run/stream - Legacy compatibility endpoint
#[instrument(level = "info", skip(state, req))]
pub async fn stream_run_legacy(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
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

    let run = Run::new(session.id, session.agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;

    info!(
        "Creating run {} for session {} (legacy /agent/run/stream)",
        run_id.0, session_id.0
    );

    state.run_manager.insert_run(run.clone());

    // Register SSE channel before spawning so early events aren't missed
    let (tx, rx) = event_channel();
    state.run_manager.register_sender(run_id, tx);

    let state_clone = state.clone();
    tokio::spawn(async move {
        execute_run(state_clone, run_id, session_id, agent_id, run.input, RunOverrides::default()).await;
    });

    Ok(RunEventStream::stream(rx))
}
