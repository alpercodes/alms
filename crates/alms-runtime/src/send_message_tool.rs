//! send_message tool -- sends a peer message from one agent to another.
//!
//! Fire-and-forget: the tool returns immediately with a delivery confirmation.
//! The recipient processes the message asynchronously via a triggered run.

use crate::message_sender::{MessageSender, SendError};
use alms_core::AgentId;
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
}

impl SendMessageTool {
    pub fn new(
        sender: Arc<dyn MessageSender>,
        sender_agent_id: AgentId,
        sender_name: String,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self {
            sender,
            sender_agent_id,
            sender_name,
            session_manager,
        }
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
         check the conversation later."
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

        match self
            .sender
            .send(
                &self.sender_name,
                self.sender_agent_id,
                to,
                recipient.id,
                message,
            )
            .await
        {
            Ok(receipt) => Ok(serde_json::json!({
                "delivered": true,
                "dm_session_id": receipt.session_id.0.to_string(),
                "note": "Message delivered. The recipient will process it asynchronously."
            })),
            Err(SendError::CooldownActive) => {
                warn!("send_message cooldown active for {to}");
                Ok(serde_json::json!({
                    "error": "Cooldown active -- wait a few seconds before sending another message to this agent.",
                    "cooldown": true,
                }))
            }
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
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            Err(SendError::Internal("noop".into()))
        }
    }

    fn make_tool() -> SendMessageTool {
        let mgr = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let sender: Arc<dyn MessageSender> = Arc::new(NoopSender);
        SendMessageTool::new(sender, AgentId::new(), "test-sender".into(), mgr)
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
}
