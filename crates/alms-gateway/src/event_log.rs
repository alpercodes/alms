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

    pub async fn append(&self, event: LoggedEvent) {
        let mut events = self.events.write().await;
        events.push(event);
    }

    pub async fn next_event_id(&self) -> u64 {
        let mut next = self.next_id.write().await;
        let id = *next;
        *next += 1;
        id
    }

    pub async fn events_from(&self, from_id: u64) -> Vec<LoggedEvent> {
        let events = self.events.read().await;
        events
            .iter()
            .filter(|e| e.event_id >= from_id)
            .cloned()
            .collect()
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
        let event_id = log.next_event_id().await;

        let event = LoggedEvent {
            event_id,
            run_id,
            session_id,
            event_type: event_type.to_string(),
            data,
            ts: Utc::now(),
        };

        // Single lock acquisition for append
        log.events.write().await.push(event);
        event_id
    }

    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&run_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
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
        let event_id = log.next_event_id().await;

        let event = LoggedEvent {
            event_id,
            run_id,
            session_id,
            event_type: event_type.to_string(),
            data,
            ts: Utc::now(),
        };

        // Append + trim in a single lock acquisition
        {
            let mut events = log.events.write().await;
            events.push(event);
            if events.len() > SESSION_EVENT_LOG_MAX {
                let drain = events.len() - SESSION_EVENT_LOG_MAX;
                events.drain(..drain);
            }
        }

        event_id
    }

    pub async fn events_from(&self, session_id: SessionId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&session_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
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
        let event_id = log.next_event_id().await;

        let event = LoggedEvent {
            event_id,
            run_id,
            session_id,
            event_type: event_type.to_string(),
            data,
            ts: Utc::now(),
        };

        // Append + trim in a single lock acquisition
        {
            let mut events = log.events.write().await;
            events.push(event);
            if events.len() > AGENT_EVENT_LOG_MAX {
                let drain = events.len() - AGENT_EVENT_LOG_MAX;
                events.drain(..drain);
            }
        }

        event_id
    }

    pub async fn events_from(&self, agent_id: AgentId, from_id: u64) -> Vec<LoggedEvent> {
        let logs = self.logs.read().await;
        match logs.get(&agent_id) {
            Some(log) => log.events_from(from_id).await,
            None => Vec::new(),
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
