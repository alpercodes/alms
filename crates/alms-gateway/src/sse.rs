//! Server-Sent Events (SSE) streaming for ALMS
//!
//! Provides event streaming per docs/api.md specification.

use alms_core::RunId;
use axum::response::sse::{Event, Sse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::error;
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
        Self::new(
            "connected",
            ConnectedData {
                run_id: run_id.0.to_string(),
            },
        )
    }

    pub fn run_started(run_id: RunId, session_id: alms_core::SessionId) -> Self {
        Self::new(
            "run_started",
            RunStartedData {
                run_id: run_id.0.to_string(),
                session_id: session_id.0.to_string(),
                ts: Utc::now(),
            },
        )
    }

    pub fn token_delta(run_id: RunId, delta: &str, source_agent: Option<String>) -> Self {
        Self::new(
            "token_delta",
            TokenDeltaData {
                run_id: run_id.0.to_string(),
                delta: delta.to_string(),
                source_agent,
            },
        )
    }

    pub fn tool_start(
        run_id: RunId,
        tool_invocation_id: ToolInvocationId,
        tool: &str,
        params: serde_json::Value,
        source_agent: Option<String>,
    ) -> Self {
        Self::new(
            "tool_start",
            ToolStartData {
                run_id: run_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.0.to_string(),
                tool: tool.to_string(),
                params,
                source_agent,
            },
        )
    }

    pub fn tool_end(
        run_id: RunId,
        tool_invocation_id: ToolInvocationId,
        ok: bool,
        result: serde_json::Value,
        source_agent: Option<String>,
    ) -> Self {
        Self::new(
            "tool_end",
            ToolEndData {
                run_id: run_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.0.to_string(),
                ok,
                result,
                source_agent,
            },
        )
    }

    pub fn approval_required(
        run_id: RunId,
        approval_id: &str,
        capability: &str,
        request: serde_json::Value,
        source_agent: Option<String>,
    ) -> Self {
        Self::new(
            "approval_required",
            ApprovalRequiredData {
                run_id: run_id.0.to_string(),
                approval_id: approval_id.to_string(),
                capability: capability.to_string(),
                request,
                source_agent,
            },
        )
    }

    pub fn approval_resolved(run_id: RunId, approval_id: &str, decision: &str) -> Self {
        Self::new(
            "approval_resolved",
            ApprovalResolvedData {
                run_id: run_id.0.to_string(),
                approval_id: approval_id.to_string(),
                decision: decision.to_string(),
                ts: Utc::now(),
            },
        )
    }

    pub fn run_finished(run_id: RunId, ok: bool, usage: alms_core::TokenUsage) -> Self {
        Self::new(
            "run_finished",
            RunFinishedData {
                run_id: run_id.0.to_string(),
                ok,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                ts: Utc::now(),
            },
        )
    }

    pub fn run_error(run_id: RunId, error: &str) -> Self {
        Self::new(
            "run_error",
            RunErrorData {
                run_id: run_id.0.to_string(),
                error: ErrorData {
                    code: "INTERNAL".to_string(),
                    message: error.to_string(),
                },
            },
        )
    }

    pub fn run_cancelled(run_id: RunId) -> Self {
        Self::new(
            "run_cancelled",
            RunCancelledData {
                run_id: run_id.0.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a new run was created on this session.
    pub fn run_created(
        run_id: RunId,
        session_id: alms_core::SessionId,
        is_notification: bool,
    ) -> Self {
        Self::new(
            "run_created",
            RunCreatedData {
                run_id: run_id.0.to_string(),
                session_id: session_id.0.to_string(),
                is_notification,
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a background subagent completed.
    pub fn subagent_completed(
        session_id: alms_core::SessionId,
        subagent_name: Option<String>,
        status: &str,
        summary: &str,
    ) -> Self {
        Self::new(
            "subagent_completed",
            SubagentCompletedData {
                session_id: session_id.0.to_string(),
                subagent_name,
                status: status.to_string(),
                summary: summary.chars().take(200).collect(),
                ts: Utc::now(),
            },
        )
    }
}

/// SSE event stream wrapper
pub struct RunEventStream;

impl RunEventStream {
    pub fn stream(
        receiver: mpsc::UnboundedReceiver<SseEventData>,
    ) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
        Self::stream_with_replay(receiver, Vec::new())
    }

    /// Stream only replayed (historical) events, then close.
    /// Used for terminal runs (Completed/Failed/Cancelled) where no new events will arrive.
    pub fn stream_replay_only(
        replay: Vec<SseEventData>,
    ) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
        let stream = tokio_stream::iter(replay.into_iter().map(|data| {
            let event = Event::default()
                .event(&data.event_type)
                .id(data
                    .event_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| Uuid::new_v4().to_string()))
                .json_data(&data.data)
                .unwrap_or_else(|e| {
                    error!(
                        "Failed to serialize SSE replay event '{}': {}",
                        data.event_type, e
                    );
                    Event::default().data("{}")
                });
            Ok::<_, Infallible>(event)
        }));

        Sse::new(stream)
    }

    pub fn stream_with_replay(
        receiver: mpsc::UnboundedReceiver<SseEventData>,
        replay: Vec<SseEventData>,
    ) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
        // Track the highest event_id in the replay set so we can deduplicate
        // events that arrive on both the replay snapshot and the live channel
        // (possible because we register-before-replay to close the race gap).
        let max_replay_id = replay.iter().filter_map(|e| e.event_id).max().unwrap_or(0);

        let replay_stream = tokio_stream::iter(replay.into_iter().map(|data| {
            let event = Event::default()
                .event(&data.event_type)
                .id(data
                    .event_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| Uuid::new_v4().to_string()))
                .json_data(&data.data)
                .unwrap_or_else(|e| {
                    error!(
                        "Failed to serialize SSE replay event '{}': {}",
                        data.event_type, e
                    );
                    Event::default().data("{}")
                });
            Ok::<_, Infallible>(event)
        }));

        let live_stream = UnboundedReceiverStream::new(receiver)
            .filter(move |data| {
                // Skip events already delivered during replay
                !matches!(data.event_id, Some(id) if id <= max_replay_id)
            })
            .map(|data| {
                let event = Event::default()
                    .event(&data.event_type)
                    .id(data
                        .event_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| Uuid::new_v4().to_string()))
                    .json_data(&data.data)
                    .unwrap_or_else(|e| {
                        error!(
                            "Failed to serialize SSE live event '{}': {}",
                            data.event_type, e
                        );
                        Event::default().data("{}")
                    });
                Ok::<_, Infallible>(event)
            });

        let stream = replay_stream.chain(live_stream);

        Sse::new(stream).keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
    }
}

/// Create event channel
pub fn event_channel() -> (
    mpsc::UnboundedSender<SseEventData>,
    mpsc::UnboundedReceiver<SseEventData>,
) {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolStartData {
    run_id: String,
    tool_invocation_id: String,
    tool: String,
    params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolEndData {
    run_id: String,
    tool_invocation_id: String,
    ok: bool,
    result: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApprovalRequiredData {
    run_id: String,
    approval_id: String,
    capability: String,
    request: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApprovalResolvedData {
    run_id: String,
    approval_id: String,
    decision: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct RunFinishedData {
    run_id: String,
    ok: bool,
    prompt_tokens: u32,
    completion_tokens: u32,
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

#[derive(Debug, Serialize)]
struct RunCancelledData {
    run_id: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct RunCreatedData {
    run_id: String,
    session_id: String,
    is_notification: bool,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct SubagentCompletedData {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_name: Option<String>,
    status: String,
    summary: String,
    ts: DateTime<Utc>,
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

        let event = SseEventData::tool_end(
            run_id,
            tool_id,
            true,
            serde_json::json!({"output": "test"}),
            None,
        );

        assert_eq!(event.event_type, "tool_end");
    }

    #[tokio::test]
    async fn test_dedup_filters_replayed_events() {
        use tokio_stream::StreamExt as _;

        let run_id = RunId::new();

        // Simulate: replay has max event_id = 3. Live channel receives ids 2,3,4.
        // The dedup filter should drop ids 2 and 3 from live, passing only id 4.
        let max_replay_id: u64 = 3;

        let (tx, rx) = mpsc::unbounded_channel();
        for id in 2..=4 {
            let mut e = SseEventData::connected(run_id);
            e.event_id = Some(id);
            tx.send(e).unwrap();
        }
        drop(tx);

        let live_stream = UnboundedReceiverStream::new(rx)
            .filter(move |data| !matches!(data.event_id, Some(id) if id <= max_replay_id));

        let events: Vec<_> = live_stream.collect().await;
        assert_eq!(events.len(), 1, "only event_id=4 should pass dedup filter");
        assert_eq!(events[0].event_id, Some(4));
    }
}
