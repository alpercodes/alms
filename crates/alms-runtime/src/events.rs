//! Runtime events emitted during agent execution.
//!
//! These events bridge the runtime to the HTTP/SSE layer without coupling
//! the runtime to axum or HTTP concerns. The gateway consumes these events
//! and converts them to SSE events for clients.

use serde_json::Value;
use uuid::Uuid;

// ── Phase constants for RuntimeEvent::Status ──
// Keep these in sync with the frontend (use-session-stream.js and app.js).

/// Agent is assembling the token-budgeted context window.
pub const PHASE_BUILDING_CONTEXT: &str = "building_context";

/// Agent is summarizing old conversation history (sliding-summary strategy).
pub const PHASE_SUMMARIZING: &str = "summarizing";

/// Agent is waiting for the LLM to respond.
pub const PHASE_CALLING_LLM: &str = "calling_llm";

/// Agent is executing tool calls returned by the LLM.
pub const PHASE_EXECUTING_TOOLS: &str = "executing_tools";

/// Events emitted by the agent runtime during a run.
pub enum RuntimeEvent {
    /// A tool is about to be executed.
    ToolStart {
        invocation_id: Uuid,
        tool: String,
        params: Value,
        /// When set, this event originated from a subagent (not the parent).
        source_agent: Option<String>,
    },
    /// A tool execution completed (ok=true) or failed (ok=false).
    ToolEnd {
        invocation_id: Uuid,
        ok: bool,
        result: Value,
        /// When set, this event originated from a subagent (not the parent).
        source_agent: Option<String>,
    },
    /// A chunk of text from the LLM response, for real-time streaming to the UI.
    TokenDelta {
        delta: String,
        /// When set, this delta originated from a subagent's LLM stream.
        source_agent: Option<String>,
    },
    /// A status update indicating the current phase of the agent run.
    ///
    /// Emitted at key moments so the gateway can forward a `status` SSE event
    /// and the UI can show what the agent is doing during dead-air phases
    /// (e.g. "Building context...", "Thinking...", "Running shell_exec...").
    Status {
        /// Phase identifier: `building_context`, `summarizing`, `calling_llm`, `executing_tools`.
        phase: String,
        /// Optional detail (e.g. tool name for `executing_tools`).
        detail: Option<String>,
    },
    /// Approval is required before executing a tool (guarded posture).
    ///
    /// The gateway stores the `decision_tx` and emits an `approval_required`
    /// SSE event. When the user POSTs to `/approvals/{id}`, the gateway sends
    /// `true` (approve) or `false` (deny) to `decision_tx`, which unblocks
    /// `execute_tool_call`.
    ApprovalRequired {
        approval_id: Uuid,
        tool: String,
        params: Value,
        /// Send `true` to allow execution, `false` to deny.
        decision_tx: tokio::sync::oneshot::Sender<bool>,
        /// When set, this approval originated from a subagent.
        source_agent: Option<String>,
    },
}

/// Sender half of the runtime event channel.
pub type RuntimeEventSender = tokio::sync::mpsc::UnboundedSender<RuntimeEvent>;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_tool_start_end_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let id = Uuid::new_v4();

        tx.send(RuntimeEvent::ToolStart {
            invocation_id: id,
            tool: "echo".to_string(),
            params: serde_json::json!({"text": "hi"}),
            source_agent: None,
        })
        .unwrap();

        tx.send(RuntimeEvent::ToolEnd {
            invocation_id: id,
            ok: true,
            result: serde_json::json!({"output": "hi"}),
            source_agent: None,
        })
        .unwrap();

        drop(tx);

        let start = rx.recv().await.unwrap();
        assert!(matches!(start, RuntimeEvent::ToolStart { tool, .. } if tool == "echo"));

        let end = rx.recv().await.unwrap();
        assert!(matches!(end, RuntimeEvent::ToolEnd { ok: true, .. }));

        assert!(rx.recv().await.is_none()); // channel closed
    }

    #[tokio::test]
    async fn test_approval_approve() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();

        tx.send(RuntimeEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            tool: "shell".to_string(),
            params: serde_json::json!({}),
            decision_tx,
            source_agent: None,
        })
        .unwrap();

        // Simulate gateway receiving and approving
        if let Some(RuntimeEvent::ApprovalRequired { decision_tx, .. }) = rx.recv().await {
            decision_tx.send(true).unwrap();
        }

        assert!(decision_rx.await.unwrap());
    }

    #[tokio::test]
    async fn test_approval_deny() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();

        tx.send(RuntimeEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            tool: "shell".to_string(),
            params: serde_json::json!({}),
            decision_tx,
            source_agent: None,
        })
        .unwrap();

        if let Some(RuntimeEvent::ApprovalRequired { decision_tx, .. }) = rx.recv().await {
            decision_tx.send(false).unwrap();
        }

        assert!(!decision_rx.await.unwrap());
    }

    #[tokio::test]
    async fn test_approval_dropped_sender() {
        let (_tx, decision_rx) = tokio::sync::oneshot::channel::<bool>();
        // If sender is dropped without sending, receiver gets Err
        drop(_tx);
        assert!(decision_rx.await.is_err());
    }

    #[tokio::test]
    async fn test_status_event_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();

        tx.send(RuntimeEvent::Status {
            phase: PHASE_CALLING_LLM.to_string(),
            detail: None,
        })
        .unwrap();

        tx.send(RuntimeEvent::Status {
            phase: PHASE_EXECUTING_TOOLS.to_string(),
            detail: Some("shell_exec".to_string()),
        })
        .unwrap();

        drop(tx);

        let first = rx.recv().await.unwrap();
        assert!(
            matches!(&first, RuntimeEvent::Status { phase, detail } if phase == PHASE_CALLING_LLM && detail.is_none())
        );

        let second = rx.recv().await.unwrap();
        assert!(
            matches!(&second, RuntimeEvent::Status { phase, detail } if phase == PHASE_EXECUTING_TOOLS && detail.as_deref() == Some("shell_exec"))
        );

        assert!(rx.recv().await.is_none());
    }
}
