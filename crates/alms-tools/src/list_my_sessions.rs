//! `list_my_sessions` tool -- lets an agent discover its own sessions.
//!
//! Returns sessions the agent has participated in (keyed by agent_id),
//! excluding internal sessions (subagent, episodic).  Each entry includes
//! session_id, context_type, context_id, message count, last activity, and
//! the episodic summary if one exists.

use alms_core::classify_session_type;
use alms_core::source_label::derive_source_label;
use alms_core::{AgentId, SessionId};
use alms_sandbox::{Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that lists all sessions belonging to the calling agent.
///
/// Agents use this to discover their conversation history across channels
/// (web, Telegram, DMs, jobs) and can follow up with `read_session` to
/// pull in details from a specific session.
#[derive(Debug)]
pub struct ListMySessionsTool {
    session_manager: Arc<SessionManager>,
    agent_id: AgentId,
    /// Current session ID -- excluded by default so the agent does not
    /// list the session it is currently running in. Note (#1214): a scheduled
    /// job runs ON its own `job_{id}` session, so an agent calling this from
    /// inside a job run won't see that job session unless it passes
    /// `include_current: true` (or calls from a different session).
    current_session_id: SessionId,
    /// Agent name, used to derive peer names in DM sessions.
    agent_name: String,
}

impl ListMySessionsTool {
    pub fn new(
        session_manager: Arc<SessionManager>,
        agent_id: AgentId,
        current_session_id: SessionId,
        agent_name: String,
    ) -> Self {
        Self {
            session_manager,
            agent_id,
            current_session_id,
            agent_name,
        }
    }
}

/// Returns `true` for internal session types that should be hidden from
/// the agent's session listing.
fn is_internal_session(context_id: &str) -> bool {
    matches!(
        classify_session_type(context_id),
        "subagent" | "episodic" | "notification"
    )
}

#[async_trait::async_trait]
impl Tool for ListMySessionsTool {
    fn name(&self) -> &str {
        "list_my_sessions"
    }

    fn description(&self) -> &str {
        "List your conversation sessions across all channels. Returns session ID, \
         context type (chat, telegram, dm, job), last activity time, message count, \
         and the episodic summary if one exists. Use this to recall what you have \
         worked on and with whom."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of sessions to return. Default: 10."
                },
                "include_current": {
                    "type": "boolean",
                    "description": "Whether to include the current session in the list. Default: false. Note: a scheduled job runs on its own session, so set this true to see the current job session when calling from inside a job run."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v.clamp(1, 100) as usize)
            .unwrap_or(10);

        let include_current = params
            .get("include_current")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Get sessions from the session manager (uses in-memory DashMap).
        // list_active_with_dms also includes shared DM sessions where this
        // agent is a participant (stored under AgentId::nil() sentinel).
        let mut sessions = self
            .session_manager
            .list_active_with_dms(self.agent_id, &self.agent_name);

        // Sort by last_activity descending (most recent first).
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity.0));

        // Filter out internal sessions and optionally the current session.
        // Collect all visible sessions first, then slice for limit -- avoids
        // iterating the filter chain twice (once for `take(limit)`, once for `total`).
        let all_visible: Vec<_> = sessions
            .iter()
            .filter(|s| !is_internal_session(&s.context_id))
            .filter(|s| include_current || s.id != self.current_session_id)
            .collect();
        let total = all_visible.len();
        let filtered = &all_visible[..total.min(limit)];

        // Build the result, enriching each session with message count and summary.
        //
        // NOTE: N+1 query pattern -- for each session we issue two SQLite queries
        // (message_count + load_session_summary). Acceptable for MVP since the
        // queries are simple indexed lookups and limit defaults to 10. If this
        // becomes a hot path, batch methods should be added to SqliteStore:
        //   - message_counts_for_sessions(ids) -> HashMap<SessionId, usize>
        //   - load_session_summaries_batch(agent_id, ids) -> HashMap<SessionId, SessionSummary>
        let mut session_list = Vec::with_capacity(filtered.len());
        for session in filtered {
            // Derive context type and source label.
            let ctx_type = classify_session_type(&session.context_id);
            let source_label = derive_source_label(&session.context_id, &self.agent_name)
                .map(|sl| sl.source_label)
                .unwrap_or_else(|| ctx_type.to_string());

            // Get message count -- try SQLite store first, fall back to in-memory history.
            let message_count = if let Some(store) = self.session_manager.store() {
                store.message_count(session.id).unwrap_or(0)
            } else {
                self.session_manager
                    .get_history(session.id)
                    .map(|h| h.len())
                    .unwrap_or(0)
            };

            // Look up episodic summary (if available).
            let summary = self
                .session_manager
                .load_session_summary(self.agent_id, session.id)
                .ok()
                .flatten()
                .map(|s| s.summary);

            session_list.push(serde_json::json!({
                "session_id": session.id.0.to_string(),
                "context_type": ctx_type,
                "context_id": session.context_id,
                "source_label": source_label,
                "message_count": message_count,
                "last_activity": session.last_activity.0.to_rfc3339(),
                "summary": summary,
            }));
        }

        Ok(serde_json::json!({
            "sessions": session_list,
            "showing": session_list.len(),
            "total": total,
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
    use alms_session::SessionConfig;

    fn make_manager() -> SessionManager {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        SessionManager::with_store(SessionConfig::default(), store).unwrap()
    }

    fn make_tool(
        mgr: Arc<SessionManager>,
        agent_id: AgentId,
        current_session_id: SessionId,
    ) -> ListMySessionsTool {
        ListMySessionsTool::new(mgr, agent_id, current_session_id, "test-agent".into())
    }

    // -- classify_session_type integration (canonical tests in alms-core) --

    #[test]
    fn test_context_type_chat_default() {
        // Verify the tool uses classify_session_type from alms-core
        // (returns "chat" not "web" for unrecognized context IDs).
        assert_eq!(classify_session_type("web-chat-2026-03-25"), "chat");
        assert_eq!(classify_session_type("some-random-context"), "chat");
    }

    #[test]
    fn test_context_type_telegram() {
        assert_eq!(classify_session_type("telegram_mybot_12345"), "telegram");
    }

    // -- is_internal_session --

    #[test]
    fn test_subagent_sessions_are_internal() {
        assert!(is_internal_session("subagent_task_1"));
        assert!(is_internal_session("subagent_"));
    }

    #[test]
    fn test_episodic_sessions_are_internal() {
        assert!(is_internal_session("episodic:main"));
    }

    #[test]
    fn test_notification_sessions_are_internal() {
        assert!(is_internal_session("notifications:bob"));
        assert!(is_internal_session("notifications:"));
    }

    #[test]
    fn test_user_sessions_are_not_internal() {
        assert!(!is_internal_session("web-chat"));
        assert!(!is_internal_session("telegram_bot_123"));
        assert!(!is_internal_session("dm:alice:bob"));
        assert!(!is_internal_session("job_abc"));
    }

    // -- tool execution --

    #[tokio::test]
    async fn test_excludes_subagent_sessions() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "web-chat");
        mgr.get_or_create(agent_id, "subagent_task_1");

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        // Should only have web-chat (current excluded by default, subagent excluded always)
        assert_eq!(result["showing"], 1);
        assert_eq!(result["sessions"][0]["context_id"], "web-chat");
    }

    #[tokio::test]
    async fn test_excludes_notification_sessions() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "web-chat");
        mgr.get_or_create(agent_id, "notifications:bob");

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        // Should only have web-chat (current excluded by default, notification excluded always)
        assert_eq!(result["showing"], 1);
        assert_eq!(result["sessions"][0]["context_id"], "web-chat");
    }

    #[tokio::test]
    async fn test_excludes_episodic_sessions() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "web-chat");
        mgr.get_or_create(agent_id, "episodic:main");

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(result["showing"], 1);
        assert_eq!(result["sessions"][0]["context_id"], "web-chat");
    }

    #[tokio::test]
    async fn test_current_session_excluded_by_default() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "other-ctx");

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(result["showing"], 1);
        assert_eq!(result["sessions"][0]["context_id"], "other-ctx");
    }

    #[tokio::test]
    async fn test_include_current_session() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "other-ctx");

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool
            .execute(serde_json::json!({"include_current": true}))
            .await
            .unwrap();

        assert_eq!(result["showing"], 2);
        assert_eq!(result["total"], 2);
    }

    #[tokio::test]
    async fn test_limit_parameter() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        for i in 0..5 {
            mgr.get_or_create(agent_id, format!("session-{i}"));
        }

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({"limit": 3})).await.unwrap();

        assert_eq!(result["showing"], 3);
        assert_eq!(result["total"], 5);
    }

    #[tokio::test]
    async fn test_no_store_returns_empty() {
        // SessionManager without SQLite -- list_active returns from in-memory DashMap.
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let agent_id = AgentId::new();
        let current_sid = SessionId::new();

        let tool = make_tool(mgr, agent_id, current_sid);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(result["showing"], 0);
        assert_eq!(result["total"], 0);
    }

    #[tokio::test]
    async fn test_includes_message_count() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        let chat = mgr.get_or_create(agent_id, "web-chat");

        // Add some messages to the chat session.
        for i in 0..3 {
            let msg = alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text(format!("msg {i}")),
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            };
            mgr.append_message(chat.id, msg).unwrap();
        }

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(result["sessions"][0]["message_count"], 3);
    }

    #[tokio::test]
    async fn test_includes_episodic_summary() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        let chat = mgr.get_or_create(agent_id, "web-chat");

        // Create an episodic summary for the chat session.
        mgr.upsert_session_summary(
            agent_id,
            chat.id,
            "Helped debug CORS headers.",
            None,
            Some("User chat"),
        )
        .unwrap();

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(
            result["sessions"][0]["summary"],
            "Helped debug CORS headers."
        );
    }

    #[tokio::test]
    async fn test_schema_structure() {
        let mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let tool = make_tool(mgr, AgentId::new(), SessionId::new());
        let schema = tool.parameters();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["limit"].is_object());
        assert!(schema["properties"]["include_current"].is_object());
    }

    #[tokio::test]
    async fn test_includes_dm_sessions() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");

        // Create a web session under the agent's own ID.
        mgr.get_or_create(agent_id, "web-chat");

        // Create a shared DM session (stored under AgentId::nil sentinel).
        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");

        // The tool should see both the web session AND the DM session
        // because the agent name "alice" appears in the DM context_id.
        let tool = ListMySessionsTool::new(
            mgr,
            agent_id,
            current.id,
            "alice".into(), // agent name matches one side of the DM
        );
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(
            result["showing"], 2,
            "Should show web-chat + DM session (current excluded)"
        );

        // Verify the DM session is in the result.
        let sessions = result["sessions"].as_array().unwrap();
        let has_dm = sessions
            .iter()
            .any(|s| s["context_type"].as_str() == Some("dm"));
        assert!(has_dm, "DM session should appear in the listing");
    }

    #[tokio::test]
    async fn test_includes_job_sessions() {
        // Regression test for #1214: an agent's own `job_{id}` sessions are
        // stored under the agent's real id (`fire_job_run` uses
        // `get_or_create(job.agent_id, "job_{id}")`) and are NOT internal
        // per `is_internal_session`, so they must appear in the listing.
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "web-chat");
        let job_ctx = format!("job_{}", uuid::Uuid::new_v4());
        mgr.get_or_create(agent_id, &job_ctx);

        let tool = make_tool(mgr, agent_id, current.id);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(
            result["showing"], 2,
            "Should show web-chat + the agent's own job session"
        );
        let sessions = result["sessions"].as_array().unwrap();
        let job = sessions
            .iter()
            .find(|s| s["context_type"].as_str() == Some("job"))
            .expect("the agent's own job session must appear in the listing");
        assert_eq!(job["context_id"], job_ctx.as_str());
    }

    #[tokio::test]
    async fn test_current_job_session_hidden_by_default_visible_with_include_current() {
        // #1214 follow-up (Tim): an agent that calls `list_my_sessions` from
        // INSIDE its own job run does NOT see that job session by default,
        // because the current session is excluded unless `include_current` is
        // set — and a job run's session IS the current session. This is the
        // intended current-session exclusion, not a bug, but it is the likely
        // real-world explanation for a "my job session is missing" report, so
        // pin both halves: hidden when current + default, visible with
        // include_current: true.
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();

        // The job session is the CURRENT session (the run executes on it).
        let job_ctx = format!("job_{}", uuid::Uuid::new_v4());
        let job_session = mgr.get_or_create(agent_id, &job_ctx);
        mgr.get_or_create(agent_id, "web-chat");

        let tool = make_tool(mgr.clone(), agent_id, job_session.id);

        // Default (include_current: false): the current job session is hidden.
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(
            result["showing"], 1,
            "the current job session is excluded by default"
        );
        let sessions = result["sessions"].as_array().unwrap();
        assert!(
            sessions
                .iter()
                .all(|s| s["context_id"].as_str() != Some(job_ctx.as_str())),
            "the current job session must NOT appear when include_current is false"
        );
        assert_eq!(sessions[0]["context_id"], "web-chat");

        // include_current: true surfaces the current job session.
        let result = tool
            .execute(serde_json::json!({"include_current": true}))
            .await
            .unwrap();
        assert_eq!(result["showing"], 2);
        let sessions = result["sessions"].as_array().unwrap();
        assert!(
            sessions
                .iter()
                .any(|s| s["context_id"].as_str() == Some(job_ctx.as_str())),
            "include_current: true must surface the current job session"
        );
    }

    #[tokio::test]
    async fn test_dm_sessions_not_shown_for_non_participant() {
        let mgr = Arc::new(make_manager());
        let agent_id = AgentId::new();
        let current = mgr.get_or_create(agent_id, "current-ctx");
        mgr.get_or_create(agent_id, "web-chat");

        // Create a DM between alice and bob.
        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");

        // Agent named "charlie" should NOT see the alice-bob DM.
        let tool = ListMySessionsTool::new(mgr, agent_id, current.id, "charlie".into());
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert_eq!(
            result["showing"], 1,
            "Charlie should only see web-chat, not alice-bob DM"
        );
    }
}
