//! SSE event streaming — per-run and per-session event streams.

use crate::api_error;
use crate::server::AppState;
use crate::sse::{RunEventStream, SseEventData, event_channel};
use alms_core::{RunId, RunStatus, SessionId};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use tracing::{info, instrument, warn};

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
    /// (token_delta, status) use `ephemeral-N` as their `id` field.  If the
    /// last event the browser saw was ephemeral, the client may send an
    /// `ephemeral-N` value here.  Using `String` prevents Axum from rejecting
    /// the request with a 422 deserialization error, which would break SSE
    /// reconnection and leave the run appearing stuck (see #465 follow-up).
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
    // Both sources may contain non-numeric values (`ephemeral-N` from
    // ephemeral events), so we parse with `.ok()` to silently ignore
    // unparseable values and fall back to replaying all events (from_id=0).
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
