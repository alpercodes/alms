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

        log.append(event).await;
        event_id
    }

    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        let log = self.get_or_create(run_id).await;
        log.events_from(from_id).await
    }
}
