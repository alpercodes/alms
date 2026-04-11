//! Server-Sent Events (SSE) streaming for ALMS
//!
//! Provides event streaming per docs/api.md specification.

use alms_core::RunId;
use axum::response::sse::{Event, Sse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::error;
use uuid::Uuid;

/// Monotonic counter for ephemeral SSE event IDs.
///
/// Events without a persisted `event_id` (e.g. `token_delta`, `status`) are
/// assigned IDs like `ephemeral-1`, `ephemeral-2`, etc.  These are clearly
/// non-numeric so the browser's native `EventSource` auto-reconnect will never
/// send them as a valid `Last-Event-Id` that the backend would try to parse as
/// `u64`.
static EPHEMERAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_ephemeral_id() -> String {
    let n = EPHEMERAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("ephemeral-{n}")
}

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
        Self::run_error_with_code(run_id, &classify_error(error), error)
    }

    /// Emit a `run_error` with an explicit error code for the frontend to
    /// style differently (e.g. `AUTH`, `RATE_LIMIT`, `TIMEOUT`, `INTERNAL`).
    pub fn run_error_with_code(run_id: RunId, code: &str, message: &str) -> Self {
        Self::new(
            "run_error",
            RunErrorData {
                run_id: run_id.0.to_string(),
                error: ErrorData {
                    code: code.to_string(),
                    message: message.to_string(),
                },
            },
        )
    }

    /// Emit a `run_warning` event for non-fatal conditions like max iterations
    /// reached. The frontend should style these as warnings (yellow), not
    /// errors (red).
    pub fn run_warning(
        run_id: RunId,
        code: &str,
        message: &str,
        source_agent: Option<String>,
    ) -> Self {
        Self::new(
            "run_warning",
            RunWarningData {
                run_id: run_id.0.to_string(),
                warning: WarningData {
                    code: code.to_string(),
                    message: message.to_string(),
                },
                source_agent,
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

    /// Transient status update for the UI to show what the agent is doing.
    ///
    /// Phases: `building_context`, `summarizing`, `calling_llm`, `executing_tools`.
    /// `detail` carries extra info such as tool names for the `executing_tools` phase.
    pub fn status(run_id: RunId, phase: &str, detail: Option<String>) -> Self {
        Self::new(
            "status",
            StatusData {
                run_id: run_id.0.to_string(),
                phase: phase.to_string(),
                detail,
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a new run was created on this session.
    ///
    /// `source` indicates what triggered the run:
    /// - `"user"` — normal user-initiated run
    /// - `"peer:<agent_name>"` — DM from another agent
    /// - `"job"` — scheduled job
    /// - `"subagent"` — subagent completion notification
    /// - `"notification:dm_ended:<agent_name>"` — DM conversation ended notification
    ///
    /// `queued_behind` indicates how many runs are ahead of this one in the
    /// agent's queue. 0 means the run will start immediately.
    pub fn run_created(
        run_id: RunId,
        session_id: alms_core::SessionId,
        is_notification: bool,
        source: Option<String>,
        queued_behind: usize,
    ) -> Self {
        Self::new(
            "run_created",
            RunCreatedData {
                run_id: run_id.0.to_string(),
                session_id: session_id.0.to_string(),
                is_notification,
                source,
                queued_behind,
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
        subagent_session_id: alms_core::SessionId,
    ) -> Self {
        Self::new(
            "subagent_completed",
            SubagentCompletedData {
                session_id: session_id.0.to_string(),
                subagent_name,
                status: status.to_string(),
                summary: summary.chars().take(200).collect(),
                subagent_session_id: subagent_session_id.0.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a scheduled job completed (sent to the agent's user-facing session).
    pub fn job_completed(
        session_id: alms_core::SessionId,
        job_name: &str,
        status: &str,
        summary: &str,
    ) -> Self {
        Self::new(
            "job_completed",
            JobCompletedData {
                session_id: session_id.0.to_string(),
                job_name: job_name.chars().take(100).collect(),
                status: status.to_string(),
                summary: summary.chars().take(200).collect(),
                ts: Utc::now(),
            },
        )
    }

    /// Debug snapshot of the full context window sent to the LLM.
    ///
    /// Only emitted when debug mode is enabled. Contains the assembled
    /// messages array, tool names, and token count estimates so the web
    /// UI can display exactly what the LLM sees.
    pub fn context_debug(
        run_id: RunId,
        messages: serde_json::Value,
        tool_names: Vec<String>,
        total_tokens: usize,
        system_tokens: usize,
        history_message_count: usize,
    ) -> Self {
        Self::new(
            "context_debug",
            ContextDebugData {
                run_id: run_id.0.to_string(),
                messages,
                tool_names,
                total_tokens,
                system_tokens,
                history_message_count,
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a DM conversation between two agents has ended.
    ///
    /// Emitted on the DM session SSE stream so the web UI can show a
    /// "conversation ended" indicator. See Phase 6 of #384.
    pub fn dm_conversation_ended(
        session_id: alms_core::SessionId,
        ended_by: &str,
        peer: &str,
        reason: &str,
        context_id: &str,
    ) -> Self {
        Self::new(
            "dm_conversation_ended",
            DmConversationEndedData {
                session_id: session_id.0.to_string(),
                ended_by: ended_by.to_string(),
                peer: peer.to_string(),
                reason: reason.to_string(),
                context_id: context_id.to_string(),
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
                    .unwrap_or_else(next_ephemeral_id))
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
                    .unwrap_or_else(next_ephemeral_id))
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
                        .unwrap_or_else(next_ephemeral_id))
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
struct RunWarningData {
    run_id: String,
    warning: WarningData,
    /// When set, this warning originated from a subagent.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

#[derive(Debug, Serialize)]
struct WarningData {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct RunCancelledData {
    run_id: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct StatusData {
    run_id: String,
    phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct RunCreatedData {
    run_id: String,
    session_id: String,
    is_notification: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// Number of runs ahead of this one in the agent's queue.
    /// 0 means the run will start immediately.
    queued_behind: usize,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct SubagentCompletedData {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_name: Option<String>,
    status: String,
    summary: String,
    /// The subagent's own session ID (so the frontend can navigate to it).
    subagent_session_id: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct JobCompletedData {
    session_id: String,
    job_name: String,
    status: String,
    summary: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct ContextDebugData {
    run_id: String,
    messages: serde_json::Value,
    tool_names: Vec<String>,
    total_tokens: usize,
    system_tokens: usize,
    history_message_count: usize,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DmConversationEndedData {
    session_id: String,
    ended_by: String,
    peer: String,
    reason: String,
    context_id: String,
    ts: DateTime<Utc>,
}

/// Classify an error message into an error code for the frontend.
///
/// The code is used by the UI to pick appropriate styling:
/// - `AUTH` — authentication failure (red, actionable hint)
/// - `RATE_LIMIT` — rate-limited by the LLM provider (yellow/warning)
/// - `TIMEOUT` — request or connection timeout (yellow/warning)
/// - `INTERNAL` — catch-all for unexpected errors (red)
fn classify_error(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
        || lower.contains("invalid api key")
        || lower.contains("invalid x-api-key")
    {
        "AUTH".to_string()
    } else if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
    {
        "RATE_LIMIT".to_string()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "TIMEOUT".to_string()
    } else {
        "INTERNAL".to_string()
    }
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

    #[test]
    fn test_status_event() {
        let run_id = RunId::new();

        let event = SseEventData::status(run_id, "calling_llm", None);
        assert_eq!(event.event_type, "status");
        assert_eq!(event.data["phase"], "calling_llm");
        assert!(event.data["detail"].is_null());
        assert!(event.data["ts"].is_string());

        let event_with_detail = SseEventData::status(
            run_id,
            "executing_tools",
            Some("shell_exec, fs_read".into()),
        );
        assert_eq!(event_with_detail.data["phase"], "executing_tools");
        assert_eq!(event_with_detail.data["detail"], "shell_exec, fs_read");
    }

    #[test]
    fn test_job_completed_event() {
        let session_id = alms_core::SessionId::new();
        let event = SseEventData::job_completed(
            session_id,
            "Summarize yesterday",
            "success",
            "All systems operational. Summary generated.",
        );

        assert_eq!(event.event_type, "job_completed");

        // Verify the inner data has the expected fields.
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["job_name"], "Summarize yesterday");
        assert_eq!(event.data["status"], "success");
        assert_eq!(
            event.data["summary"],
            "All systems operational. Summary generated."
        );
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_job_completed_truncates_long_fields() {
        let session_id = alms_core::SessionId::new();
        let long_name = "x".repeat(150);
        let long_summary = "y".repeat(300);
        let event = SseEventData::job_completed(session_id, &long_name, "error", &long_summary);

        let name_len = event.data["job_name"].as_str().unwrap().len();
        let summary_len = event.data["summary"].as_str().unwrap().len();
        assert!(name_len <= 100, "job_name should be truncated to 100 chars");
        assert!(
            summary_len <= 200,
            "summary should be truncated to 200 chars"
        );
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

    #[test]
    fn test_classify_error_auth() {
        assert_eq!(classify_error("401 Unauthorized"), "AUTH");
        assert_eq!(classify_error("403 Forbidden"), "AUTH");
        assert_eq!(classify_error("authentication failed"), "AUTH");
        assert_eq!(classify_error("Invalid API key provided"), "AUTH");
        assert_eq!(classify_error("invalid x-api-key"), "AUTH");
    }

    #[test]
    fn test_classify_error_rate_limit() {
        assert_eq!(classify_error("429 Too Many Requests"), "RATE_LIMIT");
        assert_eq!(classify_error("rate limit exceeded"), "RATE_LIMIT");
        assert_eq!(classify_error("rate_limit_error"), "RATE_LIMIT");
    }

    #[test]
    fn test_classify_error_timeout() {
        assert_eq!(classify_error("connection timed out"), "TIMEOUT");
        assert_eq!(classify_error("request timeout after 120s"), "TIMEOUT");
    }

    #[test]
    fn test_classify_error_internal() {
        assert_eq!(classify_error("something went wrong"), "INTERNAL");
    }

    #[test]
    fn test_run_warning_event() {
        let run_id = RunId::new();
        let event =
            SseEventData::run_warning(run_id, "MAX_ITERATIONS", "Max iterations reached", None);
        assert_eq!(event.event_type, "run_warning");
        assert_eq!(event.data["warning"]["code"], "MAX_ITERATIONS");
        assert_eq!(event.data["warning"]["message"], "Max iterations reached");
    }

    #[test]
    fn test_run_error_with_code() {
        let run_id = RunId::new();
        let event = SseEventData::run_error_with_code(run_id, "AUTH", "401 Unauthorized");
        assert_eq!(event.event_type, "run_error");
        assert_eq!(event.data["error"]["code"], "AUTH");
        assert_eq!(event.data["error"]["message"], "401 Unauthorized");
    }

    #[test]
    fn test_run_error_auto_classifies() {
        let run_id = RunId::new();

        let auth_event = SseEventData::run_error(run_id, "401 Unauthorized");
        assert_eq!(auth_event.data["error"]["code"], "AUTH");

        let rate_event = SseEventData::run_error(run_id, "429 Too Many Requests");
        assert_eq!(rate_event.data["error"]["code"], "RATE_LIMIT");

        let timeout_event = SseEventData::run_error(run_id, "connection timed out");
        assert_eq!(timeout_event.data["error"]["code"], "TIMEOUT");

        let generic_event = SseEventData::run_error(run_id, "unknown failure");
        assert_eq!(generic_event.data["error"]["code"], "INTERNAL");
    }

    #[test]
    fn test_dm_conversation_ended_event() {
        let session_id = alms_core::SessionId::new();
        let event = SseEventData::dm_conversation_ended(
            session_id,
            "alice",
            "bob",
            "ignored",
            "dm:alice:bob",
        );

        assert_eq!(event.event_type, "dm_conversation_ended");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["ended_by"], "alice");
        assert_eq!(event.data["peer"], "bob");
        assert_eq!(event.data["reason"], "ignored");
        assert_eq!(event.data["context_id"], "dm:alice:bob");
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_dm_conversation_ended_event_depth_exceeded() {
        let session_id = alms_core::SessionId::new();
        let event = SseEventData::dm_conversation_ended(
            session_id,
            "bob",
            "alice",
            "depth_exceeded",
            "dm:alice:bob",
        );

        assert_eq!(event.event_type, "dm_conversation_ended");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["ended_by"], "bob");
        assert_eq!(event.data["peer"], "alice");
        assert_eq!(event.data["reason"], "depth_exceeded");
        assert_eq!(event.data["context_id"], "dm:alice:bob");
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_run_cancelled_event() {
        let run_id = RunId::new();
        let event = SseEventData::run_cancelled(run_id);

        assert_eq!(event.event_type, "run_cancelled");
        assert_eq!(event.data["run_id"], run_id.0.to_string());
        assert!(event.data["ts"].is_string(), "ts should be a string");

        // Verify ts is RFC3339 format
        let ts_str = event.data["ts"].as_str().unwrap();
        assert!(ts_str.contains("T"), "ts should be ISO8601/RFC3339");
    }

    #[test]
    fn test_context_debug_event() {
        let run_id = RunId::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "prompt"},
            {"role": "user", "content": "hello"},
        ]);
        let tool_names = vec!["shell_exec".to_string(), "fs_read".to_string()];

        let event = SseEventData::context_debug(run_id, messages, tool_names, 500, 200, 3);

        assert_eq!(event.event_type, "context_debug");
        assert_eq!(event.data["run_id"], run_id.0.to_string());
        assert_eq!(event.data["total_tokens"], 500);
        assert_eq!(event.data["system_tokens"], 200);
        assert_eq!(event.data["history_message_count"], 3);
        assert_eq!(event.data["tool_names"].as_array().unwrap().len(), 2);
        assert_eq!(event.data["messages"].as_array().unwrap().len(), 2);
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }
}
