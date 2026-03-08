//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md

use crate::approvals::{ApprovalStore, PendingApproval};
use crate::server::AppState;
use crate::sse::{RunEventStream, SseEventData, ToolInvocationId, event_channel};
use alms_core::{
    CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus, RunStatusResponse,
    SessionId,
};
use alms_runtime::RuntimeEvent;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{error, info, instrument};

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

    let run = Run::new(session.id, session.agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id;

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    state.run_manager.insert_run(run.clone());

    let state_clone = state.clone();
    tokio::spawn(async move {
        execute_run(state_clone, run_id, session_id, agent_id, run.input).await;
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
#[instrument(level = "info", skip(state, input), fields(run_id = %run_id.0, session_id = %session_id.0))]
async fn execute_run(
    state: AppState,
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
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

    if let Some(mut run) = state.run_manager.get_run(run_id) {
        run.mark_running();
        state.run_manager.update_run(run);
    }

    // Build runtime — drop gateway lock before running to avoid blocking other requests
    let (agent_config, llm) = {
        let gateway = state.gateway.lock().await;
        (gateway.agent_config().clone(), gateway.llm().clone())
    };

    // Create a runtime event channel so we can forward tool events to SSE
    let (runtime_tx, runtime_rx) = mpsc::unbounded_channel::<RuntimeEvent>();

    let mut runtime = alms_runtime::AgentRuntime::new(agent_id, agent_config, llm)
        .with_event_sender(runtime_tx)
        .with_run_id(run_id);

    // Attach workspace if configured — registers the workspace_write tool for this run
    if let Some(ref workspace_dir) = state.workspace_dir {
        let workspace = alms_runtime::AgentWorkspace::new(workspace_dir, agent_id);
        runtime = runtime.with_workspace(workspace);
    }

    // Spawn forwarder: converts RuntimeEvents → SseEventData (and stores approvals)
    let forwarder_state = state.clone();
    tokio::spawn(forward_runtime_events(
        runtime_rx,
        run_id,
        session_id,
        forwarder_state.run_manager.clone(),
        forwarder_state.approval_store.clone(),
    ));

    let result = runtime
        .run(&state.session_manager, &session_id.0.to_string(), input)
        .await;

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

            if let Some(mut run) = state.run_manager.get_run(run_id) {
                run.mark_completed(output.response, output.usage);
                state.run_manager.update_run(run);
            }

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

            if let Some(mut run) = state.run_manager.get_run(run_id) {
                run.mark_failed(e.to_string());
                state.run_manager.update_run(run);
            }

            error!("Run {} failed: {}", run_id.0, e);
        }
    }

    state.run_manager.remove_sender(run_id);
    // Clean up any stale pending approvals for this run
    state.approval_store.clear_for_run(run_id);
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
        execute_run(state_clone, run_id, session_id, agent_id, run.input).await;
    });

    Ok(RunEventStream::stream(rx))
}
