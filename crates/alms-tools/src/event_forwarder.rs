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

    /// A status update indicating the current phase of the agent run.
    fn forward_status(&self, phase: String, detail: Option<String>);

    /// A non-fatal warning condition during the run.
    fn forward_warning(&self, code: String, message: String, source_agent: Option<String>);

    /// A subagent's session has just been created (#1105). The gateway
    /// converts this into a `subagent_started` SSE event so the UI's
    /// SubagentBar can render the "View session" button live during a
    /// foreground `invoke_agent` run instead of only at tool_end.
    ///
    /// `tool_invocation_id` is the parent's `invoke_agent` invocation id —
    /// the UI's resolver falls back to it for ephemeral / unnamed
    /// subagents where `subagent_name` is `None`. `subagent_session_id`
    /// is the new session row's UUID. Default is a no-op so implementers
    /// that don't yet care about the event don't have to opt in.
    fn forward_subagent_started(
        &self,
        _tool_invocation_id: Uuid,
        _subagent_name: Option<String>,
        _subagent_session_id: Uuid,
    ) {
    }
}
