//! read_subagent_session tool — on-demand context retrieval from a named
//! subagent's conversation history.
//!
//! Instead of carrying full subagent transcripts in the parent's context
//! window, the parent calls this tool when it needs detail from a specific
//! subagent's session. The tool derives the subagent's deterministic session
//! ID (same UUID v5 logic as invoke_agent) and reads from SessionManager.

use crate::context::content_to_string;
use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that reads conversation history from a named subagent's session.
///
/// Named subagents (created via `invoke_agent(name=...)`) have persistent
/// sessions. This tool lets the parent agent selectively read their
/// conversation history without carrying it all in its own context window.
#[derive(Debug)]
pub struct ReadSubagentSessionTool {
    session_manager: Arc<SessionManager>,
    parent_session_id: SessionId,
}

impl ReadSubagentSessionTool {
    pub fn new(session_manager: Arc<SessionManager>, parent_session_id: SessionId) -> Self {
        Self {
            session_manager,
            parent_session_id,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ReadSubagentSessionTool {
    fn name(&self) -> &str {
        "read_subagent_session"
    }

    fn description(&self) -> &str {
        "Read the conversation history of a named subagent. Named subagents \
         (created via invoke_agent with a name) have persistent sessions — this \
         tool lets you read their full conversation when you need the detail. \
         Returns the last N messages (default 20) and the session summary if available."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The subagent's persistent name (e.g., 'reviewer', 'researcher')."
                },
                "last_n": {
                    "type": "integer",
                    "description": "Number of most recent messages to return. Default: 20."
                },
                "summary_only": {
                    "type": "boolean",
                    "description": "If true, return only the rolling context summary (if one exists), \
                                    not individual messages. Default: false."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'name' is required and must be non-empty".into())
            })?;

        let last_n = params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let summary_only = params
            .get("summary_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Derive the same deterministic identity that invoke_agent uses
        let parent_as_agent = AgentId(self.parent_session_id.0);
        let stable_id = AgentId::deterministic(parent_as_agent, name);
        let stable_ctx = format!("subagent_{}_{}", self.parent_session_id.0, name);

        // Check if the session exists without creating it
        let key = (stable_id, stable_ctx);
        if !self.session_manager.has_session(&key) {
            return Ok(serde_json::json!({
                "error": format!("No session found for subagent '{name}'. It may not have been invoked yet."),
                "subagent": name
            }));
        }

        let session = self.session_manager.get_or_create(stable_id, &key.1);

        // Get summary if available
        let summary = self
            .session_manager
            .get_summary(session.id)
            .ok()
            .filter(|s| !s.text.is_empty())
            .map(|s| s.text);

        if summary_only {
            return Ok(serde_json::json!({
                "subagent": name,
                "summary": summary.as_deref().unwrap_or(""),
                "has_summary": summary.is_some(),
            }));
        }

        // Read message history
        let messages = self
            .session_manager
            .get_history(session.id)
            .map_err(|e| SandboxError::Io(format!("Failed to read session history: {e}")))?;

        let total = messages.len();

        // Take last_n messages
        let start = total.saturating_sub(last_n);
        let recent: Vec<Value> = messages[start..]
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": format!("{:?}", m.role).to_lowercase(),
                    "content": content_to_string(&m.content)
                })
            })
            .collect();

        Ok(serde_json::json!({
            "subagent": name,
            "message_count": total,
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
    use alms_session::{Content, Message, Role, SessionConfig};

    fn make_tool() -> (ReadSubagentSessionTool, Arc<SessionManager>) {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let parent_session_id = SessionId::new();
        let tool = ReadSubagentSessionTool::new(mgr.clone(), parent_session_id);
        (tool, mgr)
    }

    fn make_msg(role: Role, text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: Content::Text(text.to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        }
    }

    /// Populate a named subagent's session with messages, using the same
    /// deterministic derivation that invoke_agent uses.
    fn populate_subagent(
        tool: &ReadSubagentSessionTool,
        mgr: &SessionManager,
        name: &str,
        messages: Vec<Message>,
    ) {
        let parent_as_agent = AgentId(tool.parent_session_id.0);
        let stable_id = AgentId::deterministic(parent_as_agent, name);
        let stable_ctx = format!("subagent_{}_{}", tool.parent_session_id.0, name);
        let session = mgr.get_or_create(stable_id, &stable_ctx);
        for msg in messages {
            mgr.append_message(session.id, msg).unwrap();
        }
    }

    #[tokio::test]
    async fn test_missing_name_is_error() {
        let (tool, _) = make_tool();
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_empty_name_is_error() {
        let (tool, _) = make_tool();
        let err = tool
            .execute(serde_json::json!({ "name": "" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_nonexistent_subagent_returns_error_message() {
        let (tool, _) = make_tool();
        let result = tool
            .execute(serde_json::json!({ "name": "ghost" }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No session found")
        );
        assert_eq!(result["subagent"], "ghost");
    }

    #[tokio::test]
    async fn test_reads_subagent_messages() {
        let (tool, mgr) = make_tool();
        populate_subagent(
            &tool,
            &mgr,
            "reviewer",
            vec![
                make_msg(Role::User, "Review this code"),
                make_msg(Role::Assistant, "Found 3 issues"),
            ],
        );

        let result = tool
            .execute(serde_json::json!({ "name": "reviewer" }))
            .await
            .unwrap();

        assert_eq!(result["subagent"], "reviewer");
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["showing"], 2);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Review this code");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "Found 3 issues");
    }

    #[tokio::test]
    async fn test_last_n_limits_messages() {
        let (tool, mgr) = make_tool();
        let msgs: Vec<Message> = (0..10)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();
        populate_subagent(&tool, &mgr, "chatty", msgs);

        let result = tool
            .execute(serde_json::json!({ "name": "chatty", "last_n": 3 }))
            .await
            .unwrap();

        assert_eq!(result["message_count"], 10);
        assert_eq!(result["showing"], 3);
        let msgs = result["messages"].as_array().unwrap();
        // Should be the last 3 messages (msg 7, msg 8, msg 9)
        assert_eq!(msgs[0]["content"], "msg 7");
        assert_eq!(msgs[2]["content"], "msg 9");
    }

    #[tokio::test]
    async fn test_summary_only_mode() {
        let (tool, mgr) = make_tool();
        populate_subagent(
            &tool,
            &mgr,
            "researcher",
            vec![make_msg(Role::User, "research topic X")],
        );

        // No summary set — should return empty
        let result = tool
            .execute(serde_json::json!({ "name": "researcher", "summary_only": true }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert_eq!(result["summary"], "");
        // Messages should NOT be included in summary_only mode
        assert!(result.get("messages").is_none());
    }

    #[tokio::test]
    async fn test_summary_included_with_messages() {
        let (tool, mgr) = make_tool();
        let parent_as_agent = AgentId(tool.parent_session_id.0);
        let stable_id = AgentId::deterministic(parent_as_agent, "summarized");
        let stable_ctx = format!("subagent_{}_{}", tool.parent_session_id.0, "summarized");
        let session = mgr.get_or_create(stable_id, &stable_ctx);
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        // Set a summary
        mgr.update_summary(
            session.id,
            alms_session::ContextSummary {
                text: "Discussed architecture decisions".to_string(),
                messages_covered: 1,
                updated_at: Some(alms_core::Timestamp::now()),
            },
        )
        .unwrap();

        let result = tool
            .execute(serde_json::json!({ "name": "summarized" }))
            .await
            .unwrap();

        assert_eq!(result["summary"], "Discussed architecture decisions");
        assert_eq!(result["message_count"], 1);
    }

    #[tokio::test]
    async fn test_schema_has_required_name() {
        let (tool, _) = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "name"));
    }
}
