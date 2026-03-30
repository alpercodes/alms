//! `read_session` tool -- on-demand session recall for an agent's own sessions.
//!
//! Allows an agent to read conversation history from any of its own sessions
//! by session UUID. This is the general-purpose complement to `read_messages`
//! (DM-only) and `read_subagent_session` (subagent-only).
//!
//! Security: the tool verifies that the requested session belongs to the
//! calling agent (`session.agent_id == self.agent_id`). For shared DM sessions
//! (where `agent_id` is the nil UUID sentinel), access is granted if the
//! agent's name appears in the session's `context_id`.

use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;

/// Built-in tool that reads conversation history from one of the agent's own sessions.
#[derive(Debug)]
pub struct ReadSessionTool {
    session_manager: Arc<SessionManager>,
    /// The calling agent's ID -- used for ownership verification.
    agent_id: AgentId,
    /// The calling agent's name -- used for DM shared session access checks.
    /// `None` for unnamed agents (they cannot access DM shared sessions).
    agent_name: Option<String>,
}

impl ReadSessionTool {
    pub fn new(
        session_manager: Arc<SessionManager>,
        agent_id: AgentId,
        agent_name: Option<String>,
    ) -> Self {
        Self {
            session_manager,
            agent_id,
            agent_name,
        }
    }

    /// Check whether this agent is allowed to read the given session.
    ///
    /// Returns `Ok(())` if access is allowed, or an error message string.
    fn check_access(&self, session: &alms_session::Session) -> Result<(), String> {
        // Case 1: session belongs to this agent directly.
        if session.agent_id == self.agent_id {
            return Ok(());
        }

        // Case 2: shared DM session (agent_id is nil UUID sentinel).
        // Access is granted if the agent's name exactly matches one of the
        // colon-delimited segments in the context_id (pattern: "dm:{name1}:{name2}").
        // We compare exact segments to prevent substring false-positives
        // (e.g. agent "al" must NOT match "dm:alice:bob").
        let nil_sentinel = AgentId(uuid::Uuid::nil());
        if session.agent_id == nil_sentinel {
            if let Some(ref name) = self.agent_name {
                let segments: Vec<&str> = session.context_id.split(':').collect();
                if segments.len() >= 3
                    && segments[0] == "dm"
                    && (segments[1] == name || segments[2] == name)
                {
                    return Ok(());
                }
            }
            return Err(format!(
                "Session {} is a shared session that does not belong to you.",
                session.id.0,
            ));
        }

        // Case 3: session belongs to a different agent.
        Err(format!(
            "Session {} does not belong to you. You can only read your own sessions.",
            session.id.0,
        ))
    }
}

#[async_trait::async_trait]
impl Tool for ReadSessionTool {
    fn name(&self) -> &str {
        "read_session"
    }

    fn description(&self) -> &str {
        "Read the conversation history of one of your sessions. Use list_my_sessions \
         first to find the session ID, then read_session to get the detail. Returns \
         the last N messages and the session summary."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The UUID of the session to read."
                },
                "last_n": {
                    "type": "integer",
                    "description": "Number of most recent messages to return. Default: 20."
                },
                "summary_only": {
                    "type": "boolean",
                    "description": "If true, return only the episodic summary (if exists), not individual messages. Default: false."
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // Parse session_id
        let session_id_str = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters(
                    "'session_id' is required and must be a non-empty string".into(),
                )
            })?;

        let uuid = uuid::Uuid::parse_str(session_id_str).map_err(|_| {
            SandboxError::InvalidParameters(format!(
                "'{session_id_str}' is not a valid UUID. Use list_my_sessions to find valid session IDs."
            ))
        })?;
        let session_id = SessionId(uuid);

        let last_n = params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let summary_only = params
            .get("summary_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Look up the session
        let session = match self.session_manager.get(session_id) {
            Ok(s) => s,
            Err(_) => {
                return Ok(serde_json::json!({
                    "error": format!("No session found with ID '{session_id_str}'."),
                    "session_id": session_id_str,
                }));
            }
        };

        // Security: verify ownership
        if let Err(msg) = self.check_access(&session) {
            debug!(
                agent_id = %self.agent_id.0,
                session_id = %session_id_str,
                "read_session access denied"
            );
            return Ok(serde_json::json!({
                "error": msg,
                "session_id": session_id_str,
            }));
        }

        // Load episodic summary (if available)
        let episodic_summary = self
            .session_manager
            .load_session_summary(self.agent_id, session_id)
            .ok()
            .flatten()
            .map(|s| s.summary);

        // Load context summary (rolling compression summary)
        let context_summary = self
            .session_manager
            .get_summary(session_id)
            .ok()
            .filter(|s| !s.text.is_empty())
            .map(|s| s.text);

        // Prefer episodic summary, fall back to context summary
        let summary = episodic_summary.or(context_summary);

        if summary_only {
            return Ok(serde_json::json!({
                "session_id": session_id_str,
                "context_id": session.context_id,
                "summary": summary.as_deref().unwrap_or(""),
                "has_summary": summary.is_some(),
            }));
        }

        // Read message history
        let messages = self
            .session_manager
            .get_history(session_id)
            .map_err(|e| SandboxError::Io(format!("Failed to read session history: {e}")))?;

        let total = messages.len();
        let start = total.saturating_sub(last_n);

        let is_dm = session.context_id.starts_with("dm:");

        let recent: Vec<Value> = messages[start..]
            .iter()
            .map(|m| {
                let mut entry = serde_json::json!({
                    "role": format!("{:?}", m.role).to_lowercase(),
                    "content": m.content.to_display_string(),
                });
                // For DM sessions, include sender attribution from metadata
                // so the output is readable (all DM messages have role "user").
                if is_dm {
                    let from_agent = m
                        .metadata
                        .as_ref()
                        .and_then(|meta| meta.get("from_agent"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    entry["from"] = Value::String(from_agent.to_string());
                }
                entry
            })
            .collect();

        let mut result = serde_json::json!({
            "session_id": session_id_str,
            "context_id": session.context_id,
            "message_count": total,
            "showing": recent.len(),
            "messages": recent,
            "summary": summary.as_deref().unwrap_or(""),
        });

        // Add a hint for DM sessions directing agents to read_messages
        // for proper perspective mapping ("you" vs peer name).
        if is_dm {
            result["note"] = Value::String(
                "This is a DM session. For perspective-aware sender labels, \
                 use the read_messages tool instead."
                    .to_string(),
            );
        }

        Ok(result)
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::{Content, Message, Role, SessionConfig};

    fn make_session_manager() -> Arc<SessionManager> {
        Arc::new(SessionManager::new(SessionConfig::default()))
    }

    fn make_tool(mgr: Arc<SessionManager>) -> ReadSessionTool {
        let agent_id = AgentId::new();
        ReadSessionTool::new(mgr, agent_id, Some("alice".to_string()))
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

    #[tokio::test]
    async fn test_missing_session_id_is_error() {
        let mgr = make_session_manager();
        let tool = make_tool(mgr);
        let err = tool.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_empty_session_id_is_error() {
        let mgr = make_session_manager();
        let tool = make_tool(mgr);
        let err = tool
            .execute(serde_json::json!({ "session_id": "" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_invalid_uuid_format_is_error() {
        let mgr = make_session_manager();
        let tool = make_tool(mgr);
        let err = tool
            .execute(serde_json::json!({ "session_id": "not-a-uuid" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
        if let SandboxError::InvalidParameters(msg) = err {
            assert!(msg.contains("not a valid UUID"));
        }
    }

    #[tokio::test]
    async fn test_nonexistent_session_returns_error_message() {
        let mgr = make_session_manager();
        let tool = make_tool(mgr);
        let fake_id = uuid::Uuid::new_v4().to_string();
        let result = tool
            .execute(serde_json::json!({ "session_id": fake_id }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No session found")
        );
    }

    #[tokio::test]
    async fn test_reads_own_session_successfully() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        // Create a session owned by this agent
        let session = mgr.get_or_create(agent_id, "web-chat-test");
        mgr.append_message(session.id, make_msg(Role::User, "Hello"))
            .unwrap();
        mgr.append_message(session.id, make_msg(Role::Assistant, "Hi there!"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        assert_eq!(result["session_id"], session.id.0.to_string());
        assert_eq!(result["context_id"], "web-chat-test");
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["showing"], 2);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hello");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "Hi there!");
    }

    #[tokio::test]
    async fn test_rejects_other_agents_session() {
        let mgr = make_session_manager();
        let my_id = AgentId::new();
        let other_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), my_id, Some("alice".to_string()));

        // Create a session owned by a different agent
        let session = mgr.get_or_create(other_id, "other-ctx");
        mgr.append_message(session.id, make_msg(Role::User, "secret"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("does not belong to you")
        );
    }

    #[tokio::test]
    async fn test_last_n_limits_messages() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let session = mgr.get_or_create(agent_id, "ctx-many");
        for i in 0..10 {
            mgr.append_message(session.id, make_msg(Role::User, &format!("msg {i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({
                "session_id": session.id.0.to_string(),
                "last_n": 3
            }))
            .await
            .unwrap();

        assert_eq!(result["message_count"], 10);
        assert_eq!(result["showing"], 3);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "msg 7");
        assert_eq!(msgs[2]["content"], "msg 9");
    }

    #[tokio::test]
    async fn test_summary_only_mode() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let session = mgr.get_or_create(agent_id, "ctx-summary");
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        // No summary set -- should return empty
        let result = tool
            .execute(serde_json::json!({
                "session_id": session.id.0.to_string(),
                "summary_only": true
            }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert_eq!(result["summary"], "");
        assert_eq!(result["context_id"], "ctx-summary");
        // Messages should NOT be included in summary_only mode
        assert!(result.get("messages").is_none());
    }

    #[tokio::test]
    async fn test_summary_only_with_context_summary() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let session = mgr.get_or_create(agent_id, "ctx-with-summary");
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        // Set a context summary (rolling compression summary)
        mgr.update_summary(
            session.id,
            alms_session::ContextSummary {
                text: "Discussed debugging techniques".to_string(),
                messages_covered: 1,
                updated_at: Some(alms_core::Timestamp::now()),
            },
        )
        .unwrap();

        let result = tool
            .execute(serde_json::json!({
                "session_id": session.id.0.to_string(),
                "summary_only": true
            }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], true);
        assert_eq!(result["summary"], "Discussed debugging techniques");
    }

    #[tokio::test]
    async fn test_dm_shared_session_accessible_by_participant() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        // Create a shared DM session (nil UUID sentinel as agent_id)
        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");
        mgr.append_message(session.id, make_msg(Role::User, "Hey Bob!"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Access should be granted because "alice" appears in "dm:alice:bob"
        assert!(result.get("error").is_none());
        assert_eq!(result["message_count"], 1);
        assert_eq!(result["context_id"], "dm:alice:bob");
    }

    #[tokio::test]
    async fn test_dm_shared_session_denied_for_non_participant() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("charlie".to_string()));

        // Create a shared DM session between alice and bob
        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");
        mgr.append_message(session.id, make_msg(Role::User, "Private"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Access should be denied -- "charlie" is not in "dm:alice:bob"
        assert!(result["error"].as_str().unwrap().contains("shared session"));
    }

    #[tokio::test]
    async fn test_unnamed_agent_cannot_access_dm_session() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        // No agent name -- cannot verify DM participation
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");
        mgr.append_message(session.id, make_msg(Role::User, "Private"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        assert!(result["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_dm_substring_prefix_does_not_grant_access() {
        // Regression: agent named "al" should NOT access dm:alice:bob
        // because "al" is a substring of "alice" but not an exact segment match.
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("al".to_string()));

        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");
        mgr.append_message(session.id, make_msg(Role::User, "Secret"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Access must be denied -- "al" is not an exact segment in "dm:alice:bob"
        assert!(
            result["error"].as_str().is_some(),
            "Agent 'al' should not access dm:alice:bob via substring match"
        );
        assert!(result["error"].as_str().unwrap().contains("shared session"));
    }

    #[tokio::test]
    async fn test_dm_session_includes_sender_attribution() {
        // S2: DM sessions should include "from" field with sender name
        // and a "note" suggesting read_messages for perspective mapping.
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");

        // Add DM messages with from_agent metadata
        let msg1 = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: alms_session::Content::Text("Hey Bob!".into()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({"from_agent": "alice"})),
        };
        let msg2 = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: alms_session::Content::Text("Hi Alice!".into()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({"from_agent": "bob"})),
        };
        mgr.append_message(session.id, msg1).unwrap();
        mgr.append_message(session.id, msg2).unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Verify sender attribution is included
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["from"], "alice");
        assert_eq!(msgs[1]["from"], "bob");

        // Verify the DM note is present
        assert!(
            result["note"].as_str().is_some(),
            "DM sessions should include a note about read_messages"
        );
        assert!(result["note"].as_str().unwrap().contains("read_messages"));
    }

    #[tokio::test]
    async fn test_non_dm_session_has_no_from_or_note() {
        // Non-DM sessions should NOT include "from" field or "note"
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        let session = mgr.get_or_create(agent_id, "web-chat");
        mgr.append_message(session.id, make_msg(Role::User, "Hello"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        let msgs = result["messages"].as_array().unwrap();
        // Non-DM messages should not have "from" field
        assert!(msgs[0].get("from").is_none());
        // Non-DM sessions should not have a note
        assert!(result.get("note").is_none());
    }

    #[tokio::test]
    async fn test_schema_has_required_session_id() {
        let mgr = make_session_manager();
        let tool = make_tool(mgr);
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "session_id"));
    }

    #[tokio::test]
    async fn test_summary_included_with_messages() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let session = mgr.get_or_create(agent_id, "ctx-summary-msg");
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        // Set a context summary
        mgr.update_summary(
            session.id,
            alms_session::ContextSummary {
                text: "Explored architecture decisions".to_string(),
                messages_covered: 1,
                updated_at: Some(alms_core::Timestamp::now()),
            },
        )
        .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Summary should be included alongside messages
        assert_eq!(result["summary"], "Explored architecture decisions");
        assert_eq!(result["message_count"], 1);
        assert!(result["messages"].as_array().is_some());
    }
}
