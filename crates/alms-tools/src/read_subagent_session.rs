//! read_subagent_session tool — on-demand context retrieval from a named
//! subagent's conversation history.
//!
//! Instead of carrying full subagent transcripts in the parent's context
//! window, the parent calls this tool when it needs detail from a specific
//! subagent's session. The tool derives the subagent's deterministic session
//! ID (same UUID v5 logic as invoke_agent) and reads from SessionManager.

use alms_core::AgentId;
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that reads conversation history from a named subagent's session.
///
/// Named subagents (created via `invoke_agent(name=...)`) have persistent
/// sessions keyed on `(parent_agent_id, name)` (#1051). This tool lets the
/// parent agent selectively read their conversation history without carrying
/// it all in its own context window — and the same subagent name resolves
/// to the same session no matter which of the parent's chat sessions is
/// active.
#[derive(Debug)]
pub struct ReadSubagentSessionTool {
    session_manager: Arc<SessionManager>,
    /// Parent agent's persistent ID — drives the `(parent_agent_id, name)`
    /// keying that mirrors `invoke_agent` (#1051).
    parent_agent_id: AgentId,
}

impl ReadSubagentSessionTool {
    pub fn new(session_manager: Arc<SessionManager>, parent_agent_id: AgentId) -> Self {
        Self {
            session_manager,
            parent_agent_id,
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
                    "description": "If true, return the rolling context summary when available. \
                                    When no summary exists, falls back to returning recent messages \
                                    (capped at last_n or 10, whichever is smaller) with distinct \
                                    fallback_messages/fallback_message_count keys. Default: false."
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
        // (#1051): keyed on `(parent_agent_id, name)`, so the same named
        // subagent resolves to the same session across every chat the
        // parent agent participates in.
        let stable_id = AgentId::deterministic(self.parent_agent_id, name);
        let stable_ctx = format!("subagent_{}_{}", self.parent_agent_id.0, name);

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
            // Read message count — needed for both the has-summary and fallback paths.
            let messages = self
                .session_manager
                .get_history(session.id)
                .map_err(|e| SandboxError::Io(format!("Failed to read session history: {e}")))?;
            let total = messages.len();

            if let Some(ref text) = summary {
                return Ok(serde_json::json!({
                    "subagent": name,
                    "summary": text,
                    "has_summary": true,
                    "message_count": total,
                }));
            }

            // No summary available — fall back to the last N messages so the
            // caller still gets useful context instead of an empty response.
            const FALLBACK_COUNT: usize = 10;
            let effective_count = last_n.min(FALLBACK_COUNT);
            let start = total.saturating_sub(effective_count);
            let recent: Vec<Value> = messages[start..]
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "role": format!("{:?}", m.role).to_lowercase(),
                        "content": m.content.to_display_string()
                    })
                })
                .collect();

            return Ok(serde_json::json!({
                "subagent": name,
                "summary": Value::Null,
                "has_summary": false,
                "fallback_messages": recent,
                "fallback_message_count": total,
                "fallback_showing": recent.len(),
                "note": "No summary available. Showing the last messages as a fallback.",
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
                    "content": m.content.to_display_string()
                })
            })
            .collect();

        Ok(serde_json::json!({
            "subagent": name,
            "message_count": total,
            "showing": recent.len(),
            "messages": recent,
            "summary": summary.as_deref().map(Value::from).unwrap_or(Value::Null),
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::{Content, Message, Role, SessionConfig};

    fn make_tool() -> (ReadSubagentSessionTool, Arc<SessionManager>) {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let parent_agent_id = AgentId::new();
        let tool = ReadSubagentSessionTool::new(mgr.clone(), parent_agent_id);
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
    /// deterministic derivation that invoke_agent uses (#1051: keyed on
    /// `(parent_agent_id, name)`).
    fn populate_subagent(
        tool: &ReadSubagentSessionTool,
        mgr: &SessionManager,
        name: &str,
        messages: Vec<Message>,
    ) {
        let stable_id = AgentId::deterministic(tool.parent_agent_id, name);
        let stable_ctx = format!("subagent_{}_{}", tool.parent_agent_id.0, name);
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
    async fn test_summary_only_with_real_summary() {
        let (tool, mgr) = make_tool();
        let stable_id = AgentId::deterministic(tool.parent_agent_id, "summarized-sub");
        let stable_ctx = format!("subagent_{}_{}", tool.parent_agent_id.0, "summarized-sub");
        let session = mgr.get_or_create(stable_id, &stable_ctx);
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        // Set a context summary
        mgr.update_summary(
            session.id,
            alms_session::ContextSummary {
                text: "Researched topic X thoroughly".to_string(),
                messages_covered: 1,
                updated_at: Some(alms_core::Timestamp::now()),
            },
        )
        .unwrap();

        let result = tool
            .execute(serde_json::json!({ "name": "summarized-sub", "summary_only": true }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], true);
        assert_eq!(result["summary"], "Researched topic X thoroughly");
        // message_count is present even when has_summary is true
        assert_eq!(result["message_count"], 1);
        // No fallback messages when a real summary exists
        assert!(result.get("fallback_messages").is_none());
    }

    #[tokio::test]
    async fn test_summary_only_falls_back_to_messages() {
        let (tool, mgr) = make_tool();
        populate_subagent(
            &tool,
            &mgr,
            "researcher",
            vec![
                make_msg(Role::User, "research topic X"),
                make_msg(Role::Assistant, "Here are my findings on topic X"),
            ],
        );

        // No summary set — should fall back to returning messages
        let result = tool
            .execute(serde_json::json!({ "name": "researcher", "summary_only": true }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert!(result["summary"].is_null());
        // Fallback messages should be included
        let fallback = result["fallback_messages"].as_array().unwrap();
        assert_eq!(fallback.len(), 2);
        assert_eq!(fallback[0]["content"], "research topic X");
        assert_eq!(fallback[1]["content"], "Here are my findings on topic X");
        assert_eq!(result["fallback_message_count"], 2);
        assert_eq!(result["fallback_showing"], 2);
        assert!(result["note"].as_str().unwrap().contains("fallback"));
    }

    #[tokio::test]
    async fn test_summary_only_zero_messages_fallback() {
        let (tool, mgr) = make_tool();
        // Create a session with zero messages (session exists but nothing appended)
        let stable_id = AgentId::deterministic(tool.parent_agent_id, "empty-sub");
        let stable_ctx = format!("subagent_{}_{}", tool.parent_agent_id.0, "empty-sub");
        let _session = mgr.get_or_create(stable_id, &stable_ctx);

        // No summary, no messages — should return empty fallback without panicking
        let result = tool
            .execute(serde_json::json!({ "name": "empty-sub", "summary_only": true }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert!(result["summary"].is_null());
        let fallback = result["fallback_messages"].as_array().unwrap();
        assert!(fallback.is_empty());
        assert_eq!(result["fallback_message_count"], 0);
        assert_eq!(result["fallback_showing"], 0);
    }

    #[tokio::test]
    async fn test_summary_only_fallback_respects_last_n() {
        let (tool, mgr) = make_tool();
        let msgs: Vec<Message> = (0..8)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();
        populate_subagent(&tool, &mgr, "capped", msgs);

        // last_n=3 should cap fallback to 3, not the default 10
        let result = tool
            .execute(serde_json::json!({ "name": "capped", "summary_only": true, "last_n": 3 }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert_eq!(result["fallback_message_count"], 8);
        assert_eq!(result["fallback_showing"], 3);
        let fallback = result["fallback_messages"].as_array().unwrap();
        assert_eq!(fallback.len(), 3);
        // Should be the last 3 messages (msg 5, msg 6, msg 7)
        assert_eq!(fallback[0]["content"], "msg 5");
        assert_eq!(fallback[2]["content"], "msg 7");
    }

    #[tokio::test]
    async fn test_summary_included_with_messages() {
        let (tool, mgr) = make_tool();
        let stable_id = AgentId::deterministic(tool.parent_agent_id, "summarized");
        let stable_ctx = format!("subagent_{}_{}", tool.parent_agent_id.0, "summarized");
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

    /// Regression for #1051 — named subagent sessions are keyed on
    /// `(parent_agent_id, name)`, so two tool instances built for the same
    /// parent agent (registered separately into two different runtime
    /// instances backing two different chat sessions) must resolve
    /// `read_subagent_session("reviewer")` to the SAME subagent session.
    ///
    /// Since #1068 dropped the unused `parent_session_id` field, the two
    /// tool instances are constructed identically here — the test still
    /// pins the cross-chat-session contract because `parent_agent_id` is
    /// now the sole driver.
    #[tokio::test]
    async fn test_cross_session_same_parent_agent_resolves_to_same_session() {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let parent_agent_id = AgentId::new();

        // Both tools share the same parent_agent_id — they stand in for two
        // separate runtimes (e.g., one per chat session) backed by the same
        // parent agent. Both must land on the same `(parent_agent_id, name)`
        // subagent session.
        let tool_a = ReadSubagentSessionTool::new(mgr.clone(), parent_agent_id);
        let tool_b = ReadSubagentSessionTool::new(mgr.clone(), parent_agent_id);

        // Populate via tool A — message arrives in the shared session.
        populate_subagent(
            &tool_a,
            &mgr,
            "reviewer",
            vec![make_msg(Role::User, "from chat A")],
        );

        // Tool B (separate instance, same parent agent) must see it.
        let result_b = tool_b
            .execute(serde_json::json!({ "name": "reviewer" }))
            .await
            .unwrap();
        assert_eq!(result_b["message_count"], 1);
        let msgs_b = result_b["messages"].as_array().unwrap();
        assert_eq!(msgs_b[0]["content"], "from chat A");

        // Sanity: a tool bound to a DIFFERENT parent agent must NOT see it.
        let other_parent = AgentId::new();
        assert_ne!(other_parent, parent_agent_id);
        let tool_c = ReadSubagentSessionTool::new(mgr.clone(), other_parent);
        let result_c = tool_c
            .execute(serde_json::json!({ "name": "reviewer" }))
            .await
            .unwrap();
        assert!(
            result_c["error"]
                .as_str()
                .unwrap_or("")
                .contains("No session found"),
            "different parent agent must not resolve to the same subagent \
             session (got: {result_c})"
        );
    }
}
