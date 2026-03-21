//! Event log for durable SSE event storage (gateway-local)

use alms_core::{RunId, SessionId};
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
}
