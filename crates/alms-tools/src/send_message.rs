//! send_message tool -- sends a peer message from one agent to another.
//!
//! Fire-and-forget: the tool returns immediately with a delivery confirmation.
//! The recipient processes the message asynchronously via a triggered run.

use crate::message_sender::{MessageSender, SendError};
use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

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
        "Send a message to another agent. The target agent will receive it \
         and may respond. Use this for peer-to-peer communication -- asking \
         for reviews, sharing updates, requesting help. The message is \
         delivered asynchronously (fire-and-forget). Use read_messages to \
         check the conversation later. Do NOT use this to reply to an agent \
         you are already in a direct conversation with -- your final reply \
         text is delivered to them automatically."
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
                "note": "Message delivered. The recipient will process it asynchronously."
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
