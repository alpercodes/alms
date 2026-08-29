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

/// Hard byte cap on the summed *serialized JSON* size of the per-message
/// entries returned in a single `read_messages` call (~15K tokens). Sized
/// to stay well below typical context windows so the response can't
/// single-handedly blow the agent's budget when an LLM forgets to page
/// through long history.
///
/// The unit is JSON bytes -- specifically the `serde_json` rendering of
/// each `{"from": "...", "content": "..."}` object -- so content with many
/// escapable characters (`"`, `\`, newlines, control bytes) is measured at
/// its post-escape size, not its raw UTF-8 size. This is what the LLM
/// actually consumes.
pub const READ_MESSAGES_SERIALIZED_BYTE_CAP: usize = 60_000;

/// Soft backstop on the message count returned in a single `read_messages`
/// call. Guards against pathological inputs (very chatty DMs where each
/// message is a few bytes and the byte cap would never fire).
pub const READ_MESSAGES_MESSAGE_CAP: usize = 200;

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
        "Returns the transcript of your DM conversation with another agent. \
         This is a reader for what was already said -- it is NOT a way to \
         wait for a reply. If a reply is not here yet, the recipient's run \
         may still be in progress; you will be invoked again when they reply \
         (or when they end the conversation), so reading again will not \
         surface it any sooner. Use this to review past exchanges or confirm \
         what was already said. By default returns all messages from your DM \
         session with the specified agent. The response carries `total_count` \
         (real messages in the DM), `returned_count` (what's in the \
         `messages` array), and `truncated: bool` -- check these to detect if \
         older messages were omitted. Pass `last_n` explicitly if you only \
         need a specific count."
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
                    "minimum": 0,
                    "description": "Optional: number of most-recent messages to return. \
                                    Must be a non-negative integer. Omit to return all messages \
                                    (subject to response-size limits indicated by the `truncated` \
                                    flag). Malformed values (negative, non-integer, non-numeric) \
                                    are rejected with InvalidParameters rather than silently \
                                    falling back."
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

        // `last_n` is optional. When the caller passes it explicitly we honor
        // it verbatim (agents can still page deterministically). When omitted
        // (or explicitly `null`) we return everything bounded only by the byte
        // / message-count caps below.
        //
        // Malformed values -- negative numbers, non-integer floats like 3.5,
        // strings, booleans, arrays, objects -- are rejected with a clear
        // `InvalidParameters` error instead of silently falling back to the
        // default-all path. The schema also pins `last_n` to a non-negative
        // integer, but we validate here too because JSON-schema enforcement
        // in the tool-call path is best-effort at the LLM layer and the
        // runtime is the only place we can guarantee deterministic paging.
        // See PR #1028 (Codex P2) for the regression this guards against:
        // pre-fix `params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20)`
        // routed `{ "last_n": -1 }` to a hard fallback of 20; the byte-cap
        // refactor turned the same `None` into "return everything up to the
        // byte / message caps", silently inflating returned context.
        let explicit_last_n = match params.get("last_n") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => {
                if let Some(u) = n.as_u64() {
                    // Genuine non-negative integer in u64 range.
                    Some(u as usize)
                } else {
                    return Err(SandboxError::InvalidParameters(format!(
                        "'last_n' must be a non-negative integer; got {n}"
                    )));
                }
            }
            Some(other) => {
                return Err(SandboxError::InvalidParameters(format!(
                    "'last_n' must be a non-negative integer; got {other}"
                )));
            }
        };

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
        // real conversational messages.
        let real_messages: Vec<&Message> = messages
            .iter()
            .filter(|m| !dm_filter::is_synthetic_marker(m))
            .collect();

        let total_count = real_messages.len();

        // Helper: project a `Message` into its wire-shape JSON value (the
        // exact `{"from": "...", "content": "..."}` object that ends up in
        // the response `messages` array).
        let project = |m: &Message| -> Value {
            let from_agent = m
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("from_agent"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            // Show perspective-aware role: messages from self = "you",
            // messages from others = their name.
            let display_sender = if from_agent == self.agent_name {
                "you"
            } else {
                from_agent
            };

            serde_json::json!({
                "from": display_sender,
                "content": m.content.to_display_string(),
            })
        };

        // Decide which trailing slice to return.
        //
        // Order of precedence:
        //   1. Explicit `last_n` from the caller -- honored verbatim. If the
        //      DM actually has more messages than `last_n` we still flag
        //      `truncated: true` with reason `explicit_last_n` so the agent
        //      knows older messages exist.
        //   2. Otherwise, walk backwards from the newest message and keep
        //      messages as long as neither the byte cap nor the message-count
        //      cap is exceeded. Whichever cap fires first wins.
        //
        // Both branches build the trailing slice in reverse order, then
        // reverse once at the end. The default branch walks the history
        // backwards and breaks as soon as a cap fires, so the work is
        // O(returned_count) rather than O(total_count) -- the 10k-message-DM
        // worst case never materializes a full projection (see #1028 / P2).
        let (mut recent_rev, truncation_reason): (Vec<Value>, Option<&'static str>) =
            match explicit_last_n {
                Some(n) => {
                    let start = total_count.saturating_sub(n);
                    let reason = if start > 0 {
                        Some("explicit_last_n")
                    } else {
                        None
                    };

                    // Project only the trailing `n` messages and reverse so
                    // we can share the post-loop `reverse()` with the default
                    // branch below.
                    let mut acc: Vec<Value> =
                        real_messages[start..].iter().map(|m| project(m)).collect();
                    acc.reverse();
                    (acc, reason)
                }
                None => {
                    let mut acc: Vec<Value> = Vec::new();
                    let mut byte_sum: usize = 0;
                    let mut reason: Option<&'static str> = None;

                    // Iterate newest -> oldest. Measure each projected entry
                    // at its *serialized JSON* size (not raw UTF-8) so the
                    // cap stays accurate even when content contains lots of
                    // escapable characters like `"`, `\`, or newlines.
                    //
                    // As soon as adding the next message would breach either
                    // cap, stop and record the reason. We break BEFORE
                    // serializing+projecting the remaining tail, so the work
                    // is bounded by what we actually return.
                    for m in real_messages.iter().rev() {
                        if acc.len() >= READ_MESSAGES_MESSAGE_CAP {
                            reason = Some("message_cap");
                            break;
                        }
                        let entry = project(m);
                        // Serialize this entry to measure its exact wire-bytes
                        // -- post-escape, with key wrappers -- which is what
                        // the LLM consumes from the tool result. The serialized
                        // string is dropped here (we only keep its length);
                        // the response builder below re-encodes each admitted
                        // entry once when constructing the final JSON array.
                        // The win is still O(returned_count) measurements
                        // instead of O(total_count) projection -- we break
                        // before serializing the rejected tail.
                        let entry_bytes = serde_json::to_string(&entry)
                            .map(|s| s.len())
                            .unwrap_or(usize::MAX);
                        let next_bytes = byte_sum.saturating_add(entry_bytes);
                        if next_bytes > READ_MESSAGES_SERIALIZED_BYTE_CAP {
                            reason = Some("byte_cap");
                            break;
                        }
                        byte_sum = next_bytes;
                        acc.push(entry);
                    }
                    (acc, reason)
                }
            };

        // Restore chronological order: we appended newest-first above.
        recent_rev.reverse();
        let recent = recent_rev;

        let returned_count = recent.len();
        let truncated = truncation_reason.is_some();

        // Get summary if available
        let summary = self
            .session_manager
            .get_summary(session_id)
            .ok()
            .filter(|s| !s.text.is_empty())
            .map(|s| s.text);

        Ok(serde_json::json!({
            "peer": from,
            // Legacy fields kept for back-compat with any consumers that
            // grew up on the old shape; new contract is the explicit
            // `total_count` / `returned_count` / `truncated` triple.
            "message_count": total_count,
            "showing": returned_count,
            "total_count": total_count,
            "returned_count": returned_count,
            "truncated": truncated,
            "truncation_reason": truncation_reason,
            "messages": recent,
            "summary": summary.as_deref().unwrap_or(""),
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
        assert_eq!(result["total_count"], 2);
        assert_eq!(result["returned_count"], 2);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
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
        assert_eq!(result["total_count"], 10);
        assert_eq!(result["returned_count"], 3);
        // Explicit last_n that is smaller than total_count flags truncation
        // with `explicit_last_n` as the reason -- the agent should know
        // older messages exist beyond what it asked for.
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "explicit_last_n");
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "msg 7");
        assert_eq!(msgs[2]["content"], "msg 9");
    }

    /// When the caller passes `last_n` equal to (or larger than) the total
    /// message count, truncation must NOT be flagged -- the agent got
    /// everything it asked for and nothing is being hidden.
    #[tokio::test]
    async fn test_explicit_last_n_meeting_total_is_not_truncated() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        for i in 0..5 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("msg {i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": 10 }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 5);
        assert_eq!(result["returned_count"], 5);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
    }

    /// Default path with a small DM: caller omits `last_n`, gets everything,
    /// and `truncated` stays false.
    #[tokio::test]
    async fn test_default_returns_all_messages_when_fits() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        for i in 0..5 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("msg {i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 5);
        assert_eq!(result["returned_count"], 5);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0]["content"], "msg 0");
        assert_eq!(msgs[4]["content"], "msg 4");
    }

    /// Empty DM session: no messages, no truncation flag.
    #[tokio::test]
    async fn test_default_empty_dm() {
        let (tool, mgr) = make_tool();

        // Materialize the session so the "no session" branch doesn't fire.
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 0);
        assert_eq!(result["returned_count"], 0);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
        assert_eq!(result["messages"].as_array().unwrap().len(), 0);
    }

    /// Byte-cap truncation: messages whose summed serialized content exceeds
    /// `READ_MESSAGES_SERIALIZED_BYTE_CAP` must trip the cap; only the trailing slice
    /// that fits is returned, and `truncation_reason` is `"byte_cap"`.
    #[tokio::test]
    async fn test_default_byte_cap_truncates_to_trailing_slice() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // 10 messages of 10 KiB each -> 100 KiB total, well above the
        // 60 KB cap. Stays under the 200-message backstop so we know
        // the byte cap (not the message cap) is the one that fires.
        let body = "x".repeat(10_000);
        for i in 0..10 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("{i}-{body}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 10);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "byte_cap");

        let returned_count = result["returned_count"].as_u64().unwrap() as usize;
        assert!(
            returned_count > 0,
            "byte cap should leave at least one message"
        );
        assert!(
            returned_count < 10,
            "byte cap should drop at least one message"
        );

        // The trailing slice property: returned messages must be the most
        // recent ones. The newest message is "9-xxx..." so msgs.last()
        // should start with "9-".
        let msgs = result["messages"].as_array().unwrap();
        let last = msgs.last().unwrap();
        assert!(
            last["content"].as_str().unwrap().starts_with("9-"),
            "byte cap should return the most-recent slice, not the oldest"
        );
        // And the first returned message should be later than msg 0 (the
        // very oldest must have been dropped).
        let first = msgs.first().unwrap();
        assert!(
            !first["content"].as_str().unwrap().starts_with("0-"),
            "oldest message should have been truncated when byte cap fires"
        );
    }

    /// Message-count backstop: 250 tiny messages where the byte cap never
    /// fires must still get clipped to the 200-message backstop, with
    /// `truncation_reason: "message_cap"`.
    #[tokio::test]
    async fn test_default_message_cap_backstops_chatty_dms() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // 250 messages, each ~5 bytes. 250 * 5 = 1.25 KiB total, nowhere
        // near the 60 KB byte cap -- so the 200-message backstop is the
        // cap that must fire.
        for i in 0..250 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("m{i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 250);
        assert_eq!(result["returned_count"], 200);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "message_cap");

        // Trailing-slice property: newest message should be present.
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs.last().unwrap()["content"], "m249");
        // And the very first messages must be missing (50 of them).
        assert_eq!(msgs.first().unwrap()["content"], "m50");
    }

    /// Boundary inclusivity: a DM whose summed *serialized JSON* size is
    /// exactly the byte cap must NOT be flagged as truncated. The cap is
    /// `>` not `>=`.
    #[tokio::test]
    async fn test_default_byte_cap_boundary_is_inclusive() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // Compute the serialized wrapper size of a single per-message entry
        // (`{"from":"bob","content":""}`) so we can size the bodies to make
        // the combined serialized sum land EXACTLY on the cap. We can't use
        // raw content length any more -- the cap is measured against the
        // serialized JSON bytes, which include wrapper overhead and any
        // escape expansion.
        let wrapper_overhead = serde_json::to_string(&serde_json::json!({
            "from": "bob",
            "content": "",
        }))
        .unwrap()
        .len();

        // Two messages, each carrying half of the remaining budget after
        // accounting for two wrappers. Bodies are plain ASCII -- no escape
        // expansion -- so raw body bytes equal serialized body bytes.
        let total_body_budget = READ_MESSAGES_SERIALIZED_BYTE_CAP - 2 * wrapper_overhead;
        let half = total_body_budget / 2;
        let body_a = "a".repeat(half);
        let body_b = "b".repeat(total_body_budget - half);
        mgr.append_message(session_id, make_dm_msg("bob", &body_a))
            .unwrap();
        mgr.append_message(session_id, make_dm_msg("bob", &body_b))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 2);
        assert_eq!(result["returned_count"], 2);
        assert_eq!(
            result["truncated"], false,
            "serialized sum equal to the byte cap must not be flagged as truncated"
        );
        assert!(result["truncation_reason"].is_null());
    }

    /// Regression test for PR #1028 P1 (Codex): the byte cap must account
    /// for JSON-escape expansion, not raw UTF-8 length.
    ///
    /// A single message of 60_000 pure `"` characters has raw length
    /// 60_000 (== cap, so the pre-fix raw-length check let it through)
    /// but JSON-escapes to 120_000 characters (each `"` -> `\"`). With
    /// wrapper overhead the serialized entry is well over the 60 KB cap,
    /// so post-fix the cap must fire and `truncation_reason` must be
    /// `byte_cap`.
    #[tokio::test]
    async fn test_default_byte_cap_accounts_for_json_escape_expansion() {
        let (tool, mgr) = make_tool();

        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);

        // 60_000 raw bytes of pure `"`. Raw length == cap so pre-fix
        // (which used raw `content.len()`) would treat this as "fits".
        // JSON-escaped this becomes 120_000 bytes (each `"` -> `\"`),
        // which is double the cap.
        let body = "\"".repeat(60_000);
        mgr.append_message(session_id, make_dm_msg("bob", &body))
            .unwrap();

        let result = tool
            .execute(serde_json::json!({ "from": "bob" }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 1);
        assert_eq!(
            result["truncated"], true,
            "byte cap must fire on JSON-escape-inflated content even when \
             raw UTF-8 length equals the cap"
        );
        assert_eq!(result["truncation_reason"], "byte_cap");
        // The single oversize message can't fit under the cap, so the
        // trailing slice is empty -- caller is told they need to page
        // differently (or accept a larger budget).
        assert_eq!(result["returned_count"], 0);
        assert_eq!(result["messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_schema_requires_from() {
        let (tool, _) = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "from"));
    }

    /// The `last_n` parameter schema must declare itself as a non-negative
    /// integer so JSON-schema validators in the tool-call path can reject
    /// malformed shapes before we ever see them at runtime.
    #[test]
    fn test_schema_last_n_is_non_negative_integer() {
        let (tool, _) = make_tool();
        let schema = tool.parameters();
        let last_n = &schema["properties"]["last_n"];
        assert_eq!(last_n["type"], "integer");
        assert_eq!(last_n["minimum"], 0);
    }

    // -----------------------------------------------------------------
    // Regression tests for PR #1028 (Codex P2): malformed `last_n` must
    // return `InvalidParameters` instead of silently falling back to the
    // default-all path. Before the byte-cap refactor, `as_u64().unwrap_or(20)`
    // mapped every malformed shape to a 20-message fallback; after the
    // refactor the same `None` routed to "return everything up to the byte /
    // message caps", which can materially inflate returned context.
    // -----------------------------------------------------------------

    async fn make_tool_with_dm() -> (ReadMessagesTool, Arc<SessionManager>, SessionId) {
        let (tool, mgr) = make_tool();
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);
        mgr.append_message(session_id, make_dm_msg("bob", "hi"))
            .unwrap();
        (tool, mgr, session_id)
    }

    #[tokio::test]
    async fn test_last_n_negative_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": -1 }))
            .await
            .unwrap_err();
        match err {
            SandboxError::InvalidParameters(msg) => assert!(msg.contains("last_n")),
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_last_n_non_integer_float_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": 3.5 }))
            .await
            .unwrap_err();
        match err {
            SandboxError::InvalidParameters(msg) => assert!(msg.contains("last_n")),
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_last_n_string_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": "three" }))
            .await
            .unwrap_err();
        match err {
            SandboxError::InvalidParameters(msg) => assert!(msg.contains("last_n")),
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_last_n_bool_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": true }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_last_n_array_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": [1, 2, 3] }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_last_n_object_is_invalid_parameters() {
        let (tool, _mgr, _sid) = make_tool_with_dm().await;
        let err = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": { "n": 5 } }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    /// `last_n: null` is semantically equivalent to omitting the field: fall
    /// through to the default-all path bounded by the byte / message-count
    /// caps. This is a pin to keep the behavior explicit -- some JSON clients
    /// always emit nulls for absent optional fields, and we don't want to
    /// punish them with InvalidParameters.
    #[tokio::test]
    async fn test_last_n_null_falls_through_to_default_all() {
        let (tool, mgr) = make_tool();
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);
        for i in 0..3 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("m{i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": null }))
            .await
            .unwrap();

        // All 3 messages returned; no truncation.
        assert_eq!(result["total_count"], 3);
        assert_eq!(result["returned_count"], 3);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_reason"].is_null());
    }

    /// `last_n: 0` is a valid request -- the agent is asking for zero messages
    /// (effectively a count probe via the `total_count` / `truncated` metadata).
    /// We honor it, return an empty messages array, and flag truncation iff
    /// the DM actually has messages the agent isn't seeing.
    #[tokio::test]
    async fn test_last_n_zero_returns_empty_with_truncation_when_nonempty() {
        let (tool, mgr) = make_tool();
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);
        for i in 0..5 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("m{i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": 0 }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 5);
        assert_eq!(result["returned_count"], 0);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "explicit_last_n");
        assert_eq!(result["messages"].as_array().unwrap().len(), 0);
    }

    /// Regression guard: a valid positive `last_n` must still work after the
    /// validation tightening. This re-asserts the happy path that
    /// `test_last_n_limits` covers, but stays close to the malformed-input
    /// tests so future regressions are obvious in the diff.
    #[tokio::test]
    async fn test_last_n_valid_positive_still_works() {
        let (tool, mgr) = make_tool();
        let session_id = SessionId::deterministic_dm("alice", "bob");
        let dm_ctx = dm_context_id("alice", "bob");
        let _session = mgr.get_or_create_shared(session_id, &dm_ctx);
        for i in 0..10 {
            mgr.append_message(session_id, make_dm_msg("bob", &format!("m{i}")))
                .unwrap();
        }

        let result = tool
            .execute(serde_json::json!({ "from": "bob", "last_n": 5 }))
            .await
            .unwrap();

        assert_eq!(result["total_count"], 10);
        assert_eq!(result["returned_count"], 5);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["truncation_reason"], "explicit_last_n");
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "m5");
        assert_eq!(msgs[4]["content"], "m9");
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
