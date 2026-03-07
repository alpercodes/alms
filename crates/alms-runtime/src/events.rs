//! Runtime events emitted during agent execution.
//!
//! These events bridge the runtime to the HTTP/SSE layer without coupling
//! the runtime to axum or HTTP concerns. The gateway consumes these events
//! and converts them to SSE events for clients.

use serde_json::Value;
use uuid::Uuid;

/// Events emitted by the agent runtime during a run.
pub enum RuntimeEvent {
    /// A tool is about to be executed.
    ToolStart {
        invocation_id: Uuid,
        tool: String,
        params: Value,
    },
    /// A tool execution completed (ok=true) or failed (ok=false).
    ToolEnd {
        invocation_id: Uuid,
        ok: bool,
        result: Value,
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
        })
        .unwrap();

        tx.send(RuntimeEvent::ToolEnd {
            invocation_id: id,
            ok: true,
            result: serde_json::json!({"output": "hi"}),
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
        })
        .unwrap();

        // Simulate gateway receiving and approving
        if let Some(RuntimeEvent::ApprovalRequired { decision_tx, .. }) = rx.recv().await {
            decision_tx.send(true).unwrap();
        }

        assert_eq!(decision_rx.await.unwrap(), true);
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
        })
        .unwrap();

        if let Some(RuntimeEvent::ApprovalRequired { decision_tx, .. }) = rx.recv().await {
            decision_tx.send(false).unwrap();
        }

        assert_eq!(decision_rx.await.unwrap(), false);
    }

    #[tokio::test]
    async fn test_approval_dropped_sender() {
        let (_tx, decision_rx) = tokio::sync::oneshot::channel::<bool>();
        // If sender is dropped without sending, receiver gets Err
        drop(_tx);
        assert!(decision_rx.await.is_err());
    }
}
