//! read_messages tool -- reads DM conversation history with another agent.
//!
//! Uses the shared DM session model: both agents read from the same session.
//! The session is looked up by its deterministic SessionId (derived from the
//! sorted name pair) rather than by `(agent_id, context_id)`.

use crate::dm_filter;
use alms_core::SessionId;
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::{Message, SessionManager};
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that reads the DM conversation history with another agent.
///
/// Uses the deterministic SessionId from the sorted name pair so it finds
/// the correct shared session regardless of who initiated the conversation.
#[derive(Debug)]
pub struct ReadMessagesTool {
    session_manager: Arc<SessionManager>,
    /// This agent's name -- used for DM session lookup and display.
    agent_name: String,
}

impl ReadMessagesTool {
    pub fn new(session_manager: Arc<SessionManager>, agent_name: String) -> Self {
        Self {
            session_manager,
            agent_name,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ReadMessagesTool {
    fn name(&self) -> &str {
        "read_messages"
    }

    fn description(&self) -> &str {
        "Read the conversation history with another agent. Returns recent \
         messages from your DM session with the specified agent. Use this \
         to check for replies after sending a message via send_message."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Name of the agent whose DM thread to read"
                },
                "last_n": {
                    "type": "integer",
                    "description": "Number of recent messages to return (default: 20)"
                }
            },
            "required": ["from"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let from = params
            .get("from")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'from' is required and must be non-empty".into())
            })?;

        let last_n = params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        // Look up the shared DM session by its deterministic SessionId.
        let session_id = SessionId::deterministic_dm(&self.agent_name, from);

        if !self.session_manager.has_session_by_id(session_id) {
            return Ok(serde_json::json!({
                "error": format!("No DM session found with agent '{from}'. You may not have exchanged messages yet."),
                "peer": from,
            }));
        }

        // Read message history from the shared session.
        let messages = self
            .session_manager
            .get_history(session_id)
            .map_err(|e| SandboxError::Io(format!("Failed to read DM history: {e}")))?;

        // Filter out synthetic markers (dm_ended, empty bodies, tool
        // calls/results, synthetic notifications) so agents only see
        // real conversational messages. Apply last_n AFTER filtering
        // so the requested count reflects actual messages, not raw
        // session entries.
        let real_messages: Vec<&Message> = messages
            .iter()
            .filter(|m| !dm_filter::is_synthetic_marker(m))
            .collect();

        let visible_count = real_messages.len();
        let start = visible_count.saturating_sub(last_n);

        let recent: Vec<Value> = real_messages[start..]
            .iter()
            .map(|m: &&Message| {
                let from_agent = m
                    .metadata
                    .as_ref()
                    .and_then(|meta| meta.get("from_agent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // Show perspective-aware role: messages from self = "you",
                // messages from others = their name.
                let display_sender = if from_agent == self.agent_name {
                    "you".to_string()
                } else {
                    from_agent.to_string()
                };

                serde_json::json!({
                    "from": display_sender,
                    "content": m.content.to_display_string(),
                })
            })
            .collect();

        // Get summary if available
        let summary = self
            .session_manager
            .get_summary(session_id)
            .ok()
            .filter(|s| !s.text.is_empty())
            .map(|s| s.text);

        Ok(serde_json::json!({
            "peer": from,
            "message_count": visible_count,
            "showing": recent.len(),
            "messages": recent,
            "summary": summary.as_deref().unwrap_or(""),
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::dm_context_id;
    use alms_session::{Content, Message, Role, SessionConfig};

    fn make_tool() -> (ReadMessagesTool, Arc<SessionManager>) {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let tool = ReadMessagesTool::new(mgr.clone(), "alice".into());
        (tool, mgr)
    }

    /// Helper to create a DM message with production-format metadata.
    /// `MessageBus::send()` stamps every real DM with `"message_type": "dm"`,
    /// `"from_agent"`, and `"from_agent_id"`.
    fn make_dm_msg(from_name: &str, text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": from_name,
                "from_agent_id": uuid::Uuid::new_v4().to_string(),
                "message_type": "dm",
            })),
        }
    }

    #[tokio::test]
    async fn test_missing_from_is_error() {
        let (tool, _) = make_tool();
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_empty_from_is_error() {
        let (tool, _) = make_tool();
        let err = tool
            .execute(serde_json::json!({ "from": "" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_no_session_returns_info_message() {
        let (tool, _) = make_tool();
        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();
        assert!(result["error"].as_str().unwrap().contains("No DM session"));
    }

    #[tokio::test]
    async fn test_reads_dm_messages_from_shared_session() {
        let (tool, mgr) = make_tool();

        // Create a shared DM session and populate it with production-format messages
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        mgr.append_message(session_id, make_dm_msg("alice", "Hello Bob!"))
            .unwrap();
        mgr.append_message(session_id, make_dm_msg("bob", "Hi Alice!"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["peer"], "bob");
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["showing"], 2);
        let msgs = result["messages"].as_array().unwrap();
        // Alice's own message should show as "you"
        assert_eq!(msgs[0]["from"], "you");
        assert_eq!(msgs[0]["content"], "Hello Bob!");
        // Bob's message should show as "bob"
        assert_eq!(msgs[1]["from"], "bob");
        assert_eq!(msgs[1]["content"], "Hi Alice!");
    }

    #[tokio::test]
    async fn test_last_n_limits() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        for i in 0..10 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("msg {i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": 3 }))
            .await
            .unwrap();

        assert_eq!(result["message_count"], 10);
        assert_eq!(result["showing"], 3);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "msg 7");
        assert_eq!(msgs[2]["content"], "msg 9");
    }

    #[test]
    fn test_schema_requires_from() {
        let (tool, _) = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "from"));
    }

    /// Regression test for #558: dm_ended markers and other synthetic
    /// messages must not appear in read_messages output, but real DM
    /// messages (with `"message_type": "dm"`) must be preserved.
    #[tokio::test]
    async fn test_filters_dm_ended_markers_and_synthetic_messages() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // Real message from bob -- production format with "message_type": "dm"
        let real_msg = make_dm_msg("bob", "Hello Alice!");

        // dm_ended marker (empty content, no from_agent)
        let dm_ended_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(String::new()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "message_type": "dm_ended",
                "ended_by": "alice",
                "reason": "ignored",
            })),
        };

        // Synthetic notification marker
        let synthetic_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("synthetic notification".into()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({"synthetic": true})),
        };

        // Tool result (should also be filtered)
        let tool_result = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "t1".into(),
                result: serde_json::json!({"ok": true}),
            },
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };

        mgr.append_message(session_id, real_msg).unwrap();
        mgr.append_message(session_id, dm_ended_marker).unwrap();
        mgr.append_message(session_id, synthetic_marker).unwrap();
        mgr.append_message(session_id, tool_result).unwrap();

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        // Only the real message should be visible
        assert_eq!(result["message_count"], 1);
        assert_eq!(result["showing"], 1);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["from"], "bob");
        assert_eq!(msgs[0]["content"], "Hello Alice!");
    }

    /// Verify that real DM messages with production metadata are NOT
    /// incorrectly filtered. This is the critical regression test for C1:
    /// the original filter matched ANY `message_type` value, which would
    /// discard all real DM messages since MessageBus stamps them with
    /// `"message_type": "dm"`.
    #[tokio::test]
    async fn test_real_dm_messages_with_message_type_dm_are_preserved() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // Three real DM messages with production metadata
        mgr.append_message(session_id, make_dm_msg("alice", "Hey Bob"))
            .unwrap();
        mgr.append_message(session_id, make_dm_msg("bob", "Hey Alice"))
            .unwrap();
        mgr.append_message(session_id, make_dm_msg("alice", "How are you?"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        // All three real messages must be visible
        assert_eq!(result["message_count"], 3);
        assert_eq!(result["showing"], 3);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["from"], "you");
        assert_eq!(msgs[0]["content"], "Hey Bob");
        assert_eq!(msgs[1]["from"], "bob");
        assert_eq!(msgs[1]["content"], "Hey Alice");
        assert_eq!(msgs[2]["from"], "you");
        assert_eq!(msgs[2]["content"], "How are you?");
    }
}
