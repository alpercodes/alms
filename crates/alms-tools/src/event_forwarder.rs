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
}
