//! send_message tool -- sends a peer message from one agent to another.
//!
//! The tool returns immediately with a delivery confirmation; the sender's
//! loop is never blocked on a reply. That is the *only* thing "asynchronous"
//! means here -- delivery itself is immediate, and the exchange is not a
//! queue the sender has to drain:
//!
//! - `MessageBus::send` persists the message to the shared DM session and
//!   emits a `RunTrigger` for the recipient, so the recipient is *invoked*
//!   to process it (`crates/alms-coordinator/src/message_bus/bus.rs`).
//! - The recipient's reply (its final assistant text, delivered by the DM
//!   completion gate under #1154) goes back through the same path, so the
//!   original sender is invoked in turn on the DM session.
//! - If the recipient ends the conversation instead, `end_conversation`
//!   emits a `ConversationEnded` trigger for the peer unconditionally --
//!   and for an end that *completed*, `run_trigger_loop` starts the run, so
//!   the sender is invoked for that outcome too.
//!
//! Nothing in that loop is reached by polling `read_messages`, which is why
//! the tool description (and the delivered-result note below) say so out
//! loud. See #1111 / #1296 and `docs/layer2-peer-messaging-design.md` § 9.1.
//!
//! ## The exception the agent is deliberately not told about (#1258)
//!
//! The bus emits that third trigger unconditionally, but the trigger loop
//! does not always act on it: `run_trigger_loop` starts **no run on the
//! trigger's own target** when the end `is_interrupted()` -- `UserCancelled`,
//! or `Errored { interrupted: true }` (the peer's run was cancelled or died
//! mid-turn). The unconditional emit is therefore true of the bus and not of
//! the outcome; the suppression is one crate away, in
//! `crates/alms-gateway/src/runs/notifications.rs`, under the heading
//! "Consequence: an interrupted end is invisible to the agent".
//!
//! Read that `run_targets` branch in **evaluation order**, because its first
//! arm is an exception to the suppression rather than a case of it: a
//! resolved #1198 job episode routes a continuation onto the *job* session
//! and wins over the interrupted-end check, because "dropping it would stall
//! the job until its deadline". The silent population is therefore narrower
//! than "every interrupted end" -- it is an interrupted end that resolves no
//! open job episode.
//!
//! [`SendMessageTool::description`] and [`DELIVERED_NOTE`] state the
//! notification **without** that qualification. That is a choice, not an
//! oversight:
//!
//! - There is no agent-available remedy. The `dm_ended` bus record is
//!   empty-text, so `dm_filter`'s `is_synthetic_marker` hides it from
//!   `read_messages` *and* `read_session`; the `dm_ended_notification`
//!   marker is `Role::System` + synthetic and is stripped before the
//!   provider. No observation an agent can make distinguishes "the reply is
//!   still coming" from "the end was interrupted".
//! - So the caveat could not change what the agent does -- except in one
//!   direction. "You will usually be invoked" leaves exactly one lever
//!   (check), which is the polling instinct #1111 exists to close, and which
//!   provably cannot detect this case.
//! - The job-episode exception above cuts the same way. It *adds* a case in
//!   which the agent is invoked, so the set of ends that reach it silently is
//!   smaller than this section's premise alone implies -- which makes the
//!   unqualified strings more accurate than a hedged version would be, not
//!   less.
//!
//! A caveat is worth its tokens only if it changes what the reader can do.
//! Here it does for the human reader and cannot for the model, so it lives
//! at this level and not in the strings. **Revisit if that stops being
//! true**: if an interrupted end ever grows an agent-visible signal (the
//! machinery would be `persist_error_marker`, #874, which survives the strip
//! pass), the absolute wording becomes actionably wrong and should be
//! qualified.

use crate::message_sender::{MessageSender, SendError};
use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

/// Note attached to a successful delivery, re-read by the agent when it
/// plans its next step.
///
/// Carries the same mental model as the tool description: the recipient is
/// being *invoked*, the sender will be invoked back, and `read_messages` is
/// not a waiting room (#1111).
const DELIVERED_NOTE: &str = "Delivered. The recipient is now being invoked in your shared DM \
     session to process this. Their reply triggers a new run there and you will be invoked to \
     handle it; if they end the conversation instead, you are notified then. Do NOT poll \
     read_messages waiting for the reply -- it will not arrive any sooner, and the system \
     resumes you when it does.";

/// Built-in tool that sends a message to another registered agent.
///
/// The message is delivered to the shared DM session and a run is
/// triggered so the recipient can process it. The sender does NOT block
/// waiting for a response.
#[derive(Debug)]
pub struct SendMessageTool {
    sender: Arc<dyn MessageSender>,
    sender_agent_id: AgentId,
    sender_name: String,
    /// Session manager for agent name resolution.
    session_manager: Arc<SessionManager>,
    /// The session the sender is currently running in.
    ///
    /// Passed to `MessageSender::send()` so the MessageBus can track it
    /// as the "source session" for notification routing.
    sender_session_id: SessionId,
    /// The current DM peer, when this tool is registered inside a DM run
    /// (#1154 implicit replies).
    ///
    /// Under implicit DM replies the agent's final assistant text IS the
    /// reply to its DM peer — `send_message` aimed at the current peer is
    /// misuse. To avoid double-delivery, such calls are folded: the tool
    /// returns a non-error result explaining the implicit-reply contract
    /// and does NOT deliver anything. `send_message` remains valid for
    /// any *other* (non-peer) recipient during a DM run.
    dm_peer: Option<String>,
}

impl SendMessageTool {
    pub fn new(
        sender: Arc<dyn MessageSender>,
        sender_agent_id: AgentId,
        sender_name: String,
        session_manager: Arc<SessionManager>,
        sender_session_id: SessionId,
    ) -> Self {
        Self {
            sender,
            sender_agent_id,
            sender_name,
            session_manager,
            sender_session_id,
            dm_peer: None,
        }
    }

    /// Set the current DM peer so `send_message` calls aimed at the peer
    /// are folded instead of delivered (see [`Self::dm_peer`]).
    pub fn with_dm_peer(mut self, peer: Option<String>) -> Self {
        self.dm_peer = peer;
        self
    }

    /// Build the folded (not-delivered) result for a `send_message` call
    /// aimed at the current DM peer.
    fn folded_peer_result(peer: &str) -> Value {
        serde_json::json!({
            "delivered": false,
            "folded": true,
            "note": format!(
                "You are already in a direct conversation with '{peer}'. Your \
                 final reply text is delivered to them automatically — do NOT \
                 use send_message for this. The message was NOT sent; include \
                 the content in your final reply text instead."
            ),
        })
    }
}

#[async_trait::async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a direct message to another agent. This is the PEER \
         relationship: they run on their own and their reply is theirs. \
         Sending triggers an LLM run in your shared DM session -- they \
         are actively invoked to process your message, and may think, use \
         tools, or do work before replying. Their reply triggers a run for \
         you in that same DM session: the system invokes you again to handle \
         it. If they end the conversation instead of replying, you are \
         notified then. 'Asynchronous' means your current run is not blocked \
         waiting for the reply -- it does NOT mean delivery is deferred, and \
         it does NOT mean you have to poll: calling read_messages will not \
         make a reply arrive any sooner. Use this for peer work -- asking for \
         a review, sharing an update, requesting help. For a subordinate task \
         whose result you need inline, use invoke_agent. Do NOT use this to \
         reply to an agent you are already in a direct conversation with -- \
         your final reply text is delivered to them automatically."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Name of the target agent (must be registered via `alms agent create`)"
                },
                "message": {
                    "type": "string",
                    "description": "The message to send"
                }
            },
            "required": ["to", "message"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let to = params
            .get("to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'to' is required and must be non-empty".into())
            })?;

        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters(
                    "'message' is required and must be non-empty".into(),
                )
            })?;

        // Fold sends aimed at the current DM peer (#1154 design default #2):
        // the agent's final reply text is delivered implicitly, so a
        // `send_message` to the peer would double-deliver. Checked before
        // registry resolution so the fold works even without a SQLite store.
        if let Some(ref peer) = self.dm_peer
            && to.eq_ignore_ascii_case(peer)
        {
            warn!(
                peer = %peer,
                "send_message aimed at the current DM peer — folded (implicit reply, #1154)"
            );
            return Ok(Self::folded_peer_result(peer));
        }

        // Resolve recipient agent from registry
        let store = self.session_manager.store().ok_or_else(|| {
            SandboxError::Io("No agent store available (SQLite not configured)".into())
        })?;

        let recipient = store
            .load_agent_by_name(to)
            .map_err(|e| SandboxError::Io(format!("Failed to look up agent '{to}': {e}")))?;

        let recipient = match recipient {
            Some(r) => r,
            None => {
                return Ok(serde_json::json!({
                    "error": format!("Agent '{to}' not found. Use list_agents to see available agents."),
                }));
            }
        };

        // Defense-in-depth: the registry lookup may resolve an alias /
        // differently-cased name to the canonical record — re-check the
        // canonical name against the DM peer so a non-exact spelling
        // cannot sneak a double-delivery past the pre-resolution fold.
        if let Some(ref peer) = self.dm_peer
            && recipient.name.eq_ignore_ascii_case(peer)
        {
            warn!(
                peer = %peer,
                resolved = %recipient.name,
                "send_message resolved to the current DM peer — folded (implicit reply, #1154)"
            );
            return Ok(Self::folded_peer_result(peer));
        }

        match self
            .sender
            .send(
                &self.sender_name,
                self.sender_agent_id,
                to,
                recipient.id,
                message,
                Some(self.sender_session_id),
            )
            .await
        {
            Ok(receipt) => Ok(serde_json::json!({
                "delivered": true,
                "dm_session_id": receipt.session_id.0.to_string(),
                "note": DELIVERED_NOTE,
            })),
            Err(SendError::DepthExceeded) => {
                warn!("send_message depth exceeded for {to}");
                Ok(serde_json::json!({
                    "error": "Message depth exceeded maximum -- possible loop detected.",
                }))
            }
            Err(SendError::SelfMessage) => Ok(serde_json::json!({
                "error": "Cannot send a message to yourself.",
            })),
            Err(e) => Err(SandboxError::Io(format!("Message delivery failed: {e}"))),
        }
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SendMessageTool requires a real MessageSender implementation and
    // a SessionManager with SQLite store for agent lookup. Full integration
    // tests live in alms-coordinator's message_bus tests. Here we test
    // parameter validation only.

    /// Minimal mock that always returns an error (never called in param tests).
    #[derive(Debug)]
    struct NoopSender;

    #[async_trait::async_trait]
    impl MessageSender for NoopSender {
        async fn send(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            Err(SendError::Internal("noop".into()))
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Err(SendError::Internal("noop".into()))
        }
    }

    fn make_tool() -> SendMessageTool {
        let mgr = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let sender: Arc<dyn MessageSender> = Arc::new(NoopSender);
        SendMessageTool::new(
            sender,
            AgentId::new(),
            "test-sender".into(),
            mgr,
            SessionId::new(),
        )
    }

    #[tokio::test]
    async fn test_missing_to_is_error() {
        let tool = make_tool();
        let err = tool
            .execute(serde_json::json!({ "message": "hi" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_missing_message_is_error() {
        let tool = make_tool();
        let err = tool
            .execute(serde_json::json!({ "to": "bob" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_no_store_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "to": "bob", "message": "hi" }))
            .await;
        // Should fail because no SQLite store is configured
        assert!(result.is_err());
    }

    #[test]
    fn test_schema_has_required_fields() {
        let tool = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "to"));
        assert!(required.iter().any(|v| v == "message"));
    }

    // ---- DM-peer fold tests (#1154 implicit replies) ----
    //
    // The NoopSender returns an error from `send()`, and `make_tool()` has
    // no SQLite store — so any of these tests passing with `Ok(...)` proves
    // the fold short-circuited BEFORE both registry resolution and delivery.

    #[tokio::test]
    async fn test_send_to_current_dm_peer_is_folded_not_delivered() {
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await
            .expect("fold must be a non-error result");
        assert_eq!(result["delivered"], false);
        assert_eq!(result["folded"], true);
        assert!(
            result.get("error").is_none(),
            "fold must NOT be an error result — the agent should not retry"
        );
    }

    #[tokio::test]
    async fn test_send_to_current_dm_peer_fold_is_case_insensitive() {
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "Alice", "message": "hi" }))
            .await
            .expect("case-insensitive fold must be a non-error result");
        assert_eq!(result["folded"], true);
    }

    #[tokio::test]
    async fn test_send_to_non_peer_during_dm_is_not_folded() {
        // With a DM peer set, a send to a DIFFERENT agent must proceed past
        // the fold — here it hits the missing-store error, proving the fold
        // did not short-circuit.
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "charlie", "message": "hi" }))
            .await;
        assert!(
            result.is_err(),
            "non-peer send must proceed to resolution/delivery (and fail on \
             the missing store here), not be folded"
        );
    }

    // ---- Delivered-result shape (#1111) ----
    //
    // The wording of the note is prose and deliberately not asserted; what
    // is pinnable is that a successful delivery still *carries* one. The
    // note is the only place the agent is told, at planning time, not to
    // poll `read_messages` -- a refactor that dropped the field would
    // silently take that back, and every other test in this file stops
    // short of the delivered path (no store, erroring sender).

    /// A sender that always succeeds, so the delivered branch is reachable.
    #[derive(Debug)]
    struct OkSender(SessionId);

    #[async_trait::async_trait]
    impl MessageSender for OkSender {
        async fn send(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            Ok(crate::message_sender::DeliveryReceipt { session_id: self.0 })
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn delivered_result_carries_a_planning_note() {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        let recipient = alms_core::AgentRecord {
            id: AgentId::new(),
            name: "bob".into(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: alms_core::WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: alms_core::Timestamp::now().0,
            last_active: alms_core::Timestamp::now().0,
        };
        store.create_agent(&recipient).unwrap();
        let mgr = Arc::new(
            SessionManager::with_store(alms_session::SessionConfig::default(), store).unwrap(),
        );

        let dm_session = SessionId::new();
        let tool = SendMessageTool::new(
            Arc::new(OkSender(dm_session)),
            AgentId::new(),
            "alice".into(),
            mgr,
            SessionId::new(),
        );

        let result = tool
            .execute(serde_json::json!({ "to": "bob", "message": "hi" }))
            .await
            .expect("delivery must succeed");

        assert_eq!(result["delivered"], true);
        assert_eq!(result["dm_session_id"], dm_session.0.to_string());
        assert_eq!(
            result["note"], DELIVERED_NOTE,
            "a delivered send must still carry the planning note (#1111)"
        );
        assert!(!DELIVERED_NOTE.is_empty());
    }

    #[tokio::test]
    async fn test_no_dm_peer_means_no_fold() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await;
        assert!(
            result.is_err(),
            "without a DM peer, sends must proceed normally (missing store here)"
        );
    }
}
