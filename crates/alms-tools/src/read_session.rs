//! `read_session` tool -- on-demand session recall for an agent's own sessions.
//!
//! Allows an agent to read conversation history from any of its own sessions
//! by session UUID. This is the general-purpose complement to `read_messages`
//! (DM-only) and `read_subagent_session` (subagent-only).
//!
//! Security: the tool verifies that the requested session belongs to the
//! calling agent (`session.agent_id == self.agent_id`). For shared DM sessions
//! (where `agent_id` is the nil UUID sentinel), access is granted if the
//! agent's name appears in the session's `context_id`. Subagent sessions are
//! the one class where `session.agent_id` is not the owner and are decided by
//! [`alms_core::subagent_session_access`] instead — see [`ReadSessionTool::check_access`].

use crate::dm_filter;
use crate::session_read;
use alms_core::{AgentId, SessionId, SubagentSessionAccess};
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
    ///
    /// Case 0 runs first and is not a special case of the others: for a
    /// subagent session `session.agent_id` is the **invoked** agent since
    /// #1288, so Case 1 would answer the ownership question with the id of
    /// whoever the work was delegated *to*. The rule that supersedes it is
    /// stated once in [`alms_core::subagent_session_access`] — this tool and
    /// `read_subagent_session` both call it so they cannot drift into
    /// opposite answers about the same bytes again (#1298).
    fn check_access(&self, session: &alms_session::Session) -> Result<(), String> {
        // Case 0: subagent session — owned by the parent named in the
        // context_id, whoever the row is filed under.
        match alms_core::subagent_session_access(&session.context_id, self.agent_id) {
            SubagentSessionAccess::Owner { .. } => return Ok(()),
            SubagentSessionAccess::Denied(denial) => return Err(denial.message(session.id)),
            // Every other session class falls through to the cases below.
            SubagentSessionAccess::NotSubagent => {}
        }

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
         first to find the session ID, then read_session to get the detail. By default \
         returns the whole transcript and the session summary. The response carries \
         `total_count` (messages in the session), `returned_count` (what's in the \
         `messages` array), and `truncated: bool` -- check these to detect whether older \
         messages were omitted. Pass `last_n` explicitly if you only need a specific count."
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
                    "minimum": 0,
                    "description": "Optional: number of most-recent messages to return. \
                                    Must be a non-negative integer. Omit to return all messages \
                                    (subject to response-size limits indicated by the `truncated` \
                                    flag). Malformed values (negative, non-integer, non-numeric) \
                                    are rejected with InvalidParameters rather than silently \
                                    falling back."
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

        // #1032: no silent default. Omitted means "everything, bounded by the
        // caps"; malformed is an error rather than a quiet fallback to 20.
        let explicit_last_n = session_read::parse_last_n(&params)?;

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
            // No contract fields here, deliberately: this branch returns no
            // `messages` array at all, so `returned_count` / `truncated` would
            // describe something that is not in the response. That differs
            // from `read_subagent_session`'s `summary_only`, which DOES carry
            // them — because when no summary exists it falls back to
            // returning messages, and those can be truncated (#1032).
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

        let is_dm = session.context_id.starts_with("dm:");

        // For DM sessions, filter out synthetic markers (dm_ended, empty
        // bodies, tool calls/results, synthetic notifications) so the
        // output only contains real conversational messages. Apply last_n
        // AFTER filtering so the count reflects actual messages.
        let format_msg = |m: &alms_session::Message| {
            let mut entry = serde_json::json!({
                "role": format!("{:?}", m.role).to_lowercase(),
                "content": m.content.to_display_string(),
            });
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
        };

        // The cap walk runs on the POST-filter list, so `total_count` counts
        // real conversational messages and matches `read_messages` semantics
        // -- a DM whose history is mostly markers must not report a total the
        // agent can never page to.
        let selection = if is_dm {
            let real: Vec<&alms_session::Message> = messages
                .iter()
                .filter(|m| !dm_filter::is_synthetic_marker(m))
                .collect();
            session_read::select_recent(
                &real,
                explicit_last_n,
                session_read::SERIALIZED_BYTE_CAP,
                session_read::MESSAGE_CAP,
                |m: &&alms_session::Message| format_msg(m),
            )
        } else {
            session_read::select_recent(
                &messages,
                explicit_last_n,
                session_read::SERIALIZED_BYTE_CAP,
                session_read::MESSAGE_CAP,
                format_msg,
            )
        };

        let mut result = serde_json::json!({
            "session_id": session_id_str,
            "context_id": session.context_id,
            // Legacy keys kept for back-compat; the contract is the quartet
            // stamped just below.
            "message_count": selection.total_count,
            "showing": selection.returned_count(),
            "messages": selection.entries,
            "summary": summary.as_deref().unwrap_or(""),
        });
        selection.write_contract_fields(&mut result);

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

    fn is_auto_approved(&self) -> bool {
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

    // -- #1032: the truncation contract ---------------------------------
    //
    // `truncation_reason` has exactly four values, so the rows below cover
    // exactly four: `null`, `explicit_last_n`, `byte_cap`, `message_cap`.
    // Derived from `session_read::reason` plus the untruncated case, not from
    // the situations that happened to come to mind.

    /// Build a session owned by `agent_id` holding `count` messages of
    /// `body`, and return the tool plus the session id.
    fn session_with(count: usize, body: &str) -> (ReadSessionTool, String) {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);
        let session = mgr.get_or_create(agent_id, "ctx-contract");
        for i in 0..count {
            mgr.append_message(session.id, make_msg(Role::User, &format!("{body}{i}")))
                .unwrap();
        }
        (tool, session.id.0.to_string())
    }

    async fn read_with(tool: &ReadSessionTool, session_id: &str, params: Value) -> Value {
        let mut p = serde_json::json!({ "session_id": session_id });
        if let Some(obj) = params.as_object() {
            for (k, v) in obj {
                p[k] = v.clone();
            }
        }
        tool.execute(p).await.unwrap()
    }

    /// `null` — the whole transcript fits, and the default path returns ALL
    /// of it rather than the pre-#1032 silent 20.
    #[tokio::test]
    async fn contract_untruncated_default_returns_everything() {
        let (tool, sid) = session_with(35, "m");
        let result = read_with(&tool, &sid, serde_json::json!({})).await;

        assert_eq!(result["total_count"], 35);
        assert_eq!(
            result["returned_count"], 35,
            "the silent last_n=20 default is gone"
        );
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
        // Legacy keys still agree with the new ones.
        assert_eq!(result["message_count"], 35);
        assert_eq!(result["showing"], 35);
    }

    /// `explicit_last_n` — honoured verbatim, and flagged because older
    /// messages exist.
    #[tokio::test]
    async fn contract_explicit_last_n_is_flagged_when_older_exist() {
        let (tool, sid) = session_with(10, "m");
        let result = read_with(&tool, &sid, serde_json::json!({ "last_n": 3 })).await;

        assert_eq!(result["total_count"], 10);
        assert_eq!(result["returned_count"], 3);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "explicit_last_n");
    }

    /// The complement that keeps the row above honest: an explicit `last_n`
    /// covering the whole session omits nothing, so it must NOT be flagged.
    #[tokio::test]
    async fn contract_explicit_last_n_covering_all_is_not_flagged() {
        let (tool, sid) = session_with(3, "m");
        let result = read_with(&tool, &sid, serde_json::json!({ "last_n": 3 })).await;

        assert_eq!(result["returned_count"], 3);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
    }

    /// `byte_cap` — a few very large messages trip the serialized-byte cap.
    #[tokio::test]
    async fn contract_byte_cap_truncates_to_the_trailing_slice() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);
        let session = mgr.get_or_create(agent_id, "ctx-big");
        // 10 x ~10 KB comfortably exceeds the 60 KB cap.
        for i in 0..10 {
            let body = format!("{i}{}", "x".repeat(10_000));
            mgr.append_message(session.id, make_msg(Role::User, &body))
                .unwrap();
        }

        let result = read_with(&tool, &session.id.0.to_string(), serde_json::json!({})).await;

        assert_eq!(result["total_count"], 10);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "byte_cap");
        let returned = result["returned_count"].as_u64().unwrap();
        assert!(
            returned > 0 && returned < 10,
            "a trailing slice, not all or nothing: {returned}"
        );
        // The NEWEST messages are the ones kept.
        let msgs = result["messages"].as_array().unwrap();
        assert!(
            msgs.last().unwrap()["content"]
                .as_str()
                .unwrap()
                .starts_with('9'),
            "the newest message must survive"
        );
    }

    /// `message_cap` — a chatty session where each message is a few bytes,
    /// so the byte cap can never fire but the count backstop must.
    #[tokio::test]
    async fn contract_message_cap_backstops_a_chatty_session() {
        let (tool, sid) = session_with(session_read::MESSAGE_CAP + 25, "m");
        let result = read_with(&tool, &sid, serde_json::json!({})).await;

        assert_eq!(result["total_count"], session_read::MESSAGE_CAP + 25);
        assert_eq!(result["returned_count"], session_read::MESSAGE_CAP);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "message_cap");
    }

    /// The #1028 P1 lesson on this sibling: the cap is charged on the
    /// SERIALIZED entry, so escape-heavy content costs its post-escape size.
    /// Measuring raw UTF-8 would admit roughly twice as much.
    #[tokio::test]
    async fn contract_byte_cap_accounts_for_json_escape_expansion() {
        async fn returned_for(body: char) -> u64 {
            let mgr = make_session_manager();
            let agent_id = AgentId::new();
            let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);
            let session = mgr.get_or_create(agent_id, "ctx-escape");
            for _ in 0..40 {
                let text: String = std::iter::repeat_n(body, 4_000).collect();
                mgr.append_message(session.id, make_msg(Role::User, &text))
                    .unwrap();
            }
            let result = tool
                .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
                .await
                .unwrap();
            assert_eq!(result["truncation_reason"], "byte_cap");
            result["returned_count"].as_u64().unwrap()
        }

        // `"` serializes to `\"`: one raw byte, two wire bytes.
        let escaped = returned_for('"').await;
        let plain = returned_for('x').await;
        assert!(
            escaped < plain,
            "escape-heavy content must be charged post-escape: \
             escaped={escaped} plain={plain}"
        );
    }

    /// The silent-fallback class #1028 closed for `read_messages`, now closed
    /// here: a malformed `last_n` is an error, not a quiet 20.
    #[tokio::test]
    async fn contract_malformed_last_n_is_rejected_not_defaulted() {
        let (tool, sid) = session_with(30, "m");
        for bad in [
            serde_json::json!(-1),
            serde_json::json!(3.5),
            serde_json::json!("20"),
            serde_json::json!(true),
        ] {
            let err = tool
                .execute(serde_json::json!({ "session_id": sid, "last_n": bad }))
                .await
                .expect_err(&format!("{bad} must be rejected"));
            assert!(matches!(err, SandboxError::InvalidParameters(_)), "{bad}");
        }
    }

    /// The DM branch filters synthetic markers, and the cap walk runs on the
    /// POST-filter list — so `total_count` counts messages an agent can
    /// actually page to, matching `read_messages` semantics.
    #[tokio::test]
    async fn contract_total_count_excludes_filtered_dm_markers() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));
        let session = mgr.get_or_create(agent_id, "dm:alice:bob");

        let mut real = make_msg(Role::User, "hello");
        real.metadata = Some(serde_json::json!({ "from_agent": "bob" }));
        mgr.append_message(session.id, real).unwrap();

        // A synthetic marker of the shape `dm_filter` hides.
        let mut marker = make_msg(Role::System, "");
        marker.metadata = Some(serde_json::json!({ "synthetic": true, "type": "dm_ended" }));
        mgr.append_message(session.id, marker).unwrap();

        let result = read_with(&tool, &session.id.0.to_string(), serde_json::json!({})).await;
        assert_eq!(
            result["total_count"], 1,
            "the marker must not inflate a total the agent can never reach"
        );
        assert_eq!(result["returned_count"], 1);
        assert!(result["truncation_reason"].is_null());
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
        mgr.append_message(session.id, make_dm_msg("alice", "Hey Bob!"))
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
    async fn test_dm_session_includes_sender_attribution() {
        // DM sessions should include "from" field with sender name
        // and a "note" suggesting read_messages for perspective mapping.
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");

        // Add DM messages with production-format metadata
        mgr.append_message(session.id, make_dm_msg("alice", "Hey Bob!"))
            .unwrap();
        mgr.append_message(session.id, make_dm_msg("bob", "Hi Alice!"))
            .unwrap();

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

    /// Regression test for #558 / S1: DM marker messages (dm_ended, synthetic,
    /// tool results) must be filtered out of read_session output for DM sessions,
    /// while real DM messages with `"message_type": "dm"` are preserved.
    #[tokio::test]
    async fn test_dm_session_filters_synthetic_markers() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));

        let dm_session_id = SessionId::deterministic_dm("alice", "bob");
        let session = mgr.get_or_create_shared(dm_session_id, "dm:alice:bob");

        // Real DM messages (production format with "message_type": "dm")
        mgr.append_message(session.id, make_dm_msg("alice", "Hey Bob!"))
            .unwrap();
        mgr.append_message(session.id, make_dm_msg("bob", "Hi Alice!"))
            .unwrap();

        // dm_ended marker -- should be filtered
        let dm_ended = Message {
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
        mgr.append_message(session.id, dm_ended).unwrap();

        // Synthetic notification -- should be filtered
        let synthetic = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("synthetic notification".into()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({"synthetic": true})),
        };
        mgr.append_message(session.id, synthetic).unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Only the two real DM messages should be visible
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["showing"], 2);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["from"], "alice");
        assert_eq!(msgs[0]["content"], "Hey Bob!");
        assert_eq!(msgs[1]["from"], "bob");
        assert_eq!(msgs[1]["content"], "Hi Alice!");
    }

    /// Non-DM sessions must NOT apply DM marker filtering -- all messages
    /// should be returned regardless of metadata.
    #[tokio::test]
    async fn test_non_dm_session_does_not_filter() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);

        let session = mgr.get_or_create(agent_id, "web-chat-no-filter");
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();
        // A message with synthetic=true in a non-DM session should still appear
        let synthetic_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("system note".into()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({"synthetic": true})),
        };
        mgr.append_message(session.id, synthetic_msg).unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        // Both messages should be present -- no filtering for non-DM sessions
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["showing"], 2);
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

    // -- #1298: subagent transcripts are owned by the spawning parent -------

    /// Reproduce the post-#1288 row exactly: `agent_id` is the *invoked*
    /// agent's registry id, `context_id` still names the parent.
    fn named_subagent_session(
        mgr: &Arc<SessionManager>,
        parent: AgentId,
        invoked: AgentId,
        name: &str,
    ) -> alms_session::Session {
        let session =
            mgr.get_or_create(invoked, alms_core::named_subagent_context_id(parent, name));
        assert_eq!(
            session.agent_id, invoked,
            "test setup: the row must be filed under the invoked agent"
        );
        mgr.append_message(session.id, make_msg(Role::Assistant, "reviewed it"))
            .unwrap();
        session
    }

    async fn read(tool: &ReadSessionTool, session: &alms_session::Session) -> Value {
        tool.execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap()
    }

    /// The bug #1298 was filed for, at the tool's own entry point. The
    /// invoked agent owns the *row* since #1288, so `session.agent_id ==
    /// self.agent_id` admits it — and that is the wrong question. Delete the
    /// subagent branch from `check_access` and this row goes green-to-red.
    #[tokio::test]
    async fn read_session_refuses_the_invoked_agent_its_own_subagent_transcript() {
        let mgr = make_session_manager();
        let parent = AgentId::new();
        let invoked = AgentId::new();
        let session = named_subagent_session(&mgr, parent, invoked, "reviewer");

        let tool = ReadSessionTool::new(mgr.clone(), invoked, Some("reviewer".to_string()));
        let result = read(&tool, &session).await;

        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("belongs to another agent's subagent"),
            "the agent the work was delegated TO must not read it back: {result}"
        );
        assert!(result.get("messages").is_none());
    }

    /// The other half of the same rule: the invoking parent is the owner and
    /// gets the transcript, even though the row is filed elsewhere. Without
    /// the subagent branch this falls to "does not belong to you".
    #[tokio::test]
    async fn read_session_admits_the_invoking_parent_of_a_subagent_transcript() {
        let mgr = make_session_manager();
        let parent = AgentId::new();
        let invoked = AgentId::new();
        let session = named_subagent_session(&mgr, parent, invoked, "reviewer");

        let tool = ReadSessionTool::new(mgr.clone(), parent, Some("alice".to_string()));
        let result = read(&tool, &session).await;

        assert!(result.get("error").is_none(), "parent denied: {result}");
        assert_eq!(result["messages"][0]["content"], "reviewed it");
    }

    /// A third agent that merely learned the UUID is refused. The id is not a
    /// bearer capability (#1185) and this tool must not make it one.
    #[tokio::test]
    async fn read_session_refuses_a_bystander_holding_the_subagent_session_id() {
        let mgr = make_session_manager();
        let parent = AgentId::new();
        let invoked = AgentId::new();
        let session = named_subagent_session(&mgr, parent, invoked, "reviewer");

        let tool = ReadSessionTool::new(mgr.clone(), AgentId::new(), Some("mallory".to_string()));
        let result = read(&tool, &session).await;

        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("belongs to another agent's subagent"),
            "{result}"
        );
    }

    /// Two parents, one named subagent: post-#1288 both rows carry the same
    /// `agent_id`, so only the context separates them. Alice must not read
    /// Bob's delegation to the shared reviewer.
    #[tokio::test]
    async fn read_session_separates_two_parents_of_the_same_named_subagent() {
        let mgr = make_session_manager();
        let alice = AgentId::new();
        let bob = AgentId::new();
        let reviewer = AgentId::new();
        let alices = named_subagent_session(&mgr, alice, reviewer, "reviewer");
        let bobs = named_subagent_session(&mgr, bob, reviewer, "reviewer");
        assert_eq!(
            alices.agent_id, bobs.agent_id,
            "test setup: the two rows must be indistinguishable by agent_id"
        );

        let tool = ReadSessionTool::new(mgr.clone(), alice, Some("alice".to_string()));
        assert_eq!(
            read(&tool, &alices).await["messages"][0]["content"],
            "reviewed it"
        );
        assert!(
            read(&tool, &bobs).await["error"]
                .as_str()
                .unwrap_or("")
                .contains("belongs to another agent's subagent"),
        );
    }

    /// The #1185 legacy shape records no parent, so it is denied to everyone
    /// — including the agent whose id the row is filed under, which is the
    /// only reader `check_access` would otherwise have admitted.
    #[tokio::test]
    async fn read_session_refuses_a_legacy_subagent_context_even_to_the_filed_owner() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, format!("subagent_{}", uuid::Uuid::new_v4()));
        mgr.append_message(session.id, make_msg(Role::Assistant, "legacy"))
            .unwrap();

        let tool = ReadSessionTool::new(mgr.clone(), agent_id, Some("alice".to_string()));
        let result = read(&tool, &session).await;

        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("legacy ephemeral subagent session"),
            "{result}"
        );
    }

    /// An ephemeral subagent is the same rule with a task id in place of a
    /// name: parent in, everyone else out.
    #[tokio::test]
    async fn read_session_applies_the_rule_to_ephemeral_subagent_sessions() {
        let mgr = make_session_manager();
        let parent = AgentId::new();
        let context_id = format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4());
        let session = mgr.get_or_create(AgentId::new(), &context_id);
        mgr.append_message(session.id, make_msg(Role::Assistant, "did the thing"))
            .unwrap();

        let parent_tool = ReadSessionTool::new(mgr.clone(), parent, None);
        assert_eq!(
            read(&parent_tool, &session).await["messages"][0]["content"],
            "did the thing"
        );

        let other_tool = ReadSessionTool::new(mgr.clone(), AgentId::new(), None);
        assert!(read(&other_tool, &session).await["error"].is_string());
    }

    /// The subagent branch must not swallow the session classes it has no
    /// opinion about: an ordinary chat the agent owns still reads back.
    #[tokio::test]
    async fn read_session_still_serves_a_context_that_merely_looks_subagent_ish() {
        let mgr = make_session_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "subagentish-notes");
        mgr.append_message(session.id, make_msg(Role::User, "hello"))
            .unwrap();

        let tool = ReadSessionTool::new(mgr.clone(), agent_id, None);
        let result = read(&tool, &session).await;

        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(result["messages"][0]["content"], "hello");
    }
}
