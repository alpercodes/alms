//! SSE event streaming — per-run, per-session, and per-agent event streams.

use crate::api_error;
use crate::server::AppState;
use crate::sse::{RunEventStream, SseEventData, event_channel};
use alms_core::{AgentId, RunId, RunStatus, SessionId};
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
        run.status(),
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
            run.status(),
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
                    r.status(),
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

/// Register a new live subscriber on a session's SSE stream and bring it up
/// to date on in-flight subagent status (#1189 follow-up).
///
/// The `subagent_activity` signal that drives the Subagent status bar is
/// ephemeral (never persisted/replayed) AND deduplicated at the source to one
/// emission per activity transition — so it only ever reaches the subscribers
/// attached at the instant of the transition. A client that attaches
/// mid-phase (page reload, session switch back from the subagent view, a
/// second tab, an EventSource reconnect) would otherwise render every
/// in-flight subagent chip as "Starting…" until the subagent's NEXT kind
/// transition, which during a long reasoning/writing phase can be minutes
/// away — the exact live symptom this fixes. So after registering the sender
/// we replay the coordinator's per-subagent activity snapshot as synthetic
/// `subagent_activity` events on the new channel.
///
/// Snapshot events carry no `event_id` (they are live-channel events, never
/// logged), so they pass the replay dedup filter in `stream_with_replay` and
/// arrive after any persisted-event replay — the same ordering a genuinely
/// live signal would have.
///
/// Shared by the `stream_session_events` handler and the reattach regression
/// test so the test exercises the identical attach path the endpoint runs.
pub(crate) fn attach_session_stream(
    state: &AppState,
    session_id: SessionId,
) -> tokio::sync::mpsc::UnboundedReceiver<SseEventData> {
    let (tx, rx) = event_channel();
    state
        .run_manager
        .register_session_sender(session_id, tx.clone());
    for snap in state.coordinator.subagent_activity_snapshot(session_id) {
        // The frontend routes the signal purely by `source_agent`; `run_id`
        // is carried for wire-shape parity with live signals (which use the
        // parent's run id) and falls back to the nil UUID when the spawn
        // predates run registration.
        let run_id = snap.parent_run_id.unwrap_or(RunId(uuid::Uuid::nil()));
        let _ = tx.send(SseEventData::subagent_activity(
            run_id,
            &snap.kind,
            snap.tool,
            snap.tool_invocation_id,
            snap.parent_tool_invocation_id,
            Some(snap.label),
        ));
    }
    rx
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

    let rx = attach_session_stream(&state, session_id);

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

/// GET /agents/{agent_id}/events — agent-scoped SSE feed (#856).
///
/// Currently carries `session_activity_started` / `session_activity_ended`
/// events for runs belonging to the given agent, across **all** sessions
/// (regular chat, jobs, notifications, DMs). Backs the web UI's session
/// sidebar so it can light up the "active" indicator on any session —
/// not just the currently-viewed one. Future per-agent events (e.g. DM
/// activity in #886) will share this same feed.
///
/// Filtering is performed by the broadcast layer
/// (`RunManager::send_agent_event`): each agent gets its own sender map
/// entry, so subscribers to one agent's feed never see events for any
/// other agent.
///
/// Supports `Last-Event-Id` header for browser auto-reconnect plus
/// `?last_event_id=<n>` query parameter for the initial connection.
/// IDs are scoped to the agent's event log (separate counter from the
/// per-run / per-session logs).
///
/// Returns `404 NOT_FOUND` when the `agent_id` does not resolve to a
/// known agent in the registry (#887). The check happens **before** any
/// sender is registered, so unknown-agent connections never insert an
/// orphan entry into the in-memory `agent_senders` map. Without this
/// guard, a misbehaving client could slowly grow that map by repeatedly
/// connecting with random UUIDs — the entries are only pruned on
/// `send_agent_event` fanout, which never fires for an agent that never
/// emits, so disconnected senders accumulate until process restart.
///
/// When no SQLite store is configured (test fixtures or store-less
/// deployments), agent existence cannot be verified and the request is
/// allowed through. In that mode the registry concept does not exist,
/// so there is no "unknown agent" to leak against.
#[instrument(level = "info", skip(state, headers, query), fields(agent_id = %agent_id.0))]
pub async fn stream_agent_events(
    State(state): State<AppState>,
    Path(agent_id): Path<AgentId>,
    headers: HeaderMap,
    Query(query): Query<SessionEventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    // Validate the agent exists *before* registering a sender so
    // unknown-agent UUIDs never insert orphan entries into
    // `agent_senders` (#887). When no store is configured (test
    // fixtures), skip the check and proceed.
    if let Some(store) = state.session_manager.store() {
        match store.load_agent_by_id(agent_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(api_error(
                    StatusCode::NOT_FOUND,
                    "NOT_FOUND",
                    format!("Agent not found: {}", agent_id.0),
                ));
            }
            Err(e) => {
                return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e));
            }
        }
    }

    // Query parameter takes precedence over header (the header is only
    // sent by the browser on automatic reconnects).
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
    state.run_manager.register_agent_sender(agent_id, tx);

    let logged_events = state.run_manager.agent_events_from(agent_id, from_id).await;
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
            "Replaying {} agent-scoped events for agent {}",
            replay_events.len(),
            agent_id.0
        );
    }

    Ok(RunEventStream::stream_with_replay(rx, replay_events).into_response())
}

/// GET /events/session-activity — GLOBAL cross-agent session-activity feed
/// (#1211).
///
/// Carries `session_activity_started` / `session_activity_ended` for runs
/// across **every** agent's sessions. The web UI sidebar surfaces sessions
/// owned by agents OTHER than the currently-active one — the cross-agent
/// Jobs / Direct-messages / Notifications sections — so a single per-agent
/// feed (`GET /agents/{id}/events`) cannot light their active-run dot: a
/// run on another agent's session never reaches the active agent's feed.
/// This global feed closes that gap; the frontend subscribes to it (instead
/// of the per-agent feed) to drive `bgRuns` and the sidebar's blinking dot.
///
/// Backed by a DEDICATED global sender list + event log (separate from the
/// per-agent maps, so no operator-supplied agent id can collide with it and
/// leak activity across the per-agent isolation boundary), reusing the
/// agent event-log machinery so `Last-Event-Id` replay and graceful
/// shutdown work identically. There is no per-agent path parameter and
/// hence no registry existence check — the feed is global and any
/// authenticated client may subscribe.
///
/// Supports `Last-Event-Id` header for browser auto-reconnect plus
/// `?last_event_id=<n>` query parameter for the initial connection.
#[instrument(level = "info", skip(state, headers, query))]
pub async fn stream_session_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SessionEventsQuery>,
) -> impl IntoResponse {
    // Query parameter takes precedence over header (the header is only
    // sent by the browser on automatic reconnects).
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

    let subscription = state.run_manager.subscribe_activity();

    let logged_events = state.run_manager.activity_events_from(from_id).await;
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
            "Replaying {} global session-activity events",
            replay_events.len()
        );
    }

    RunEventStream::stream_with_replay_source(subscription, replay_events).into_response()
}
