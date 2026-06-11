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
                resolved_config: None,
            },
        )
    }

    /// `run_started` carrying the layered run-config snapshot (#837).
    ///
    /// Same shape as [`SseEventData::run_started`] plus a `resolved_config`
    /// object identifying the provider / model / posture / budgets the
    /// run actually committed to. Live observers can use this to confirm
    /// "I set model X but Y was used"-class reports without log
    /// correlation. Older clients that don't know about the new field
    /// simply ignore it — the wire shape stays additive.
    pub fn run_started_with_config(
        run_id: RunId,
        session_id: alms_core::SessionId,
        resolved_config: alms_core::ResolvedRunConfig,
    ) -> Self {
        Self::new(
            "run_started",
            RunStartedData {
                run_id: run_id.0.to_string(),
                session_id: session_id.0.to_string(),
                ts: Utc::now(),
                resolved_config: Some(resolved_config),
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

    /// Provider-neutral reasoning / extended-thinking text delta.
    ///
    /// Emitted alongside `token_delta` when the model produces extended-
    /// thinking output (Anthropic Claude 4.x, and future reasoning models).
    /// Clients are expected to render these in a separate collapsible
    /// panel that defaults to closed.
    pub fn reasoning_delta(run_id: RunId, text: &str, source_agent: Option<String>) -> Self {
        Self::new(
            "reasoning_delta",
            ReasoningDeltaData {
                run_id: run_id.0.to_string(),
                text: text.to_string(),
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
        task_id: Option<String>,
    ) -> Self {
        Self::new(
            "tool_start",
            ToolStartData {
                run_id: run_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.0.to_string(),
                tool: tool.to_string(),
                params,
                source_agent,
                task_id,
            },
        )
    }

    pub fn tool_end(
        run_id: RunId,
        tool_invocation_id: ToolInvocationId,
        ok: bool,
        result: serde_json::Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    ) -> Self {
        Self::new(
            "tool_end",
            ToolEndData {
                run_id: run_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.0.to_string(),
                ok,
                result,
                source_agent,
                task_id,
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
                reasoning_tokens: usage.reasoning_tokens,
                // Cache tokens (#766) — only Anthropic populates these today;
                // `skip_serializing_if` on the struct keeps the wire shape
                // byte-identical to pre-#766 when unset.
                cache_creation_input_tokens: usage.cache_creation_input_tokens,
                cache_read_input_tokens: usage.cache_read_input_tokens,
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

    /// Emit a `run_warning` event for non-fatal conditions (e.g. DM text-only
    /// retries). The frontend should style these as warnings (yellow), not
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

    /// Session-level + per-run: a queued run's position has changed.
    ///
    /// Emitted when the head of the per-agent queue advances (a run finishes,
    /// fails, or is cancelled), so still-queued runs can show a live
    /// decrementing position in the chat UI.
    ///
    /// `position` is **1-indexed**: position 1 means "next up" (one run still
    /// ahead — typically the one that just started running). `position` always
    /// matches the same number that `run_created.queued_behind` carried when
    /// the run was first enqueued.
    ///
    /// No event is emitted with `position == 0`; the existing
    /// `run_started` event is the signal that the run has left the queue and
    /// is now executing. Once a queued run reaches a terminal state (cancelled
    /// before dispatch), no further position events fire for that run.
    pub fn run_queue_position(
        run_id: RunId,
        session_id: alms_core::SessionId,
        agent_id: alms_core::AgentId,
        position: usize,
    ) -> Self {
        Self::new(
            "run_queue_position",
            RunQueuePositionData {
                run_id: run_id.0.to_string(),
                session_id: session_id.0.to_string(),
                agent_id: agent_id.0.to_string(),
                position,
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a background subagent completed.
    ///
    /// `tool_invocation_id` is the parent's `invoke_agent` invocation id
    /// (#1125, A1-2), mirroring the sibling `subagent_started` event. When
    /// present the frontend resolves the completion to the right SubagentBar
    /// entry by invocation id — which disambiguates concurrent unnamed /
    /// ephemeral subagents that the name-only first-match heuristic
    /// cross-wires. `None` for legacy callers that don't carry the id; the
    /// field is then omitted from the wire and the frontend falls back to
    /// `subagent_session_id`, then `subagent_name`.
    pub fn subagent_completed(
        session_id: alms_core::SessionId,
        tool_invocation_id: Option<ToolInvocationId>,
        subagent_name: Option<String>,
        status: &str,
        summary: &str,
        subagent_session_id: alms_core::SessionId,
    ) -> Self {
        Self::new(
            "subagent_completed",
            SubagentCompletedData {
                session_id: session_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.map(|id| id.0.to_string()),
                subagent_name,
                status: status.to_string(),
                summary: summary.chars().take(200).collect(),
                subagent_session_id: subagent_session_id.0.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a subagent's session has just been created (#1105).
    ///
    /// Emitted on the parent's stream the moment the coordinator's
    /// `spawn_subagent` resolves the subagent's session id, so the UI's
    /// SubagentBar can render the "View session" button live during a
    /// foreground `invoke_agent` run instead of only at `tool_end`.
    ///
    /// Ordering invariant (per #1105 issue body and Tim's review on PR
    /// #1113): the parent's `tool_start` for `invoke_agent` MUST be
    /// emitted before this event, and any nested `tool_start` from
    /// inside the subagent MUST follow this event. The forwarding
    /// plumbing in `runs/tools.rs` and `runs/lifecycle.rs` preserves
    /// that ordering by virtue of the runtime channel's FIFO semantics —
    /// `spawn_subagent` emits `SubagentStarted` after the parent
    /// runtime has already queued its `ToolStart`, and the subagent's
    /// loop only starts firing events after this point.
    ///
    /// `tool_invocation_id` is the parent's `invoke_agent` invocation
    /// id; the frontend's resolver tries it **first** (id → session id →
    /// name, per #1125 A1-2), which disambiguates ephemeral / unnamed
    /// subagents that share `subagent_name == null`. It is a required
    /// field here — the coordinator skips emitting this event entirely
    /// when the id is absent, since a `subagent_started` without it can't
    /// be attached to a SubagentBar entry.
    pub fn subagent_started(
        session_id: alms_core::SessionId,
        tool_invocation_id: ToolInvocationId,
        subagent_name: Option<String>,
        subagent_session_id: alms_core::SessionId,
    ) -> Self {
        Self::new(
            "subagent_started",
            SubagentStartedData {
                session_id: session_id.0.to_string(),
                tool_invocation_id: tool_invocation_id.0.to_string(),
                subagent_name,
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
    /// UI can display exactly what the LLM sees. Carries the agent's
    /// id and name (#1003) so the UI can attribute each snapshot to the
    /// correct agent — important for DM sessions where two agents
    /// alternate turns on the same session and emit independent
    /// per-perspective context windows.
    #[allow(clippy::too_many_arguments)]
    pub fn context_debug(
        run_id: RunId,
        messages: serde_json::Value,
        tool_names: Vec<String>,
        total_tokens: usize,
        system_tokens: usize,
        history_message_count: usize,
        agent_id: String,
        agent_name: Option<String>,
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
                agent_id,
                agent_name,
                ts: Utc::now(),
            },
        )
    }

    /// Session-level: a new message was persisted to a DM session.
    ///
    /// Emitted by the `dm_event_loop` when the `MessageBus` notifies that a
    /// peer message was written to the shared DM session. This enables live
    /// rendering of DM messages in the web UI without requiring a page
    /// reload. See #632.
    pub fn dm_message(
        session_id: alms_core::SessionId,
        from_agent: &str,
        from_agent_id: &str,
        message: &str,
        ts: DateTime<Utc>,
    ) -> Self {
        Self::new(
            "dm_message",
            DmMessageData {
                session_id: session_id.0.to_string(),
                from_agent: from_agent.to_string(),
                from_agent_id: from_agent_id.to_string(),
                message: message.to_string(),
                ts,
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

    /// Cross-session: a DM run has started, forwarded to the agent's
    /// webchat session so the status bar can show "Chatting with {peer}".
    ///
    /// This is a lightweight echo of the `run_created` event that lands on
    /// the DM session — only the peer name is included.
    pub fn dm_activity_started(session_id: alms_core::SessionId, peer: &str) -> Self {
        Self::new(
            "dm_activity_started",
            DmActivityStartedData {
                session_id: session_id.0.to_string(),
                peer: peer.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Cross-session: a DM run status update, forwarded to the agent's
    /// webchat session so the status bar can show DM activity.
    ///
    /// All agent-loop phases are forwarded so the webchat session never
    /// has a stale or blank status bar during a DM conversation.
    pub fn dm_activity_status(
        session_id: alms_core::SessionId,
        peer: &str,
        phase: &str,
        detail: Option<String>,
    ) -> Self {
        Self::new(
            "dm_activity_status",
            DmActivityStatusData {
                session_id: session_id.0.to_string(),
                peer: peer.to_string(),
                phase: phase.to_string(),
                detail,
                ts: Utc::now(),
            },
        )
    }

    /// Cross-session: a DM run has ended, forwarded to the agent's
    /// webchat session so the status bar can update accordingly.
    ///
    /// Unlike `dm_conversation_ended` (which signals the entire DM
    /// conversation is over), this signals that a single DM run has
    /// finished.  The frontend uses this to decide whether to keep
    /// showing "Chatting with {peer}..." (if more DM runs are expected)
    /// or clear the status (if the conversation ended).
    pub fn dm_activity_ended(session_id: alms_core::SessionId, peer: &str) -> Self {
        Self::new(
            "dm_activity_ended",
            DmActivityEndedData {
                session_id: session_id.0.to_string(),
                peer: peer.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Agent-scoped: a run started on a session belonging to this agent.
    ///
    /// Mirrors the per-session `run_started` event onto the agent-scoped
    /// SSE feed (`GET /agents/{agent_id}/events`) so the web UI's session
    /// sidebar can show an "active" indicator for any session — not just
    /// the currently-viewed one (#856).
    ///
    /// Emitted from the standard run lifecycle so it covers both regular
    /// runs (chat, job, notification) and DM runs.
    pub fn session_activity_started(
        session_id: alms_core::SessionId,
        run_id: RunId,
        agent_id: alms_core::AgentId,
    ) -> Self {
        Self::new(
            "session_activity_started",
            SessionActivityStartedData {
                session_id: session_id.0.to_string(),
                run_id: run_id.0.to_string(),
                agent_id: agent_id.0.to_string(),
                ts: Utc::now(),
            },
        )
    }

    /// Agent-scoped: a run on a session belonging to this agent ended
    /// (completed, failed, or cancelled).
    ///
    /// Pair to [`session_activity_started`].  See that method for the
    /// design rationale.
    pub fn session_activity_ended(
        session_id: alms_core::SessionId,
        run_id: RunId,
        agent_id: alms_core::AgentId,
    ) -> Self {
        Self::new(
            "session_activity_ended",
            SessionActivityEndedData {
                session_id: session_id.0.to_string(),
                run_id: run_id.0.to_string(),
                agent_id: agent_id.0.to_string(),
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
    /// Layered run-config snapshot (#837). `None` for pre-#837 callers
    /// (and for the fallback `run_started` constructor that didn't have
    /// a snapshot to attach). Skipped entirely on the wire when `None`
    /// so the pre-#837 `run_started` payload stays byte-identical for
    /// clients that haven't migrated.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_config: Option<alms_core::ResolvedRunConfig>,
}

#[derive(Debug, Serialize)]
struct TokenDeltaData {
    run_id: String,
    delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
}

/// Wire payload for a `reasoning_delta` SSE event — a chunk of the model's
/// extended-thinking / chain-of-thought trace.
///
/// Provider-neutral: populated from Anthropic `thinking_delta` chunks today;
/// future reasoning-capable providers (OpenAI o-series, DeepSeek R1, xAI
/// Grok, Gemini) will emit the same event type and the UI will render them
/// identically.
#[derive(Debug, Serialize)]
struct ReasoningDeltaData {
    run_id: String,
    /// Reasoning text chunk. The UI is expected to concatenate successive
    /// chunks into a single collapsible block per assistant turn.
    text: String,
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
    /// Subagent task identifier for frontend correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ToolEndData {
    run_id: String,
    tool_invocation_id: String,
    ok: bool,
    result: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent: Option<String>,
    /// Subagent task identifier for frontend correlation.
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
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
    /// Chain-of-thought tokens when the provider reports them separately
    /// (OpenAI o-series via `usage.completion_tokens_details.reasoning_tokens`,
    /// DeepSeek / xAI via flat `usage.reasoning_tokens`). Absent from the
    /// wire when `None` so non-reasoning runs stay byte-identical to pre-#768.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_tokens: Option<u32>,
    /// Anthropic prompt caching (#766): tokens *written* to the cache on
    /// this run. Absent from the wire when `None` so non-cached or
    /// non-Anthropic runs stay byte-identical to pre-#766.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
    /// Anthropic prompt caching (#766): tokens *served from* the cache on
    /// this run. Absent from the wire when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<u32>,
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

/// Wire payload for a `run_queue_position` SSE event — an updated 1-indexed
/// queue position for a still-queued run after the head of the per-agent
/// queue advanced.
///
/// `position` carries the same semantic as `run_created.queued_behind`
/// (number of runs ahead of this one). The frontend can treat the two
/// fields interchangeably for display purposes.
#[derive(Debug, Serialize)]
struct RunQueuePositionData {
    run_id: String,
    session_id: String,
    agent_id: String,
    /// 1-indexed: 1 means "next up" (one run still ahead), 2 means
    /// "one queued ahead plus the running one," etc.
    position: usize,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct SubagentCompletedData {
    session_id: String,
    /// Parent's `invoke_agent` `tool_invocation_id` (#1125, A1-2). Mirrors
    /// `SubagentStartedData::tool_invocation_id`; the frontend's resolver
    /// prefers this id (disambiguates concurrent unnamed subagents) and
    /// falls back to `subagent_session_id`, then `subagent_name`. Omitted
    /// from the wire when the emitter doesn't carry the id (legacy callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_invocation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_name: Option<String>,
    status: String,
    summary: String,
    /// The subagent's own session ID (so the frontend can navigate to it).
    subagent_session_id: String,
    ts: DateTime<Utc>,
}

/// `subagent_started` SSE payload (#1105). Emitted on the parent's stream
/// the moment the coordinator creates the subagent's session, so the UI's
/// SubagentBar can render the "View session" button live for foreground
/// runs. Companion to `subagent_completed`; same `subagent_session_id`
/// the `invoke_agent` tool result carries post-#1104.
#[derive(Debug, Serialize)]
struct SubagentStartedData {
    /// The parent session id this event is being delivered on.
    session_id: String,
    /// Parent's `invoke_agent` `tool_invocation_id`. The frontend's
    /// resolver tries this **first** (it disambiguates concurrent unnamed /
    /// ephemeral subagents that share `subagent_name == null`), then the
    /// subagent session id, then `subagent_name`. Always serialized — a
    /// `subagent_started` without it is useless to the resolver, so the
    /// coordinator skips the whole emit rather than send a `None` id.
    tool_invocation_id: String,
    /// Registered name of the subagent, omitted from the wire when
    /// `None` (ephemeral / unnamed invocation). The frontend's resolver
    /// consults this **last**, after `tool_invocation_id` and the
    /// subagent session id (#1125, A1-2).
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_name: Option<String>,
    /// The subagent's own session id (UUID) — same value the parent's
    /// `invoke_agent` tool result carries.
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
    /// UUID of the agent whose perspective produced this context (#1003).
    agent_id: String,
    /// Human-readable agent name (#1003). `None` for unnamed runtimes.
    /// Serialised as `agent_name: null` so the UI can render a fallback.
    agent_name: Option<String>,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DmMessageData {
    session_id: String,
    from_agent: String,
    from_agent_id: String,
    message: String,
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

#[derive(Debug, Serialize)]
struct DmActivityStartedData {
    session_id: String,
    peer: String,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DmActivityStatusData {
    session_id: String,
    peer: String,
    phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DmActivityEndedData {
    session_id: String,
    peer: String,
    ts: DateTime<Utc>,
}

/// Wire payload for the agent-scoped `session_activity_started` event
/// emitted on `GET /agents/{agent_id}/events` (#856).
#[derive(Debug, Serialize)]
struct SessionActivityStartedData {
    session_id: String,
    run_id: String,
    agent_id: String,
    ts: DateTime<Utc>,
}

/// Wire payload for the agent-scoped `session_activity_ended` event.
#[derive(Debug, Serialize)]
struct SessionActivityEndedData {
    session_id: String,
    run_id: String,
    agent_id: String,
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

    /// `run_started` without a layered config snapshot serializes
    /// **without** the `resolved_config` field — pinned so older clients
    /// that don't know about #837 see a byte-identical pre-#837 wire
    /// shape (no stray `"resolved_config": null`).
    #[test]
    fn test_run_started_without_config_skips_resolved_config_field() {
        let event = SseEventData::run_started(RunId::new(), alms_core::SessionId::new());
        let value = serde_json::to_value(&event).unwrap();
        let data = value
            .get("data")
            .and_then(|v| v.as_object())
            .expect("event.data should be an object");
        assert!(
            data.get("resolved_config").is_none(),
            "resolved_config must be skipped when None — got {value}"
        );
    }

    /// `run_started_with_config` carries the layered snapshot on the
    /// wire under the field names the issue specifies (#837).
    #[test]
    fn test_run_started_with_config_includes_snapshot() {
        let event = SseEventData::run_started_with_config(
            RunId::new(),
            alms_core::SessionId::new(),
            alms_core::ResolvedRunConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
                max_tokens: 4096,
                posture: "guarded".into(),
                debug_mode: false,
                thinking_budget_tokens: 0,
                reasoning_effort: None,
                gemini_thinking_budget: None,
            },
        );
        let value = serde_json::to_value(&event).unwrap();
        let cfg = &value["data"]["resolved_config"];
        assert_eq!(cfg["provider"], "anthropic");
        assert_eq!(cfg["model"], "claude-sonnet-4-20250514");
        assert_eq!(cfg["max_tokens"], 4096);
        assert_eq!(cfg["posture"], "guarded");
        assert_eq!(cfg["debug_mode"], false);
        assert_eq!(cfg["thinking_budget_tokens"], 0);
        // Optional fields skip-serialize when None — pinned so the wire
        // doesn't gain stray nulls for runs that didn't engage Gemini /
        // OpenAI reasoning.
        assert!(cfg.get("reasoning_effort").is_none());
        assert!(cfg.get("gemini_thinking_budget").is_none());
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
            None,
        );

        assert_eq!(event.event_type, "tool_end");
    }

    /// Pins the `subagent_completed` SSE wire key for the parent's
    /// `invoke_agent` invocation id (#1125, A1-2). The frontend resolver
    /// reads `data.tool_invocation_id` (tier 1, ahead of session id and
    /// name), so the Rust field must serialize literally as snake_case
    /// `tool_invocation_id` with no `rename`. A future `rename_all` slip on
    /// `SubagentCompletedData` would silently break the Rust→JS contract the
    /// whole fix depends on; this assertion guards it. Mirrors the
    /// `subagent_started` wire shape.
    #[test]
    fn test_subagent_completed_serializes_tool_invocation_id_key() {
        let tool_id = ToolInvocationId::new();
        let event = SseEventData::subagent_completed(
            alms_core::SessionId::new(),
            Some(tool_id),
            Some("helper".into()),
            "completed",
            "did the thing",
            alms_core::SessionId::new(),
        );

        assert_eq!(event.event_type, "subagent_completed");
        let value = serde_json::to_value(&event).unwrap();
        let data = value
            .get("data")
            .and_then(|v| v.as_object())
            .expect("event.data should be an object");
        assert_eq!(
            data.get("tool_invocation_id").and_then(|v| v.as_str()),
            Some(tool_id.0.to_string().as_str()),
            "tool_invocation_id must serialize under that exact snake_case \
             key — the frontend resolver reads data.tool_invocation_id"
        );
    }

    /// When the emitter doesn't carry the invocation id (legacy callers),
    /// `tool_invocation_id` must be omitted from the wire — not serialized
    /// as `null` — so the pre-#1125 wire shape stays byte-compatible and the
    /// frontend cleanly falls back to session id, then name.
    #[test]
    fn test_subagent_completed_omits_tool_invocation_id_when_none() {
        let event = SseEventData::subagent_completed(
            alms_core::SessionId::new(),
            None,
            Some("helper".into()),
            "completed",
            "did the thing",
            alms_core::SessionId::new(),
        );

        let value = serde_json::to_value(&event).unwrap();
        let data = value
            .get("data")
            .and_then(|v| v.as_object())
            .expect("event.data should be an object");
        assert!(
            data.get("tool_invocation_id").is_none(),
            "tool_invocation_id must be skipped when None — got {value}"
        );
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
        let event = SseEventData::run_warning(
            run_id,
            "DM_EMPTY_REPLY_RETRY",
            "DM agent produced no reply text",
            None,
        );
        assert_eq!(event.event_type, "run_warning");
        assert_eq!(event.data["warning"]["code"], "DM_EMPTY_REPLY_RETRY");
        assert_eq!(
            event.data["warning"]["message"],
            "DM agent produced no reply text"
        );
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
    fn test_dm_message_event() {
        let session_id = alms_core::SessionId::new();
        let agent_id = alms_core::AgentId::new();
        let ts = Utc::now();
        let event = SseEventData::dm_message(
            session_id,
            "alice",
            &agent_id.0.to_string(),
            "Hello Bob!",
            ts,
        );

        assert_eq!(event.event_type, "dm_message");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["from_agent"], "alice");
        assert_eq!(event.data["from_agent_id"], agent_id.0.to_string());
        assert_eq!(event.data["message"], "Hello Bob!");
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
    fn test_dm_activity_started_event() {
        let session_id = alms_core::SessionId::new();
        let event = SseEventData::dm_activity_started(session_id, "researcher");

        assert_eq!(event.event_type, "dm_activity_started");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["peer"], "researcher");
        assert!(event.data["ts"].is_string(), "ts should be a string");

        // Verify ts is RFC3339 format
        let ts_str = event.data["ts"].as_str().unwrap();
        assert!(ts_str.contains("T"), "ts should be ISO8601/RFC3339");
    }

    #[test]
    fn test_dm_activity_status_event() {
        let session_id = alms_core::SessionId::new();

        // Without detail
        let event = SseEventData::dm_activity_status(session_id, "researcher", "calling_llm", None);
        assert_eq!(event.event_type, "dm_activity_status");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["peer"], "researcher");
        assert_eq!(event.data["phase"], "calling_llm");
        assert!(event.data["detail"].is_null());
        assert!(event.data["ts"].is_string(), "ts should be a string");

        // With detail
        let event_with_detail = SseEventData::dm_activity_status(
            session_id,
            "analyst",
            "executing_tools",
            Some("shell_exec, fs_read".into()),
        );
        assert_eq!(event_with_detail.event_type, "dm_activity_status");
        assert_eq!(event_with_detail.data["peer"], "analyst");
        assert_eq!(event_with_detail.data["phase"], "executing_tools");
        assert_eq!(event_with_detail.data["detail"], "shell_exec, fs_read");
    }

    #[test]
    fn test_dm_activity_ended_event() {
        let session_id = alms_core::SessionId::new();
        let event = SseEventData::dm_activity_ended(session_id, "researcher");

        assert_eq!(event.event_type, "dm_activity_ended");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["peer"], "researcher");
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_session_activity_started_event() {
        let session_id = alms_core::SessionId::new();
        let run_id = RunId::new();
        let agent_id = alms_core::AgentId::new();
        let event = SseEventData::session_activity_started(session_id, run_id, agent_id);

        assert_eq!(event.event_type, "session_activity_started");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["run_id"], run_id.0.to_string());
        assert_eq!(event.data["agent_id"], agent_id.0.to_string());
        assert!(event.data["ts"].is_string(), "ts should be a string");
        let ts_str = event.data["ts"].as_str().unwrap();
        assert!(ts_str.contains("T"), "ts should be ISO8601/RFC3339");
    }

    #[test]
    fn test_session_activity_ended_event() {
        let session_id = alms_core::SessionId::new();
        let run_id = RunId::new();
        let agent_id = alms_core::AgentId::new();
        let event = SseEventData::session_activity_ended(session_id, run_id, agent_id);

        assert_eq!(event.event_type, "session_activity_ended");
        assert_eq!(event.data["session_id"], session_id.0.to_string());
        assert_eq!(event.data["run_id"], run_id.0.to_string());
        assert_eq!(event.data["agent_id"], agent_id.0.to_string());
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_context_debug_event() {
        let run_id = RunId::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "prompt"},
            {"role": "user", "content": "hello"},
        ]);
        let tool_names = vec!["shell_exec".to_string(), "fs_read".to_string()];
        let agent_id = "00000000-0000-0000-0000-000000000abc".to_string();

        let event = SseEventData::context_debug(
            run_id,
            messages,
            tool_names,
            500,
            200,
            3,
            agent_id.clone(),
            Some("alpha".to_string()),
        );

        assert_eq!(event.event_type, "context_debug");
        assert_eq!(event.data["run_id"], run_id.0.to_string());
        assert_eq!(event.data["total_tokens"], 500);
        assert_eq!(event.data["system_tokens"], 200);
        assert_eq!(event.data["history_message_count"], 3);
        assert_eq!(event.data["tool_names"].as_array().unwrap().len(), 2);
        assert_eq!(event.data["messages"].as_array().unwrap().len(), 2);
        // #1003: agent attribution must reach the wire.
        assert_eq!(event.data["agent_id"], agent_id);
        assert_eq!(event.data["agent_name"], "alpha");
        assert!(event.data["ts"].is_string(), "ts should be a string");
    }

    #[test]
    fn test_context_debug_event_unnamed_agent() {
        // Tim-style follow-up: unnamed runtimes (e.g. legacy in-memory
        // tests) emit `agent_name = None`. The wire must serialise this
        // as `agent_name: null` so the UI can fall back to a placeholder
        // label rather than break.
        let event = SseEventData::context_debug(
            RunId::new(),
            serde_json::json!([]),
            vec![],
            0,
            0,
            0,
            "00000000-0000-0000-0000-000000000abc".to_string(),
            None,
        );
        assert!(
            event.data["agent_name"].is_null(),
            "agent_name = None must serialise as null, got: {}",
            event.data["agent_name"]
        );
    }
}
