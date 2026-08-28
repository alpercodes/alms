//! read_subagent_session tool — on-demand context retrieval from a
//! subagent's conversation history.
//!
//! Instead of carrying full subagent transcripts in the parent's context
//! window, the parent calls this tool when it needs detail from a specific
//! subagent's session. Named subagents resolve by `name` through the same
//! `(parent_agent_id, name)` keying invoke_agent dispatches on (#1051),
//! via the shared `SessionManager::named_subagent_key` (#1278). Ephemeral
//! / unnamed subagents have no name to derive from, so
//! they resolve by `session_id` instead (#1181) — the id every invoke_agent
//! result, `subagent_started` event, and completion notification already
//! surfaces to the parent.

use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;

/// Built-in tool that reads conversation history from a subagent's session.
///
/// Named subagents (created via `invoke_agent(name=...)`) have persistent
/// sessions keyed on `(parent_agent_id, name)` (#1051) and resolve by `name`
/// — the same subagent name resolves to the same session no matter which of
/// the parent's chat sessions is active.
///
/// Ephemeral / unnamed subagents (created via `invoke_agent` without a name,
/// typically `background: true`) get a fresh random session per invocation,
/// so there is nothing to derive a session from — pre-#1181 this tool simply
/// could not read them even though their transcript is fully persisted. They
/// resolve by `session_id`, guarded by [`Self::check_subagent_session_access`]
/// so the by-id path cannot be used to read arbitrary non-subagent sessions —
/// and, because the session UUID leaks beyond the spawning parent (it appears
/// in parent-visible result/completion text and on shared DM sessions), the
/// check enforces PARENT OWNERSHIP via the parent id embedded in the session
/// context, never treating the UUID itself as a bearer capability.
#[derive(Debug)]
pub struct ReadSubagentSessionTool {
    session_manager: Arc<SessionManager>,
    /// Parent agent's persistent ID — drives the `(parent_agent_id, name)`
    /// keying that mirrors `invoke_agent` (#1051), and the ownership check
    /// on the by-`session_id` path (#1181).
    ///
    /// Note both uses read the *parent*, and #1278 changed only the agent
    /// id a named subagent session is FILED under (to the invoked agent's
    /// registry id), never the `context_id` the ownership check parses. So
    /// the by-id authorization below is unaffected by that move — see
    /// [`Self::check_subagent_session_access`].
    parent_agent_id: AgentId,
}

impl ReadSubagentSessionTool {
    pub fn new(session_manager: Arc<SessionManager>, parent_agent_id: AgentId) -> Self {
        Self {
            session_manager,
            parent_agent_id,
        }
    }

    /// Authorize a by-`session_id` readback target (#1181, hardened per the
    /// Tim / Codex access-control review on PR #1185).
    ///
    /// The by-id path must not turn this tool into "read any session by
    /// UUID" — `read_session` already exists for the agent's OWN sessions
    /// and enforces its own ownership model. Only subagent sessions are in
    /// scope here, and BOTH `context_id` shapes the coordinator's
    /// `derive_subagent_identity` produces embed the spawning parent's id,
    /// which must match this tool's `parent_agent_id`:
    ///
    /// - `subagent_{parent_agent_id}_{name}` — named subagent (#1051).
    /// - `subagent_{parent_agent_id}_{task_id}` — ephemeral / unnamed
    ///   subagent. The parent id was added to this shape by the PR #1185
    ///   hardening: the session UUID must NOT act as a bearer capability,
    ///   because it leaks beyond the spawning parent — it appears in
    ///   parent-visible `invoke_agent` result / completion text and, for
    ///   DM-triggered invocations, is persisted onto the SHARED DM parent
    ///   session where the DM peer can see it. This tool is registered for
    ///   every agent and is auto-approved, so without the ownership check
    ///   any agent that learned the UUID could read the full transcript.
    ///
    /// Legacy ephemeral sessions created before the hardening
    /// (`subagent_{task_id}`, no parent linkage) are DENIED: their ownership
    /// cannot be verified, and denying them is strictly no worse than
    /// pre-#1181 behaviour (ephemeral readback never worked at all).
    ///
    /// **The check reads the `context_id` and nothing else.** That is why
    /// #1278 — which moved a named subagent session's `agent_id` onto the
    /// invoked agent's registry id — could not weaken it: the parent is
    /// embedded in the context, the context was left byte-for-byte
    /// unchanged, and `session.agent_id` was never an input here. A future
    /// change that starts deriving ownership from `session.agent_id` would
    /// be authorizing on the INVOKED agent instead of the invoking one.
    ///
    /// Returns the subagent's name for an owned named session, `None` for an
    /// owned ephemeral one, or a caller-facing error message.
    fn check_subagent_session_access(
        &self,
        session: &alms_session::Session,
    ) -> Result<Option<String>, String> {
        let Some(rest) = session.context_id.strip_prefix("subagent_") else {
            return Err(format!(
                "Session {} is not a subagent session. Use read_session to read your own sessions.",
                session.id.0
            ));
        };

        // Legacy pre-hardening ephemeral shape: `subagent_{task_id}` with no
        // parent linkage — ownership cannot be verified, so deny.
        if uuid::Uuid::parse_str(rest).is_ok() {
            return Err(format!(
                "Session {} is a legacy ephemeral subagent session without parent \
                 ownership metadata and cannot be read back.",
                session.id.0
            ));
        }

        // Both current shapes are `{parent_uuid}_{remainder}` — a UUID never
        // contains '_', so the first '_' is the separator regardless of the
        // remainder's own characters.
        if let Some((parent, remainder)) = rest.split_once('_')
            && let Ok(parent_uuid) = uuid::Uuid::parse_str(parent)
        {
            if AgentId(parent_uuid) != self.parent_agent_id {
                return Err(format!(
                    "Session {} belongs to another agent's subagent. You can only read \
                     sessions of subagents you invoked.",
                    session.id.0
                ));
            }
            // Ephemeral remainder is the task UUID → no name label. (A named
            // subagent whose registered name happens to parse as a UUID would
            // get a null label too — cosmetic only; the ownership check above
            // is identical either way.)
            return if uuid::Uuid::parse_str(remainder).is_ok() {
                Ok(None)
            } else {
                Ok(Some(remainder.to_string()))
            };
        }

        Err(format!(
            "Session {} has an unrecognized subagent session format.",
            session.id.0
        ))
    }
}

#[async_trait::async_trait]
impl Tool for ReadSubagentSessionTool {
    fn name(&self) -> &str {
        "read_subagent_session"
    }

    fn description(&self) -> &str {
        "Read the conversation history of a subagent. For a NAMED subagent \
         (created via invoke_agent with a name), pass 'name'. For an ephemeral / \
         unnamed subagent (e.g. a background invoke_agent without a name), pass \
         'session_id' — the subagent session UUID from the invoke_agent result or \
         the completion notification. Returns the last N messages (default 20) \
         and the session summary if available."
    }

    fn parameters(&self) -> Value {
        // `required` is empty at schema level: exactly one of `name` /
        // `session_id` must be provided, which JSON Schema's `required`
        // cannot express — the execute path validates the one-of contract
        // and returns a clear error otherwise (mirrors the shell tool's
        // command/check_task mutual exclusivity).
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The subagent's persistent name (e.g., 'reviewer', 'researcher'). \
                                    Use for named subagents. Provide either this or session_id."
                },
                "session_id": {
                    "type": "string",
                    "description": "The subagent's session UUID. Use for ephemeral / unnamed \
                                    subagents (returned by invoke_agent as session_id, and \
                                    included in the background-completion notification). \
                                    Provide either this or name."
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
            "required": []
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let session_id_param = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let last_n = params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let summary_only = params
            .get("summary_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Resolve the target session: by `name` for named subagents (the
        // pre-#1181 path, unchanged), or by `session_id` for ephemeral /
        // unnamed ones. `subagent_label` is the name when known (given, or
        // recovered from a named session's context_id) and null for
        // ephemeral subagents, which have no name.
        let (session, subagent_label) = match (name, session_id_param) {
            (Some(_), Some(_)) => {
                return Err(SandboxError::InvalidParameters(
                    "provide either 'name' or 'session_id', not both".into(),
                ));
            }
            (None, None) => {
                return Err(SandboxError::InvalidParameters(
                    "either 'name' (named subagent) or 'session_id' (ephemeral / unnamed \
                     subagent) is required"
                        .into(),
                ));
            }
            (Some(name), None) => {
                // Resolve through the SAME helper `invoke_agent`'s dispatch
                // writes with (`SessionManager::named_subagent_key`), never a
                // local re-derivation: the two halves of the key have
                // different rules (the context is a pure format, the agent id
                // depends on the registry — #1278) and a second spelling of
                // them here is a silent "no session found" waiting to happen.
                let key = self
                    .session_manager
                    .named_subagent_key(self.parent_agent_id, name);

                // Check if the session exists without creating it
                if !self.session_manager.has_session(&key) {
                    return Ok(serde_json::json!({
                        "error": format!("No session found for subagent '{name}'. It may not have been invoked yet."),
                        "subagent": name
                    }));
                }

                let session = self.session_manager.get_or_create(key.0, &key.1);
                (session, Value::from(name))
            }
            (None, Some(sid_str)) => {
                // #1181: ephemeral / unnamed subagents have a random session
                // per invocation — nothing to derive. Resolve the persisted
                // session directly by the id the parent already holds.
                let session_id = uuid::Uuid::parse_str(sid_str).map(SessionId).map_err(|_| {
                    SandboxError::InvalidParameters(format!(
                        "'session_id' must be a valid UUID, got '{sid_str}'"
                    ))
                })?;

                let Ok(session) = self.session_manager.get(session_id) else {
                    return Ok(serde_json::json!({
                        "error": format!("No session found with id '{sid_str}'."),
                        "session_id": sid_str,
                    }));
                };

                match self.check_subagent_session_access(&session) {
                    Ok(named) => {
                        let label = named.map(Value::from).unwrap_or(Value::Null);
                        (session, label)
                    }
                    Err(msg) => {
                        return Ok(serde_json::json!({
                            "error": msg,
                            "session_id": sid_str,
                        }));
                    }
                }
            }
        };

        // Both resolution paths converge here. Every response carries the
        // resolved `session_id` (additive, #1181) so the parent has a stable
        // handle for follow-up reads — e.g. paging through a long transcript
        // with `last_n` after a truncated background summary.
        let session_id_str = session.id.0.to_string();

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
                    "subagent": subagent_label,
                    "session_id": session_id_str,
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
                "subagent": subagent_label,
                "session_id": session_id_str,
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
            "subagent": subagent_label,
            "session_id": session_id_str,
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

    /// Populate a named subagent's session with messages, on the same key
    /// `invoke_agent`'s dispatch writes to (#1051: keyed on
    /// `(parent_agent_id, name)`; #1278: the agent half is the invoked
    /// agent's registry id when it has one).
    fn populate_subagent(
        tool: &ReadSubagentSessionTool,
        mgr: &SessionManager,
        name: &str,
        messages: Vec<Message>,
    ) {
        let (agent_id, context_id) = mgr.named_subagent_key(tool.parent_agent_id, name);
        let session = mgr.get_or_create(agent_id, &context_id);
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

    /// #1181: `name` is no longer schema-required — exactly one of `name` /
    /// `session_id` must be provided, which JSON Schema `required` cannot
    /// express. The one-of contract is enforced in `execute` (pinned by the
    /// `test_*_is_error` tests above/below); the schema must expose both
    /// properties and require neither.
    #[tokio::test]
    async fn test_schema_one_of_name_or_session_id() {
        let (tool, _) = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "required must be empty — name and session_id are mutually exclusive alternatives"
        );
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("name"));
        assert!(props.contains_key("session_id"));
    }

    // ── #1181: by-session_id readback for ephemeral / unnamed subagents ─────

    /// Create an ephemeral subagent session exactly the way the coordinator's
    /// `derive_subagent_identity` does for unnamed subagents: fresh random
    /// `AgentId`, context `subagent_{parent_agent_id}_{task_id}` (the parent
    /// id embedded per the PR #1185 ownership hardening). Returns the
    /// session id.
    fn populate_ephemeral_subagent(
        mgr: &SessionManager,
        parent_agent_id: AgentId,
        messages: Vec<Message>,
    ) -> alms_core::SessionId {
        let task_id = uuid::Uuid::new_v4();
        let session = mgr.get_or_create(
            AgentId::new(),
            format!("subagent_{}_{task_id}", parent_agent_id.0),
        );
        for msg in messages {
            mgr.append_message(session.id, msg).unwrap();
        }
        session.id
    }

    /// The #1181 pinning test: an ephemeral / unnamed background subagent's
    /// persisted transcript is readable by `session_id` — the exact readback
    /// that failed in the live incident (session `eb90e207-…` had the full
    /// output persisted but the tool reported no session).
    #[tokio::test]
    async fn test_reads_ephemeral_subagent_by_session_id() {
        let (tool, mgr) = make_tool();
        let sid = populate_ephemeral_subagent(
            &mgr,
            tool.parent_agent_id,
            vec![
                make_msg(Role::User, "long research task"),
                make_msg(Role::Assistant, "the full 20k-char output"),
            ],
        );

        let result = tool
            .execute(serde_json::json!({ "session_id": sid.0.to_string() }))
            .await
            .unwrap();

        assert!(
            result.get("error").is_none(),
            "ephemeral session must be readable by session_id, got: {result}"
        );
        assert_eq!(result["session_id"], sid.0.to_string());
        // Ephemeral subagents have no name — the label is null, not a guess.
        assert!(result["subagent"].is_null());
        assert_eq!(result["message_count"], 2);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "the full 20k-char output");
    }

    /// The PR #1185 access-control hole (Tim / Codex): the ephemeral session
    /// UUID is NOT a bearer capability. It leaks beyond the spawning parent
    /// (parent-visible invoke_agent result / completion text; the shared DM
    /// parent session for DM-triggered invocations), and this tool is
    /// registered auto-approved for every agent — so a DIFFERENT agent (e.g.
    /// a DM peer) supplying the exact same UUID must be DENIED. Ownership is
    /// enforced via the parent id embedded in the session context.
    #[tokio::test]
    async fn test_ephemeral_by_session_id_denied_for_non_parent() {
        let (tool, mgr) = make_tool();
        let sid = populate_ephemeral_subagent(
            &mgr,
            tool.parent_agent_id,
            vec![make_msg(
                Role::Assistant,
                "spawning parent's private result",
            )],
        );

        // A different agent (DM peer / any non-parent) learned the UUID.
        let peer = AgentId::new();
        assert_ne!(peer, tool.parent_agent_id);
        let peer_tool = ReadSubagentSessionTool::new(mgr.clone(), peer);

        let result = peer_tool
            .execute(serde_json::json!({ "session_id": sid.0.to_string() }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("another agent's subagent"),
            "a non-parent supplying the leaked UUID must be denied, got: {result}"
        );
        assert!(result.get("messages").is_none());

        // Control: the spawning parent still reads it fine.
        let result = tool
            .execute(serde_json::json!({ "session_id": sid.0.to_string() }))
            .await
            .unwrap();
        assert!(
            result.get("error").is_none(),
            "the spawning parent must still be able to read, got: {result}"
        );
        assert_eq!(result["message_count"], 1);
    }

    /// Legacy ephemeral sessions created before the #1185 hardening have the
    /// old `subagent_{task_id}` context with no parent linkage — ownership
    /// cannot be verified, so they are denied (strictly no worse than
    /// pre-#1181, when ephemeral readback never worked at all). This also
    /// pins that the legacy shape can never fall through to an allow branch.
    #[tokio::test]
    async fn test_legacy_ephemeral_context_without_parent_is_denied() {
        let (tool, mgr) = make_tool();
        let task_id = uuid::Uuid::new_v4();
        let session = mgr.get_or_create(AgentId::new(), format!("subagent_{task_id}"));
        mgr.append_message(session.id, make_msg(Role::Assistant, "old transcript"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("legacy ephemeral subagent session"),
            "legacy no-parent contexts must be denied, got: {result}"
        );
        assert!(result.get("messages").is_none());
    }

    /// `last_n` and `summary_only` behave identically on the by-id path —
    /// both resolution paths converge on the same readback body.
    #[tokio::test]
    async fn test_ephemeral_by_session_id_respects_last_n_and_summary_only() {
        let (tool, mgr) = make_tool();
        let msgs: Vec<Message> = (0..6)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();
        let sid = populate_ephemeral_subagent(&mgr, tool.parent_agent_id, msgs);

        let result = tool
            .execute(serde_json::json!({ "session_id": sid.0.to_string(), "last_n": 2 }))
            .await
            .unwrap();
        assert_eq!(result["message_count"], 6);
        assert_eq!(result["showing"], 2);
        assert_eq!(result["messages"][0]["content"], "msg 4");
        assert_eq!(result["messages"][1]["content"], "msg 5");

        // summary_only with no summary set — fallback shape, same as by-name.
        let result = tool
            .execute(serde_json::json!({
                "session_id": sid.0.to_string(),
                "summary_only": true,
                "last_n": 2,
            }))
            .await
            .unwrap();
        assert_eq!(result["has_summary"], false);
        assert_eq!(result["fallback_message_count"], 6);
        assert_eq!(result["fallback_showing"], 2);
    }

    /// A NAMED subagent session read by id resolves too, recovering the name
    /// from the context — the parent may only hold the session id (e.g. from
    /// a completion notification) and shouldn't need to know which path to
    /// use.
    #[tokio::test]
    async fn test_reads_own_named_subagent_by_session_id() {
        let (tool, mgr) = make_tool();
        populate_subagent(
            &tool,
            &mgr,
            "reviewer",
            vec![make_msg(Role::Assistant, "review done")],
        );
        let stable_id = AgentId::deterministic(tool.parent_agent_id, "reviewer");
        let stable_ctx = format!("subagent_{}_{}", tool.parent_agent_id.0, "reviewer");
        let session = mgr.get_or_create(stable_id, &stable_ctx);

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();
        assert!(result.get("error").is_none(), "got: {result}");
        assert_eq!(result["subagent"], "reviewer");
        assert_eq!(result["message_count"], 1);
    }

    /// Ownership boundary (#1051 carried over to the by-id path): a session
    /// belonging to ANOTHER parent's named subagent is not readable, even
    /// with the exact session id.
    #[tokio::test]
    async fn test_by_session_id_rejects_other_parents_named_subagent() {
        let (tool, mgr) = make_tool();
        let other_parent = AgentId::new();
        assert_ne!(other_parent, tool.parent_agent_id);
        let other_id = AgentId::deterministic(other_parent, "reviewer");
        let other_ctx = format!("subagent_{}_{}", other_parent.0, "reviewer");
        let session = mgr.get_or_create(other_id, &other_ctx);
        mgr.append_message(session.id, make_msg(Role::Assistant, "private"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("another agent's subagent"),
            "must reject another parent's named subagent session, got: {result}"
        );
        assert!(result.get("messages").is_none());
    }

    /// The by-id path is scoped to SUBAGENT sessions only — it must not
    /// become a generic read-any-session-by-uuid bypass of `read_session`'s
    /// ownership model.
    #[tokio::test]
    async fn test_by_session_id_rejects_non_subagent_session() {
        let (tool, mgr) = make_tool();
        // A regular chat session (context does not start with "subagent_").
        let session = mgr.get_or_create(AgentId::new(), "webchat");
        mgr.append_message(session.id, make_msg(Role::User, "private chat"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("not a subagent session"),
            "must reject non-subagent sessions, got: {result}"
        );
        assert!(result.get("messages").is_none());
    }

    #[tokio::test]
    async fn test_by_session_id_unknown_session_is_friendly_error() {
        let (tool, _) = make_tool();
        let unknown = uuid::Uuid::new_v4();
        let result = tool
            .execute(serde_json::json!({ "session_id": unknown.to_string() }))
            .await
            .unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap_or("")
                .contains("No session found"),
            "unknown id must produce the friendly no-session error, got: {result}"
        );
    }

    #[tokio::test]
    async fn test_by_session_id_invalid_uuid_is_error() {
        let (tool, _) = make_tool();
        let err = tool
            .execute(serde_json::json!({ "session_id": "not-a-uuid" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_both_name_and_session_id_is_error() {
        let (tool, _) = make_tool();
        let err = tool
            .execute(serde_json::json!({
                "name": "reviewer",
                "session_id": uuid::Uuid::new_v4().to_string(),
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
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

    // -- #1278: the session moved, the ownership check did not ---------------

    /// A registry-backed manager, so `named_subagent_key` takes the #1278
    /// arm rather than the store-less fallback the rows above exercise.
    fn registry_tool(name: &str) -> (ReadSubagentSessionTool, Arc<SessionManager>, AgentId) {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        let mgr = Arc::new(SessionManager::with_store(SessionConfig::default(), store).unwrap());
        let record = alms_core::AgentRecord {
            id: AgentId::new(),
            name: name.to_string(),
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
        mgr.store().unwrap().create_agent(&record).unwrap();
        let tool = ReadSubagentSessionTool::new(mgr.clone(), AgentId::new());
        (tool, mgr, record.id)
    }

    /// The by-name readback must follow dispatch onto the registry id.
    /// Re-deriving `AgentId::deterministic(parent, name)` here — as this
    /// tool did before #1278 — would miss the session dispatch actually
    /// wrote and report "not invoked yet" for a subagent that just ran.
    #[tokio::test]
    async fn by_name_follows_the_session_onto_the_invoked_agents_registry_id() {
        let (tool, mgr, reviewer) = registry_tool("reviewer");
        let context_id = alms_core::named_subagent_context_id(tool.parent_agent_id, "reviewer");

        // Exactly what dispatch does post-#1278.
        let session = mgr.get_or_create(reviewer, &context_id);
        mgr.append_message(session.id, make_msg(Role::Assistant, "reviewed it"))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "name": "reviewer" }))
            .await
            .unwrap();

        assert_eq!(result["session_id"], session.id.0.to_string());
        assert_eq!(result["message_count"], 1);
        assert_eq!(result["messages"][0]["content"], "reviewed it");
    }

    /// The security half of #1278. `check_subagent_session_access`
    /// authorizes on the parent embedded in the `context_id`, and the
    /// context did not move — so the invoking parent still gets in even
    /// though `session.agent_id` is now the invoked agent's registry id.
    #[tokio::test]
    async fn by_session_id_still_admits_the_invoking_parent_after_the_move() {
        let (tool, mgr, reviewer) = registry_tool("reviewer");
        let context_id = alms_core::named_subagent_context_id(tool.parent_agent_id, "reviewer");
        let session = mgr.get_or_create(reviewer, &context_id);
        mgr.append_message(session.id, make_msg(Role::Assistant, "reviewed it"))
            .unwrap();

        assert_eq!(
            session.agent_id, reviewer,
            "test setup: the session must be filed under the invoked agent"
        );

        let result = tool
            .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
            .await
            .unwrap();

        assert_eq!(result["subagent"], "reviewer");
        assert_eq!(result["messages"][0]["content"], "reviewed it");
    }

    /// ...and the denial half. A peer that merely learned the session UUID
    /// is still refused — including the INVOKED agent itself, which now
    /// owns the row. This tool answers "sessions of subagents *you*
    /// invoked", and the new `agent_id` is deliberately not an input to
    /// that question: authorizing on it would hand the transcript to
    /// whoever the work was delegated TO rather than BY.
    #[tokio::test]
    async fn by_session_id_denies_everyone_but_the_invoking_parent_after_the_move() {
        let (tool, mgr, reviewer) = registry_tool("reviewer");
        let context_id = alms_core::named_subagent_context_id(tool.parent_agent_id, "reviewer");
        let session = mgr.get_or_create(reviewer, &context_id);

        for (label, snooper_parent) in [
            ("unrelated peer", AgentId::new()),
            ("invoked agent", reviewer),
        ] {
            let snooper = ReadSubagentSessionTool::new(mgr.clone(), snooper_parent);
            let result = snooper
                .execute(serde_json::json!({ "session_id": session.id.0.to_string() }))
                .await
                .unwrap();
            assert!(
                result["error"]
                    .as_str()
                    .unwrap_or("")
                    .contains("belongs to another agent's subagent"),
                "{label} must be denied, got: {result}"
            );
        }
    }
}
