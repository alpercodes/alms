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

/// Agent is summarizing old conversation history (compact strategy, #869 —
/// renamed from `sliding-summary` but the SSE phase string is unchanged).
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
    /// The current LLM call abandoned its partial stream and is restarting as
    /// a buffered (non-streaming) call — discard any partial text already
    /// painted for this run.
    ///
    /// `call_llm_with_cancellation` attempts streaming first and falls back to
    /// a fresh buffered `complete()` when the stream fails mid-flight (e.g.
    /// minimax-m3 on OpenRouter, whose stream reliably faults — see #1162 /
    /// #1163). By the time it falls back, any `TokenDelta` / `ReasoningDelta`
    /// chunks emitted before the failure have already been forwarded to the UI
    /// and painted as a *partial* live render. The buffered retry then returns
    /// the **full** response, which is delivered separately (for DM runs as the
    /// `dm_message` bubble), so the abandoned partial surfaced as the
    /// cut-off-then-full duplicate (#1162 sym-2).
    ///
    /// This event is emitted **only** when the failed stream actually emitted
    /// ≥1 delta, immediately before the buffered `content` / `reasoning` are
    /// re-emitted as fresh `TokenDelta` / `ReasoningDelta`. The gateway
    /// forwards it as a `stream_reset` SSE event and, unlike the ephemeral
    /// `token_delta` it retracts, **persists** it to the run and session event
    /// logs (it is not in `send_event`'s `is_ephemeral` set). Persistence is
    /// load-bearing: it gives the reasoning-rehydration endpoint
    /// (`get_run_reasoning`) a durable boundary to drop every same-run
    /// `reasoning_delta` at or before it, so a mid-fallback reconnect
    /// rehydrates only the re-emitted full reasoning. Live, the UI drops the
    /// run's partial so the re-emitted full text rebuilds a single clean
    /// render that matches reload (live === reload).
    StreamReset {
        /// When set, this reset originated from a subagent's LLM stream.
        /// Mirrors the `source_agent` gating on `TokenDelta` / `ReasoningDelta`
        /// so the UI suppresses subagent stream resets the same way it
        /// suppresses subagent deltas.
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
        /// Machine-readable warning code (e.g. `DM_EMPTY_REPLY_RETRY`).
        code: String,
        /// Human-readable warning message.
        message: String,
        /// When set, this warning originated from a subagent (not the parent).
        source_agent: Option<String>,
    },
    /// A subagent's session has just been created and the parent's
    /// `invoke_agent` tool call is now blocked waiting for it to finish.
    ///
    /// Emitted from the coordinator's `spawn_subagent` path the moment the
    /// subagent's session row exists, AFTER the parent's `ToolStart` for
    /// `invoke_agent` has been emitted to the parent's stream and BEFORE
    /// any nested `ToolStart` from inside the subagent runs. The gateway
    /// forwards it as the `subagent_started` SSE event so the UI's
    /// SubagentBar can render the "View session" button live during the
    /// foreground run instead of only at `tool_end`. See #1105.
    ///
    /// Idempotent on the client: background subagents already get their
    /// `session_id` through the `invoke_agent` tool result and via the
    /// `subagent_completed` event — `setSubagentSessionId` writes the same
    /// value either way, so firing for background paths too keeps the
    /// event semantics consistent without changing visible behaviour.
    SubagentStarted {
        /// Parent's `invoke_agent` `tool_invocation_id`. The frontend
        /// resolves the SubagentBar entry by `subagent_name` first and
        /// falls back to this id for ephemeral / unnamed subagents.
        tool_invocation_id: Uuid,
        /// Registered name of the subagent, or `None` for ephemeral
        /// invocations spawned without `--name`.
        subagent_name: Option<String>,
        /// UUID of the subagent's session — the row where the subagent
        /// persists its own messages (#1104). Same value the parent's
        /// `invoke_agent` tool result carries; emitted early so the UI
        /// can drill down before the parent's tool_end arrives.
        subagent_session_id: alms_core::SessionId,
        /// `true` when the invocation was `dispatch_background`, `false`
        /// for the foreground `dispatch` path. The live SSE event is
        /// identical either way, but the gateway persists a durable
        /// `subagent_started` lifecycle marker **only** for the foreground
        /// path (#1125, A1-1) — background subagents are already reload-safe
        /// via their persisted `{task_id, session_id}` tool result, while a
        /// foreground subagent's session id otherwise survives only on the
        /// non-durable live SSE event log and is lost on reload.
        background: bool,
    },
    /// Coarse status signal describing what a subagent is doing right now,
    /// for the parent's **Subagent status bar** (#1180 follow-up).
    ///
    /// NEVER emitted by the runtime itself. The coordinator's subagent→parent
    /// relay (`alms-coordinator/src/lib.rs`) synthesises it from the
    /// subagent's own `ReasoningDelta` / `TokenDelta` / `ToolStart` /
    /// `ToolEnd` events, deduplicated so that only *transitions* between
    /// activity kinds are forwarded — the subagent's actual reasoning/token
    /// TEXT and tool params/results are deliberately NOT forwarded to the
    /// parent (they bloat the parent's stream and session log; the full
    /// content lives on the subagent's OWN session stream, #1184).
    ///
    /// The gateway converts this to an **ephemeral** `subagent_activity` SSE
    /// event (never persisted — see `send_event`'s `is_ephemeral` set): the
    /// bar is a live status surface that re-derives from fresh events after a
    /// reload.
    SubagentActivity {
        /// Activity kind: one of the `alms_tools::subagent_activity_kind`
        /// constants (`reasoning`, `writing`, `tool_start`, `tool_end`).
        kind: String,
        /// Tool name, populated only for `tool_start`.
        tool: Option<String>,
        /// The subagent's tool invocation id, populated for the tool kinds
        /// (`tool_start` / `tool_end`) — carried from the subagent's own
        /// `ToolStart` / `ToolEnd` events. The UI counts DISTINCT ids into
        /// the chip's `toolsUsed` (#1190): an attach-time snapshot replay of
        /// the in-progress `tool_start` re-sends the SAME id (recognised, not
        /// recounted), while parallel invocations of the same tool
        /// (`run_tool_calls_parallel`) carry distinct ids and each count —
        /// which no name-based heuristic can distinguish.
        tool_invocation_id: Option<uuid::Uuid>,
        /// The PARENT `invoke_agent` tool-invocation-id — the chip-resolution
        /// correlator (#1190 Codex P2). Unnamed subagent chips are keyed by
        /// this id (it is the same id `subagent_started` carries), while the
        /// backend `source_agent` label is task-id-derived; without the
        /// correlator, a label-based first-match resolver can persistently
        /// attach one concurrent unnamed subagent's status to another's chip
        /// on attach-time snapshot replay. Distinct from `tool_invocation_id`
        /// above, which is the CHILD tool's id (for tool counting).
        parent_tool_invocation_id: Option<uuid::Uuid>,
        /// The subagent label this status belongs to. Always `Some` in
        /// practice — the UI routes the signal to the matching status-bar
        /// chip by this label.
        source_agent: String,
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
            code: "DM_EMPTY_REPLY_RETRY".to_string(),
            message: "Agent produced no reply text in DM — retrying".to_string(),
            source_agent: None,
        })
        .unwrap();

        drop(tx);

        let event = rx.recv().await.unwrap();
        assert!(
            matches!(&event, RuntimeEvent::Warning { code, message, source_agent }
                if code == "DM_EMPTY_REPLY_RETRY"
                && message.contains("no reply text")
                && source_agent.is_none())
        );

        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_subagent_started_event_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let inv_id = Uuid::new_v4();
        let sub_session = alms_core::SessionId::new();

        tx.send(RuntimeEvent::SubagentStarted {
            tool_invocation_id: inv_id,
            subagent_name: Some("reviewer".to_string()),
            subagent_session_id: sub_session,
            background: false,
        })
        .unwrap();

        drop(tx);

        let event = rx.recv().await.unwrap();
        match event {
            RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id,
                background,
            } => {
                assert_eq!(tool_invocation_id, inv_id);
                assert_eq!(subagent_name.as_deref(), Some("reviewer"));
                assert_eq!(subagent_session_id, sub_session);
                assert!(!background);
            }
            _ => panic!("Expected SubagentStarted event"),
        }

        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_subagent_started_event_unnamed() {
        let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let inv_id = Uuid::new_v4();
        let sub_session = alms_core::SessionId::new();

        tx.send(RuntimeEvent::SubagentStarted {
            tool_invocation_id: inv_id,
            subagent_name: None,
            subagent_session_id: sub_session,
            background: true,
        })
        .unwrap();

        drop(tx);

        let event = rx.recv().await.unwrap();
        match event {
            RuntimeEvent::SubagentStarted {
                subagent_name,
                background,
                ..
            } => {
                assert!(subagent_name.is_none());
                assert!(background);
            }
            _ => panic!("Expected SubagentStarted event"),
        }
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
