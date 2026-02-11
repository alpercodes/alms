//! Event log for durable SSE event storage
//!
//! Enables reconnect-after-restart by persisting events.

use crate::{RunId, SessionId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A logged event for persistence and replay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub event_id: u64,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub event_type: String,
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
}

/// Event log for a run - append-only storage
#[derive(Debug, Default)]
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

    /// Append an event to the log
    pub async fn append(&self, event: LoggedEvent) {
        let mut events = self.events.write().await;
        events.push(event);
    }

    /// Get next event ID
    pub async fn next_event_id(&self) -> u64 {
        let mut next = self.next_id.write().await;
        let id = *next;
        *next += 1;
        id
    }

    /// Get events starting from a specific ID (for reconnect)
    pub async fn events_from(&self, from_id: u64) -> Vec<LoggedEvent> {
        let events = self.events.read().await;
        events
            .iter()
            .filter(|e| e.event_id >= from_id)
            .cloned()
            .collect()
    }

    /// Get all events
    pub async fn all_events(&self) -> Vec<LoggedEvent> {
        let events = self.events.read().await;
        events.clone()
    }

    /// Get latest event ID
    pub async fn latest_event_id(&self) -> Option<u64> {
        let events = self.events.read().await;
        events.last().map(|e| e.event_id)
    }
}

/// Global event log manager
#[derive(Debug, Default)]
pub struct EventLogManager {
    logs: Arc<RwLock<std::collections::HashMap<RunId, EventLog>>>,
}

impl EventLogManager {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Get or create event log for a run
    pub async fn get_or_create(&self, run_id: RunId) -> EventLog {
        let mut logs = self.logs.write().await;
        logs.get(&run_id).cloned().unwrap_or_else(|| {
            let log = EventLog::new();
            logs.insert(run_id, log.clone());
            log
        })
    }

    /// Get event log for a run (if exists)
    pub async fn get(&self, run_id: RunId) -> Option<EventLog> {
        let logs = self.logs.read().await;
        logs.get(&run_id).cloned()
    }

    /// Log an event and get its assigned ID
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
        
        log.append(event).await;
        event_id
    }
}

impl Clone for EventLog {
    fn clone(&self) -> Self {
        Self {
            events: Arc::clone(&self.events),
            next_id: Arc::clone(&self.next_id),
        }
    }
}