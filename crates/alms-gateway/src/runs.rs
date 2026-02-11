//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md

use crate::server::AppState;
use crate::sse::{event_channel, RunEventStream, SseEventData};
use alms_core::{CreateRunRequest, CreateRunResponse, Run, RunId, RunInput, RunStatus, RunStatusResponse};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use tracing::{error, info};

/// POST /runs - Create a new run
/// 
/// Per API spec: Returns 201 Created with { run_id, session_id, status: "queued", ts }
pub async fn create_run(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<CreateRunResponse>), (StatusCode, String)> {
    // Get existing session
    let session = match state.session_manager.get(req.session_id) {
        Ok(session) => session,
        Err(_) => return Err((StatusCode::NOT_FOUND, "Session not found".to_string())),
    };

    // Create run
    let input_text = match req.input {
        RunInput::Text { text } => text,
    };
    
    let run = Run::new(session.id, session.agent_id, input_text);
    let run_id = run.run_id;
    let session_id = run.session_id;
    let agent_id = run.agent_id.clone();

    info!("Creating run {} for session {}", run_id.0, session_id.0);

    // Store run
    state.run_manager.insert_run(run.clone());

    // Spawn background task to execute run
    let state_clone = AppState {
        session_manager: state.session_manager.clone(),
        gateway: state.gateway.clone(),
        run_manager: state.run_manager.clone(),
    };

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

/// Execute a run in background
async fn execute_run(
    state: AppState,
    run_id: RunId,
    session_id: alms_core::SessionId,
    agent_id: alms_core::AgentId,
    input: String,
) {
    // Register event channel for this run
    let (tx, _rx) = event_channel();
    state.run_manager.register_sender(run_id, tx.clone());

    // Send initial events (no "connected" in spec)
    state.run_manager.send_event(run_id, SseEventData::run_started(run_id, session_id));

    if let Some(mut run) = state.run_manager.get_run(run_id) {
        run.mark_running();
        state.run_manager.update_run(run);
    }

    // Execute via gateway
    let gateway = state.gateway.lock().await;
    let runtime = alms_runtime::AgentRuntime::new(
        agent_id,
        gateway.agent_config().clone(),
        gateway.llm().clone(),
    );
    
    // For MVP: emit response as single token delta
    let result = runtime.run(&state.session_manager, &session_id.0.to_string(), input).await;
    drop(gateway);

    // Mark completion
    match result {
        Ok(response) => {
            // Send token delta
            state.run_manager.send_event(run_id, SseEventData::token_delta(run_id, &response));
            // Send finished
            state.run_manager.send_event(run_id, SseEventData::run_finished(run_id, true));

            if let Some(mut run) = state.run_manager.get_run(run_id) {
                run.mark_completed(response);
                state.run_manager.update_run(run);
            }

            info!("Run {} completed successfully", run_id.0);
        }
        Err(e) => {
            state.run_manager.send_event(run_id, SseEventData::run_error(run_id, &e.to_string()));

            if let Some(mut run) = state.run_manager.get_run(run_id) {
                run.mark_failed(e.to_string());
                state.run_manager.update_run(run);
            }

            error!("Run {} failed: {}", run_id.0, e);
        }
    }

    // Cleanup
    state.run_manager.remove_sender(run_id);
}

/// GET /runs/{run_id} - Get run status
///
/// Per API spec: Returns { run_id, session_id, agent_id, status, started_at, ended_at, ts }
pub async fn get_run_status(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<Json<RunStatusResponse>, (StatusCode, String)> {
    let run = match state.run_manager.get_run(run_id) {
        Some(run) => run,
        None => return Err((StatusCode::NOT_FOUND, "Run not found".to_string())),
    };

    Ok(Json(RunStatusResponse::from(run)))
}

/// GET /runs/{run_id}/events - Stream events via SSE
///
/// Per API spec: Returns SSE stream with event: run_started, token_delta, run_finished, run_error
pub async fn stream_run_events(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
) -> Result<RunEventStream, (StatusCode, String)> {
    // Create event channel
    let (tx, rx) = event_channel();
    state.run_manager.register_sender(run_id, tx);

    info!("Starting event stream for run {}", run_id.0);
    Ok(RunEventStream::new(rx))
}