// SPDX-License-Identifier: Apache-2.0

//! In-memory SSE event log for replay during current process lifetime (gateway-local).
//!
//! Events are stored in `Arc<RwLock<Vec<LoggedEvent>>>` and are lost on
//! process restart.  This supports `Last-Event-ID` reconnect within a
//! single gateway lifetime but does **not** provide cross-restart durability.

use alms_core::{AgentId, RunId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub event_id: u64,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub event_type: String,
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

/// Replay snapshot plus the retained cursor window it came from.
#[derive(Debug, Clone)]
pub struct ReplayWindow {
    pub events: Vec<LoggedEvent>,
    pub retained_from: Option<u64>,
    pub newest: Option<u64>,
    /// True when the supplied cursor cannot be satisfied from this log.
    pub replay_gap: bool,
}

#[derive(Debug, Default, Clone)]
pub struct EventLog {
    events: Arc<RwLock<Vec<LoggedEvent>>>,
    next_id: Arc<RwLock<u64>>,
}

impl EventLog {
    pub fn new() -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }

    /// Atomically mint the next event id, build the event from it, and append
    /// it to the log — all within a single critical section.
    ///
    /// Minting the id and pushing the event used to be two separate lock
    /// acquisitions, which let a concurrent task interleave between them and
    /// tear the invariant that id-order equals append-order (an event could be
    /// assigned a lower id but land later in the log). Holding the `next_id`
    /// write lock across both the mint and the push closes that window.
    ///
    /// **Lock ordering (deadlock-free):** `next_id` is acquired *before*
    /// `events`, the order every caller already used, so holding both in one
    /// scope cannot deadlock against any other path. Holding `next_id` across
    /// the push serializes minting against itself, giving id-order ==
    /// append-order.
    ///
    /// When `max_len` is `Some(n)`, the log is trimmed to its most recent `n`
    /// events after the push (the per-session / per-agent bound).
    async fn mint_and_push(
        &self,
        build: impl FnOnce(u64) -> LoggedEvent,
        max_len: Option<usize>,
    ) -> u64 {
        let mut next = self.next_id.write().await;
        let event_id = *next;
        *next += 1;

        let event = build(event_id);

        let mut events = self.events.write().await;
        events.push(event);
        if let Some(max) = max_len
            && events.len() > max
        {
            let drain = events.len() - max;
            events.drain(..drain);
        }

        event_id
    }

    pub async fn events_from(&self, from_id: u64) -> Vec<LoggedEvent> {
        let events = self.events.read().await;
        events
            .iter()
            .filter(|e| e.event_id >= from_id)
            .cloned()
            .collect()
    }

    /// Snapshot the retained window and determine whether a client cursor is
    /// still replayable. A cursor beyond the newest id also signals a gap
    /// because it is the observable shape of a gateway restart when IDs reset.
    pub async fn replay_window(&self, last_event_id: Option<u64>) -> ReplayWindow {
        let events = self.events.read().await;
        let retained_from = events.first().map(|e| e.event_id);
        let newest = events.last().map(|e| e.event_id);
        let replay_gap = last_event_id.is_some_and(|cursor| match (retained_from, newest) {
            (Some(floor), Some(high)) => cursor.saturating_add(1) < floor || cursor > high,
            _ => cursor > 0,
        });
        let from_id = last_event_id.map(|id| id.saturating_add(1)).unwrap_or(0);
        let events = events
            .iter()
            .filter(|e| e.event_id >= from_id)
            .cloned()
            .collect();
        ReplayWindow {
            events,
            retained_from,
            newest,
            replay_gap,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct EventLogManager {
    logs: Arc<RwLock<HashMap<RunId, EventLog>>>,
}

impl EventLogManager {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_or_create(&self, run_id: RunId) -> EventLog {
        let mut logs = self.logs.write().await;
        logs.get(&run_id).cloned().unwrap_or_else(|| {
            let log = EventLog::new();
            logs.insert(run_id, log.clone());
            log
        })
    }

    pub async fn log_event(
        &self,
        run_id: RunId,
        session_id: SessionId,
        event_type: &str,
        data: serde_json::Value,
    ) -> u64 {
        let log = self.get_or_create(run_id).await;
        // Mint the id and append atomically (see [`EventLog::mint_and_push`]).
        // Per-run logs are unbounded.
        log.mint_and_push(
            |event_id| LoggedEvent {
                event_id,
                run_id,
                session_id,
                event_type: event_type.to_string(),
                data,
                ts: Utc::now(),
            },
            None,
        )
        .await
    }

    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&run_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
        }
    }

    pub async fn replay_window(&self, run_id: RunId, last_event_id: Option<u64>) -> ReplayWindow {
        let logs = self.logs.read().await;
        match logs.get(&run_id) {
            Some(log) => log.replay_window(last_event_id).await,
            None => ReplayWindow {
                events: Vec::new(),
                retained_from: None,
                newest: None,
                replay_gap: last_event_id.is_some_and(|id| id > 0),
            },
        }
    }
}

/// Maximum events retained per session. Older events are discarded.
const SESSION_EVENT_LOG_MAX: usize = 5000;

/// Session-level event log — stores events across all runs in a session.
///
/// Uses its own monotonic event ID counter (separate from per-run IDs)
/// so that `Last-Event-ID` reconnect works correctly at the session level.
/// Automatically trims to `SESSION_EVENT_LOG_MAX` events to prevent
/// unbounded memory growth across long-lived sessions.
#[derive(Debug, Default, Clone)]
pub struct SessionEventLogManager {
    logs: Arc<RwLock<HashMap<SessionId, EventLog>>>,
}

impl SessionEventLogManager {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_or_create(&self, session_id: SessionId) -> EventLog {
        let mut logs = self.logs.write().await;
        logs.get(&session_id).cloned().unwrap_or_else(|| {
            let log = EventLog::new();
            logs.insert(session_id, log.clone());
            log
        })
    }

    pub async fn log_event(
        &self,
        session_id: SessionId,
        run_id: RunId,
        event_type: &str,
        data: serde_json::Value,
    ) -> u64 {
        let log = self.get_or_create(session_id).await;
        // Mint the id, append, and trim atomically (see
        // [`EventLog::mint_and_push`]).
        log.mint_and_push(
            |event_id| LoggedEvent {
                event_id,
                run_id,
                session_id,
                event_type: event_type.to_string(),
                data,
                ts: Utc::now(),
            },
            Some(SESSION_EVENT_LOG_MAX),
        )
        .await
    }

    pub async fn events_from(&self, session_id: SessionId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&session_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
        }
    }

    pub async fn replay_window(
        &self,
        session_id: SessionId,
        last_event_id: Option<u64>,
    ) -> ReplayWindow {
        let logs = self.logs.read().await;
        match logs.get(&session_id) {
            Some(log) => log.replay_window(last_event_id).await,
            None => ReplayWindow {
                events: Vec::new(),
                retained_from: None,
                newest: None,
                replay_gap: last_event_id.is_some_and(|id| id > 0),
            },
        }
    }

    /// Return the highest event ID for a session, or `None` if no events exist.
    ///
    /// Used by the REST messages endpoint to tell the client the current
    /// high-water mark so it can open an SSE stream with
    /// `?last_event_id=<n>` and skip replay of already-loaded history.
    pub async fn latest_event_id(&self, session_id: SessionId) -> Option<u64> {
        let logs = self.logs.read().await;
        let log = logs.get(&session_id)?;
        let events = log.events.read().await;
        events.last().map(|e| e.event_id)
    }
}

/// Maximum events retained per agent. Older events are discarded.
const AGENT_EVENT_LOG_MAX: usize = 1000;

/// Agent-level event log — stores activity events across all sessions
/// belonging to a single agent (#856).
///
/// Backs the `GET /agents/{agent_id}/events` SSE feed, which currently
/// carries `session_activity_started` / `session_activity_ended` events
/// (and will carry future per-agent events such as DM activity in #886).
/// Uses its own monotonic event ID counter (separate from per-run /
/// per-session IDs) so `Last-Event-ID` reconnect works independently.
/// Trimmed to [`AGENT_EVENT_LOG_MAX`] to bound memory.
#[derive(Debug, Default, Clone)]
pub struct AgentEventLogManager {
    logs: Arc<RwLock<HashMap<AgentId, EventLog>>>,
}

impl AgentEventLogManager {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_or_create(&self, agent_id: AgentId) -> EventLog {
        let mut logs = self.logs.write().await;
        logs.get(&agent_id).cloned().unwrap_or_else(|| {
            let log = EventLog::new();
            logs.insert(agent_id, log.clone());
            log
        })
    }

    pub async fn log_event(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        session_id: SessionId,
        event_type: &str,
        data: serde_json::Value,
    ) -> u64 {
        let log = self.get_or_create(agent_id).await;
        // Mint the id, append, and trim atomically (see
        // [`EventLog::mint_and_push`]).
        log.mint_and_push(
            |event_id| LoggedEvent {
                event_id,
                run_id,
                session_id,
                event_type: event_type.to_string(),
                data,
                ts: Utc::now(),
            },
            Some(AGENT_EVENT_LOG_MAX),
        )
        .await
    }

    pub async fn events_from(&self, agent_id: AgentId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&agent_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
        }
    }

    pub async fn replay_window(
        &self,
        agent_id: AgentId,
        last_event_id: Option<u64>,
    ) -> ReplayWindow {
        let logs = self.logs.read().await;
        match logs.get(&agent_id) {
            Some(log) => log.replay_window(last_event_id).await,
            None => ReplayWindow {
                events: Vec::new(),
                retained_from: None,
                newest: None,
                replay_gap: last_event_id.is_some_and(|id| id > 0),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{RunId, SessionId};
    use uuid::Uuid;

    fn test_session_id() -> SessionId {
        SessionId(Uuid::new_v4())
    }

    fn test_run_id() -> RunId {
        RunId(Uuid::new_v4())
    }

    async fn append_bounded(log: &EventLog, max: usize) -> u64 {
        let run_id = test_run_id();
        let session_id = test_session_id();
        log.mint_and_push(
            |event_id| LoggedEvent {
                event_id,
                run_id,
                session_id,
                event_type: "test".into(),
                data: serde_json::json!({}),
                ts: Utc::now(),
            },
            Some(max),
        )
        .await
    }

    #[tokio::test]
    async fn replay_window_flags_cursor_older_than_retained_floor() {
        let log = EventLog::new();
        for _ in 0..5 {
            append_bounded(&log, 3).await;
        }

        let window = log.replay_window(Some(1)).await;
        assert_eq!(window.retained_from, Some(3));
        assert_eq!(window.newest, Some(5));
        assert!(window.replay_gap);
        assert_eq!(
            window.events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }

    #[tokio::test]
    async fn replay_window_flags_cursor_from_prior_log_epoch() {
        let restarted_log = EventLog::new();
        append_bounded(&restarted_log, 3).await;

        let window = restarted_log.replay_window(Some(900)).await;
        assert_eq!(window.retained_from, Some(1));
        assert_eq!(window.newest, Some(1));
        assert!(window.replay_gap);
        assert!(window.events.is_empty());
    }

    #[tokio::test]
    async fn replay_window_accepts_cursor_immediately_before_floor() {
        let log = EventLog::new();
        for _ in 0..5 {
            append_bounded(&log, 3).await;
        }

        let window = log.replay_window(Some(2)).await;
        assert!(!window.replay_gap);
        assert_eq!(window.events.len(), 3);
    }

    #[tokio::test]
    async fn latest_event_id_empty_session() {
        let mgr = SessionEventLogManager::new();
        let sid = test_session_id();
        assert_eq!(mgr.latest_event_id(sid).await, None);
    }

    #[tokio::test]
    async fn latest_event_id_after_single_event() {
        let mgr = SessionEventLogManager::new();
        let sid = test_session_id();
        let rid = test_run_id();

        let id = mgr
            .log_event(sid, rid, "token_delta", serde_json::json!({"delta": "hi"}))
            .await;

        assert_eq!(mgr.latest_event_id(sid).await, Some(id));
    }

    #[tokio::test]
    async fn latest_event_id_returns_highest_after_multiple_events() {
        let mgr = SessionEventLogManager::new();
        let sid = test_session_id();
        let rid = test_run_id();

        mgr.log_event(sid, rid, "run_started", serde_json::json!({}))
            .await;
        mgr.log_event(sid, rid, "token_delta", serde_json::json!({"delta": "a"}))
            .await;
        let last = mgr
            .log_event(sid, rid, "run_finished", serde_json::json!({}))
            .await;

        assert_eq!(mgr.latest_event_id(sid).await, Some(last));
    }

    #[tokio::test]
    async fn latest_event_id_independent_across_sessions() {
        let mgr = SessionEventLogManager::new();
        let sid1 = test_session_id();
        let sid2 = test_session_id();
        let rid = test_run_id();

        let id1 = mgr
            .log_event(sid1, rid, "token_delta", serde_json::json!({"delta": "x"}))
            .await;

        // sid2 has no events
        assert_eq!(mgr.latest_event_id(sid1).await, Some(id1));
        assert_eq!(mgr.latest_event_id(sid2).await, None);
    }

    #[tokio::test]
    async fn events_from_filters_correctly() {
        let mgr = SessionEventLogManager::new();
        let sid = test_session_id();
        let rid = test_run_id();

        let id1 = mgr
            .log_event(sid, rid, "run_started", serde_json::json!({}))
            .await;
        let id2 = mgr
            .log_event(sid, rid, "token_delta", serde_json::json!({"delta": "a"}))
            .await;
        let _id3 = mgr
            .log_event(sid, rid, "run_finished", serde_json::json!({}))
            .await;

        // from_id = id2 should return events at id2 and above
        let events = mgr.events_from(sid, id2).await;
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.event_id >= id2));

        // from_id = id1 + 1 should skip the first event
        let events = mgr.events_from(sid, id1 + 1).await;
        assert_eq!(events.len(), 2);
    }

    /// #1133 Layer 1 — `log_event` mints the id and appends the event in a
    /// single critical section, so id-order always equals append-order even
    /// under concurrent callers. The prior split primitive (`next_event_id()`
    /// then `append()`) did not: a task could mint a lower id but lose the race
    /// to push, landing it *after* a higher-id event — a torn id/position pair.
    ///
    /// We fire N concurrent `log_event` calls on a multi-threaded runtime so
    /// the contention is real, then assert:
    /// (a) the N returned ids are exactly the contiguous permutation `1..=N`
    ///     (no double-mint, skip, or duplicate), and
    /// (b) reading the log back in storage order yields the same `1..=N` — i.e.
    ///     position == id, the no-torn invariant.
    ///
    /// Deterministic: it asserts a structural property that holds for EVERY
    /// interleaving, not a particular ordering of two racing tasks. With the
    /// pre-fix split primitives, (b) would be flaky-failing under load.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn log_event_assigns_id_and_position_atomically_under_concurrency() {
        const N: u64 = 256;

        let mgr = SessionEventLogManager::new();
        let sid = test_session_id();
        let rid = test_run_id();

        let mut set = tokio::task::JoinSet::new();
        for i in 0..N {
            let mgr = mgr.clone();
            set.spawn(async move {
                mgr.log_event(sid, rid, "reasoning_delta", serde_json::json!({ "seq": i }))
                    .await
            });
        }

        let mut returned_ids = Vec::with_capacity(N as usize);
        while let Some(res) = set.join_next().await {
            returned_ids.push(res.expect("log_event task must not panic"));
        }

        // (a) The returned ids are exactly the contiguous permutation 1..=N.
        returned_ids.sort_unstable();
        let expected: Vec<u64> = (1..=N).collect();
        assert_eq!(
            returned_ids, expected,
            "the N returned ids must be the contiguous permutation 1..=N — no \
             id double-minted, skipped, or duplicated under concurrency"
        );

        // (b) Stored order == id order: positions and ids never tore apart.
        let stored = mgr.events_from(sid, 0).await;
        assert_eq!(stored.len(), N as usize, "every event must be persisted");
        let stored_ids: Vec<u64> = stored.iter().map(|e| e.event_id).collect();
        assert_eq!(
            stored_ids, expected,
            "events must appear in the log in strictly increasing id order — \
             id-order == append-order is the atomicity invariant Layer 1 adds"
        );
    }

    fn test_agent_id() -> AgentId {
        AgentId(Uuid::new_v4())
    }

    #[tokio::test]
    async fn agent_event_log_independent_per_agent() {
        let mgr = AgentEventLogManager::new();
        let agent_a = test_agent_id();
        let agent_b = test_agent_id();
        let session_id = test_session_id();
        let run_id = test_run_id();

        let id_a = mgr
            .log_event(
                agent_a,
                run_id,
                session_id,
                "session_activity_started",
                serde_json::json!({}),
            )
            .await;
        let id_b = mgr
            .log_event(
                agent_b,
                run_id,
                session_id,
                "session_activity_started",
                serde_json::json!({}),
            )
            .await;

        // Event IDs are per-agent, so both agents should see their own log
        // start at id 1 (and be unaware of each other's events).
        let a_events = mgr.events_from(agent_a, 0).await;
        let b_events = mgr.events_from(agent_b, 0).await;
        assert_eq!(a_events.len(), 1);
        assert_eq!(b_events.len(), 1);
        assert_eq!(a_events[0].event_id, id_a);
        assert_eq!(b_events[0].event_id, id_b);
    }

    #[tokio::test]
    async fn agent_event_log_replays_from_event_id() {
        let mgr = AgentEventLogManager::new();
        let agent_id = test_agent_id();
        let session_id = test_session_id();
        let run_id = test_run_id();

        let _id1 = mgr
            .log_event(
                agent_id,
                run_id,
                session_id,
                "session_activity_started",
                serde_json::json!({}),
            )
            .await;
        let id2 = mgr
            .log_event(
                agent_id,
                run_id,
                session_id,
                "session_activity_ended",
                serde_json::json!({}),
            )
            .await;

        // from_id = id2 should yield only the second event
        let events = mgr.events_from(agent_id, id2).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "session_activity_ended");
    }
}
