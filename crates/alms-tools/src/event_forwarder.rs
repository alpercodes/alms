//! Type-erased event forwarding trait for subagent tool events.
//!
//! Defined here in `alms-tools` so that [`SubagentDispatcher`](crate::SubagentDispatcher)
//! can accept an event sink without depending on `alms-runtime`'s `RuntimeEvent` enum
//! (which contains `oneshot::Sender<bool>` for approval flow and belongs in the runtime).
//!
//! The gateway provides a concrete `RuntimeEventForwarder` that wraps a
//! `RuntimeEventSender` and maps each `forward_*` call to the corresponding
//! `RuntimeEvent` variant.

use serde_json::Value;
use uuid::Uuid;

/// Terminal outcome of a subagent's own run (#1180), forwarded once at
/// end-of-run via [`EventForwarder::forward_run_terminal`] so the subagent's
/// own-session SSE stream gets the matching terminal event and its in-flight
/// state is cleaned up.
#[derive(Debug, Clone)]
pub enum SubagentRunOutcome {
    /// Completed successfully — `usage` populates the `run_finished` event.
    Completed { usage: alms_core::TokenUsage },
    /// Failed — `error` populates the `run_error` event.
    Failed { error: String },
    /// Cancelled — emits `run_cancelled`.
    Cancelled,
}

/// Type-erased event sink for subagent tool activity.
///
/// Implementors forward events to whatever transport the caller uses
/// (e.g. `RuntimeEventSender` in the gateway, or a no-op in tests).
///
/// All methods are fire-and-forget -- failures are silently ignored.
/// `ApprovalRequired` is NOT forwarded through this trait because it requires
/// a `oneshot::Sender<bool>` that cannot be type-erased safely.
pub trait EventForwarder: Send + Sync + std::fmt::Debug {
    /// A tool is about to be executed.
    fn forward_tool_start(
        &self,
        invocation_id: Uuid,
        tool: String,
        params: Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    );

    /// A tool execution completed (ok=true) or failed (ok=false).
    fn forward_tool_end(
        &self,
        invocation_id: Uuid,
        ok: bool,
        result: Value,
        source_agent: Option<String>,
        task_id: Option<String>,
    );

    /// A chunk of text from the LLM response stream.
    fn forward_token_delta(&self, delta: String, source_agent: Option<String>);

    /// A chunk of reasoning / extended-thinking text from the LLM stream.
    ///
    /// Provider-neutral: Anthropic's `thinking_delta`, future OpenAI /
    /// DeepSeek / Gemini reasoning streams all surface here. Default is a
    /// no-op so implementers that don't yet care about reasoning don't
    /// have to opt in explicitly.
    fn forward_reasoning_delta(&self, _text: String, _source_agent: Option<String>) {}

    /// The streamed partial for the current turn is being retracted before a
    /// buffered (non-streaming) response re-streams (#1162 sym-2). Any forwarder
    /// that paints partials from `forward_token_delta` / `forward_reasoning_delta`
    /// must surface this so the UI drops the abandoned partial before the
    /// re-emit. Required (no default): silently dropping it double-renders the
    /// turn on any stream where the partial was painted — the #1180 subagent
    /// self-session stream is exactly such a stream.
    fn forward_stream_reset(&self);

    /// A status update indicating the current phase of the agent run.
    fn forward_status(&self, phase: String, detail: Option<String>);

    /// A non-fatal warning condition during the run.
    fn forward_warning(&self, code: String, message: String, source_agent: Option<String>);

    /// The subagent's own run reached a terminal state (#1180). Called ONCE at
    /// end-of-run on the self-session forwarder only, so its own-session SSE
    /// stream gets the matching terminal event (`run_finished` / `run_error` /
    /// `run_cancelled`) — sealing the fullscreen view's assistant bubble and
    /// clearing its spinner, mirroring a top-level run. Required (no default) so
    /// no forwarder silently omits it. Parent/relay forwarders no-op it: the
    /// parent already gets `subagent_completed` via the relay, and the #1046
    /// guard keeps the coordinator's parent path from double-broadcasting.
    fn forward_run_terminal(&self, outcome: SubagentRunOutcome);

    /// A subagent's session has just been created (#1105). The gateway
    /// converts this into a `subagent_started` SSE event so the UI's
    /// SubagentBar can render the "View session" button live during a
    /// foreground `invoke_agent` run instead of only at tool_end.
    ///
    /// `tool_invocation_id` is the parent's `invoke_agent` invocation id —
    /// the UI's resolver falls back to it for ephemeral / unnamed
    /// subagents where `subagent_name` is `None`. `subagent_session_id`
    /// is the new session row's UUID.
    ///
    /// `background` is `true` when this event originated from a background
    /// (`dispatch_background`) invocation and `false` for the foreground
    /// (`dispatch`) path. The live SSE event is identical either way, but
    /// the gateway uses this flag to persist a durable `subagent_started`
    /// lifecycle marker **only** for the foreground path (#1125, A1-1):
    /// background subagents are already reload-safe because their
    /// `{task_id, session_id}` tool result is persisted to history, whereas
    /// a foreground subagent's session id reaches the client solely via the
    /// (non-durable) live SSE event log and is lost on reload. Default is a
    /// no-op so implementers that don't yet care about the event don't have
    /// to opt in.
    fn forward_subagent_started(
        &self,
        _tool_invocation_id: Uuid,
        _subagent_name: Option<String>,
        _subagent_session_id: Uuid,
        _background: bool,
    ) {
    }
}
