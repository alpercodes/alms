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
        /// When set, identifies the subagent task that produced this event.
        task_id: Option<String>,
    },
    /// A tool execution completed (ok=true) or failed (ok=false).
    ToolEnd {
        invocation_id: Uuid,
        ok: bool,
        result: Value,
        /// When set, this event originated from a subagent (not the parent).
        source_agent: Option<String>,
        /// When set, identifies the subagent task that produced this event.
        task_id: Option<String>,
    },
    /// A chunk of text from the LLM response, for real-time streaming to the UI.
    TokenDelta {
        delta: String,
        /// When set, this delta originated from a subagent's LLM stream.
        source_agent: Option<String>,
    },
    /// A chunk of *reasoning* text — provider-neutral "extended thinking" or
    /// "chain-of-thought" content that the model emits before (or alongside)
    /// its user-visible answer.
    ///
    /// Populated from Anthropic's `thinking_delta` stream events when the
    /// extended-thinking feature is enabled; future providers (OpenAI
    /// o-series, DeepSeek R1, xAI Grok, Gemini) will emit the same variant.
    ///
    /// The gateway forwards these as `reasoning_delta` SSE events so the UI
    /// can render them in a collapsible panel under the assistant turn.
    /// Reasoning text is NOT replayed back to the LLM in subsequent turns
    /// (standard Anthropic mode does not require it, and the interleaved-
    /// thinking beta is out of scope here).
    ReasoningDelta {
        text: String,
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
    /// A non-fatal warning condition during the run.
    ///
    /// The gateway converts this to a `run_warning` SSE event so the
    /// operator/UI is informed. The run continues after the warning.
    Warning {
        /// Machine-readable warning code (e.g. `DM_TEXT_ONLY_RETRY`).
        code: String,
        /// Human-readable warning message.
        message: String,
        /// When set, this warning originated from a subagent (not the parent).
        source_agent: Option<String>,
    },
    /// Debug snapshot of the full context window sent to the LLM.
    ///
    /// Only emitted when `debug_mode` is enabled on the agent config.
    /// The gateway converts this to a `context_debug` SSE event so the
    /// web UI can display exactly what the LLM sees.
    ///
    /// Subagent emissions are suppressed by the coordinator
    /// (`alms-coordinator/src/lib.rs`) — the parent's session stream
    /// only carries `ContextDebug` events from the top-level agent's
    /// turn. For DM sessions (#1003) the runtime emits one event per
    /// turn carrying the perspective of whichever agent is about to
    /// run, with `agent_id` / `agent_name` populated so the UI can
    /// attribute the snapshot to the correct DM participant.
    ContextDebug {
        /// The assembled messages array (system prompt, history, user input).
        messages: Value,
        /// Names of tools available to the LLM.
        tool_names: Vec<String>,
        /// Estimated total token count of the context window.
        total_tokens: usize,
        /// Estimated token count of the system prompt alone.
        system_tokens: usize,
        /// Number of history messages included in the context.
        history_message_count: usize,
        /// UUID of the agent whose perspective produced this context.
        ///
        /// Always populated. For DM sessions the field lets the UI
        /// attribute the panel to the correct participant (a single DM
        /// session sees one `ContextDebug` per turn, alternating
        /// between the two agents). For webchat sessions this is
        /// just the active agent's id — informational only, since
        /// there's only one possible source.
        agent_id: String,
        /// Human-readable name of the agent (`AgentRuntime::agent_name`).
        ///
        /// `None` for unnamed runtimes (legacy in-memory tests). When
        /// `Some(name)`, the UI uses it as the panel header label.
        /// Wire-shape compat: `None` is serialised as
        /// `agent_name: null`, which the UI falls back to "agent" for.
        agent_name: Option<String>,
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
            task_id: None,
        })
        .unwrap();

        tx.send(RuntimeEvent::ToolEnd {
            invocation_id: id,
            ok: true,
            result: serde_json::json!({"output": "hi"}),
            source_agent: None,
            task_id: None,
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

    #[tokio::test]
    async fn test_warning_event_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();

        tx.send(RuntimeEvent::Warning {
            code: "DM_TEXT_ONLY_RETRY".to_string(),
            message: "Agent responded with text only in DM — retrying".to_string(),
            source_agent: None,
        })
        .unwrap();

        drop(tx);

        let event = rx.recv().await.unwrap();
        assert!(
            matches!(&event, RuntimeEvent::Warning { code, message, source_agent }
                if code == "DM_TEXT_ONLY_RETRY"
                && message.contains("text only")
                && source_agent.is_none())
        );

        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_context_debug_event_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();

        let messages = serde_json::json!([
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Hello"},
        ]);

        tx.send(RuntimeEvent::ContextDebug {
            messages: messages.clone(),
            tool_names: vec!["shell_exec".to_string(), "fs_read".to_string()],
            total_tokens: 150,
            system_tokens: 100,
            history_message_count: 0,
            agent_id: "00000000-0000-0000-0000-000000000001".to_string(),
            agent_name: Some("alpha".to_string()),
        })
        .unwrap();

        drop(tx);

        let event = rx.recv().await.unwrap();
        match event {
            RuntimeEvent::ContextDebug {
                messages: m,
                tool_names,
                total_tokens,
                system_tokens,
                history_message_count,
                agent_id,
                agent_name,
            } => {
                assert_eq!(m, messages);
                assert_eq!(tool_names, vec!["shell_exec", "fs_read"]);
                assert_eq!(total_tokens, 150);
                assert_eq!(system_tokens, 100);
                assert_eq!(history_message_count, 0);
                assert_eq!(agent_id, "00000000-0000-0000-0000-000000000001");
                assert_eq!(agent_name.as_deref(), Some("alpha"));
            }
            _ => panic!("Expected ContextDebug event"),
        }

        assert!(rx.recv().await.is_none());
    }
}
