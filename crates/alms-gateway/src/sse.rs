//! Server-Sent Events (SSE) streaming for ALMS
//!
//! Provides event streaming per docs/api.md specification.

use alms_core::RunId;
use axum::{
    http::StatusCode,
    response::sse::{Event, Sse},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tracing::{error, info};
use uuid::Uuid;

/// Unique identifier for a tool invocation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationId(pub Uuid);

impl ToolInvocationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ToolInvocationId {
    fn default() -> Self {
        Self::new()
    }
}

/// SSE event data - per docs/api.md spec
#[derive(Debug, Clone, Serialize)]
pub struct SseEventData {
    #[serde(rename = "event")]
    pub event_type: String,
    pub data: serde_json::Value,
    pub ts: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<u64>,
}

impl SseEventData {
    pub fn new(event_type: &str, data: impl Serialize) -> Self {
        Self {
            event_type: event_type.to_string(),
            data: serde_json::to_value(data).unwrap_or_default(),
            ts: Utc::now(),
            event_id: None,
        }
    }

    pub fn connected(run_id: RunId) -> Self {
        Self::new("connected", ConnectedData { run_id: run_id.0.to_string() })
    }

    pub fn run_started(run_id: RunId, session_id: alms_core::SessionId) -> Self {
        Self::new("run_started", RunStartedData {
            run_id: run_id.0.to_string(),
            session_id: session_id.0.to_string(),
            ts: Utc::now(),
        })
    }

    pub fn token_delta(run_id: RunId, delta: &str) -> Self {
        Self::new("token_delta", TokenDeltaData {
            run_id: run_id.0.to_string(),
            delta: delta.to_string(),
        })
    }

    pub fn tool_start(run_id: RunId, tool_invocation_id: ToolInvocationId, tool: &str, params: serde_json::Value) -> Self {
        Self::new("tool_start", ToolStartData {
            run_id: run_id.0.to_string(),
            tool_invocation_id: tool_invocation_id.0.to_string(),
            tool: tool.to_string(),
            params,
        })
    }

    pub fn tool_end(run_id: RunId, tool_invocation_id: ToolInvocationId, ok: bool, result: serde_json::Value) -> Self {
        Self::new("tool_end", ToolEndData {
            run_id: run_id.0.to_string(),
            tool_invocation_id: tool_invocation_id.0.to_string(),
            ok,
            result,
        })
    }

    pub fn tool_error(run_id: RunId, tool_invocation_id: ToolInvocationId, error: &str) -> Self {
        Self::new("tool_error", ToolErrorData {
            run_id: run_id.0.to_string(),
            tool_invocation_id: tool_invocation_id.0.to_string(),
            error: error.to_string(),
        })
    }

    pub fn approval_required(run_id: RunId, approval_id: &str, capability: &str, request: serde_json::Value) -> Self {
        Self::new("approval_required", ApprovalRequiredData {
            run_id: run_id.0.to_string(),
            approval_id: approval_id.to_string(),
            capability: capability.to_string(),
            request,
        })
    }

    pub fn run_finished(run_id: RunId, ok: bool) -> Self {
        Self::new("run_finished", RunFinishedData {
            run_id: run_id.0.to_string(),
            ok,
            ts: Utc::now(),
        })
    }

    pub fn run_error(run_id: RunId, error: &str) -> Self {
        Self::new("run_error", RunErrorData {
            run_id: run_id.0.to_string(),
            error: ErrorData {
                code: "INTERNAL".to_string(),
                message: error.to_string(),
            },
        })
    }
}

/// SSE event stream wrapper
pub struct RunEventStream;

impl RunEventStream {
    pub fn new(receiver: mpsc::UnboundedReceiver<SseEventData>) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
        Self::new_with_events(receiver, Vec::new())
    }

    pub fn new_with_events(
        receiver: mpsc::UnboundedReceiver<SseEventData>,
        replay: Vec<SseEventData>,
    ) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
        let replay_stream = tokio_stream::iter(replay.into_iter().map(|data| {
            let event = Event::default()
                .event(&data.event_type)
                .id(data.event_id.map(|id| id.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string()))
                .json_data(&serde_json::json!({
                    "event": data.event_type,
                    "data": data.data,
                    "ts": data.ts.to_rfc3339(),
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            Ok::<_, Infallible>(event)
        }));

        let live_stream = ReceiverStream::new(receiver).map(|data| {
            let event = Event::default()
                .event(&data.event_type)
                .id(data.event_id.map(|id| id.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string()))
                .json_data(&serde_json::json!({
                    "event": data.event_type,
                    "data": data.data,
                    "ts": data.ts.to_rfc3339(),
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            Ok::<_, Infallible>(event)
        });

        let stream = replay_stream.chain(live_stream);

        Sse::new(stream)
            .keep_alive(
                axum::response::sse::KeepAlive::new()
                    .interval(Duration::from_secs(15))
                    .text("ping"),
            )
    }
}

/// Create event channel
pub fn event_channel() -> (mpsc::UnboundedSender<SseEventData>, mpsc::UnboundedReceiver<SseEventData>) {
    mpsc::unbounded_channel()
}

// Event data structs per API spec
#[derive(Debug, Serialize)]
struct ConnectedData {
    run_id: String,
}

#[derive(Debug, Serialize)]
struct RunStartedData {
    run_id: String,
    session_id: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct TokenDeltaData {
    run_id: String,
    delta: String,
}

#[derive(Debug, Serialize)]
struct ToolStartData {
    run_id: String,
    tool_invocation_id: String,
    tool: String,
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ToolEndData {
    run_id: String,
    tool_invocation_id: String,
    ok: bool,
    result: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ToolErrorData {
    run_id: String,
    tool_invocation_id: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct ApprovalRequiredData {
    run_id: String,
    approval_id: String,
    capability: String,
    request: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct RunFinishedData {
    run_id: String,
    ok: bool,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct RunErrorData {
    run_id: String,
    error: ErrorData,
}

#[derive(Debug, Serialize)]
struct ErrorData {
    code: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::RunId;

    #[test]
    fn test_event_data_serialization() {
        let run_id = RunId::new();
        let event = SseEventData::run_started(run_id, alms_core::SessionId::new());
        
        assert_eq!(event.event_type, "run_started");
        
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("run_started"));
        assert!(json.contains(&run_id.0.to_string()));
    }

    #[test]
    fn test_tool_end_event() {
        let run_id = RunId::new();
        let tool_id = ToolInvocationId::new();
        
        let event = SseEventData::tool_end(run_id, tool_id, true, serde_json::json!({"output": "test"}));
        
        assert_eq!(event.event_type, "tool_end");
    }
}