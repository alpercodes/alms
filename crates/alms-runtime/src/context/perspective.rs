//! DM-perspective role mapping and reasoning-message filter.
//!
//! For shared sessions (DM/group), all messages are stored as `Role::User`
//! and the actual role depends on who is reading the session.
//! [`apply_perspective`] flips messages whose `from_agent` metadata matches
//! the reading agent into `Role::Assistant` (or `Role::Tool` for tool
//! results) so the LLM sees its own previous responses correctly, including
//! its own prior tool calls and tool results across DM turns (#988).
//!
//! [`is_peer_reasoning_message`] gates the reasoning-message filter that
//! runs immediately before perspective mapping in [`super::ContextBuilder`]:
//! reasoning messages from the *peer* agent (thinking text / tool calls /
//! tool results persisted as `Role::User` with `message_type: "reasoning"`
//! to preserve the DM invariant) are filtered from the LLM context to avoid
//! token waste and malformed messages (a peer's `ToolResult` stored as
//! `Role::User` cannot be correctly handled by `rebuild::session_msg_to_llm`
//! — C2 in the #930 review). Reasoning rows from the *perspective* agent
//! itself survive the filter and are re-stamped onto their canonical roles
//! so [`super::rebuild::session_msg_to_llm`] reconstructs them as proper
//! `assistant` / `tool` messages — that restores same-agent tool-call
//! coherence across DM turn boundaries (#988).
//!
//! Pure free functions — no `&self`, no shared state.

use alms_session::{Content, Message, Role};

/// Read the `from_agent` field from a message's metadata, if present.
pub(super) fn from_agent_of(msg: &Message) -> Option<&str> {
    msg.metadata
        .as_ref()
        .and_then(|m| m.get("from_agent"))
        .and_then(|v| v.as_str())
}

/// Returns `true` if the message is an internal reasoning message
/// (persisted during DM agent loops with `message_type: "reasoning"`).
///
/// These messages contain the agent's thinking text, tool calls, and tool
/// results stored as `Role::User` to preserve the DM invariant. The filter
/// site uses [`is_peer_reasoning_message`] in practice — see that function
/// for why same-agent reasoning rows must be retained — but this predicate
/// is exposed for tests that exercise the raw shape.
pub(super) fn is_reasoning_message(msg: &Message) -> bool {
    msg.metadata
        .as_ref()
        .and_then(|m| m.get("message_type"))
        .and_then(|v| v.as_str())
        == Some("reasoning")
}

/// Returns `true` if the message is a reasoning message from an agent
/// other than `perspective_agent` (i.e. the peer in a DM).
///
/// Peer reasoning is filtered from the LLM context: the reading agent
/// shouldn't pay tokens for the peer's internal thinking, and a peer's
/// tool result stored as `Role::User` would hit the catch-all in
/// `rebuild::session_msg_to_llm` and produce a malformed message (C2).
///
/// Same-agent reasoning rows DO survive — they carry the agent's own
/// prior tool calls/results, which it needs for working memory across
/// DM turns. [`apply_perspective`] re-stamps those surviving rows onto
/// their canonical `assistant` / `tool` roles so the rebuild path
/// reconstructs them correctly (#988).
pub(super) fn is_peer_reasoning_message(msg: &Message, perspective_agent: &str) -> bool {
    if !is_reasoning_message(msg) {
        return false;
    }
    match from_agent_of(msg) {
        // Same-agent reasoning -> keep (handled by apply_perspective).
        Some(sender) if sender == perspective_agent => false,
        // Peer reasoning, or reasoning with no from_agent attribution -> drop.
        _ => true,
    }
}

/// Apply perspective mapping to a message.
///
/// For shared sessions (DM/group), all messages are stored as `Role::User`.
/// When building context for a specific agent:
///
/// - **Plain DM messages** (`message_type: "dm"`) from the perspective
///   agent map to `Role::Assistant` so the LLM sees them as its own prior
///   replies.
/// - **Reasoning messages** (`message_type: "reasoning"`) from the
///   perspective agent — assistant text, tool calls, tool results — are
///   re-stamped onto their canonical roles based on `Content` shape:
///   - `Content::ToolCall { .. }` -> `Role::Assistant` (rebuild path
///     reconstructs the structured `tool_calls` array).
///   - `Content::ToolResult { .. }` -> `Role::Tool` (rebuild path
///     reconstructs the structured tool-result message with
///     `tool_call_id`).
///   - `Content::Text(_)` and other content shapes -> `Role::Assistant`
///     (assistant prose).
///
///   This re-stamping is what restores same-agent tool-call coherence
///   across DM turns (#988): the perspective agent sees its own prior
///   `tool_use`/`tool_result` blocks instead of having them filtered out.
///
/// - **Other agents' messages** keep their original role.
pub(super) fn apply_perspective(msg: &Message, perspective_agent: &str) -> Message {
    let from_agent = from_agent_of(msg);

    match from_agent {
        Some(sender) if sender == perspective_agent => {
            let mut mapped = msg.clone();
            // Re-stamp same-agent reasoning rows onto their canonical
            // role so `rebuild::session_msg_to_llm` reconstructs them
            // structurally. For non-reasoning rows (plain DM bodies)
            // the existing flip-to-Assistant behaviour is preserved.
            if is_reasoning_message(msg) {
                mapped.role = match &msg.content {
                    Content::ToolResult { .. } => Role::Tool,
                    // ToolCall, Text, Image -> assistant. The rebuild
                    // path's `(Role::Assistant, Content::ToolCall)` arm
                    // reconstructs structured tool_calls; assistant
                    // text/image content goes through the catch-all
                    // assistant arm as plain text/display-string.
                    _ => Role::Assistant,
                };
            } else {
                // Plain DM message from this agent -> Assistant.
                mapped.role = Role::Assistant;
            }
            mapped
        }
        _ => {
            // Someone else's message (or no metadata) -> keep original role.
            msg.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::{make_msg, make_msg_with_meta, make_msg_with_metadata};
    use super::apply_perspective;
    use alms_core::config::ContextConfig;
    use alms_session::{Content, Message, Role};

    #[test]
    fn test_apply_perspective_no_metadata_stays_user() {
        let msg = make_msg(Role::User, "hello from nowhere");
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(mapped.role, Role::User);
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "hello from nowhere"
        );
    }

    #[test]
    fn test_apply_perspective_matching_agent_becomes_assistant() {
        let msg = make_msg_with_metadata(
            Role::User,
            "I said this",
            Some(serde_json::json!({"from_agent": "agent-alpha"})),
        );
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "own message should map to Assistant"
        );
    }

    #[test]
    fn test_apply_perspective_different_agent_stays_user() {
        let msg = make_msg_with_metadata(
            Role::User,
            "someone else said this",
            Some(serde_json::json!({"from_agent": "agent-beta"})),
        );
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::User,
            "other agent's message should stay User"
        );
    }

    #[test]
    fn test_apply_perspective_metadata_without_from_agent_stays_user() {
        let msg = make_msg_with_metadata(
            Role::User,
            "some random metadata",
            Some(serde_json::json!({"other_key": "other_value"})),
        );
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::User,
            "metadata without from_agent should stay User"
        );
    }

    #[test]
    fn test_apply_perspective_system_role_unchanged() {
        // System messages should pass through unchanged even with from_agent
        // metadata, because the function only checks from_agent == perspective_agent
        // to map to Assistant. A matching from_agent on a System message is an
        // unusual edge case but documents the current behavior.

        // System message without metadata -> stays System
        let msg = make_msg_with_metadata(Role::System, "system prompt text", None);
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::System,
            "System message without metadata should stay System"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "system prompt text",
            "content should be preserved"
        );
        assert_eq!(mapped.metadata, None, "metadata should be preserved");

        // System message with non-matching from_agent -> stays System
        let metadata = serde_json::json!({"from_agent": "agent-beta"});
        let msg =
            make_msg_with_metadata(Role::System, "system instructions", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::System,
            "System message from another agent should stay System"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "system instructions",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );
    }

    #[test]
    fn test_apply_perspective_tool_role_unchanged() {
        // Tool-result messages should pass through unchanged when from_agent
        // does not match the perspective agent (the common case).

        // Tool message without metadata -> stays Tool
        let msg = make_msg_with_metadata(Role::Tool, "tool output", None);
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Tool,
            "Tool message without metadata should stay Tool"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "tool output",
            "content should be preserved"
        );
        assert_eq!(mapped.metadata, None, "metadata should be preserved");

        // Tool message with non-matching from_agent -> stays Tool
        let metadata = serde_json::json!({"from_agent": "agent-beta"});
        let msg = make_msg_with_metadata(Role::Tool, "tool result payload", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Tool,
            "Tool message from another agent should stay Tool"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "tool result payload",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );
    }

    #[test]
    fn test_apply_perspective_assistant_role_unchanged_no_match() {
        // An Assistant message with non-matching (or absent) from_agent should
        // keep its role. This can happen when an Assistant message from one
        // agent is loaded into a DM session that is then viewed from a
        // different agent's perspective.

        let metadata = serde_json::json!({"from_agent": "agent-beta"});
        let msg =
            make_msg_with_metadata(Role::Assistant, "I replied earlier", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "Assistant message from another agent should stay Assistant"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "I replied earlier",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );
    }

    #[test]
    fn test_apply_perspective_assistant_role_with_matching_agent_stays_assistant() {
        // Idempotent case: an Assistant message whose from_agent matches the
        // perspective agent should remain Assistant. The match arm unconditionally
        // sets role = Assistant, so this is a no-op, but the test documents
        // completeness across the role matrix.

        let metadata = serde_json::json!({"from_agent": "agent-alpha"});
        let msg =
            make_msg_with_metadata(Role::Assistant, "I already replied", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "Assistant message with matching from_agent should stay Assistant"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "I already replied",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );
    }

    #[test]
    fn test_apply_perspective_non_user_role_with_matching_agent_becomes_assistant() {
        // Documents current behavior: if from_agent matches the perspective
        // agent, the role is unconditionally overwritten to Assistant,
        // regardless of the original role. This is an edge case -- in practice
        // DM messages are always stored as Role::User -- but the test locks
        // down the behavior so any future change is intentional.

        // System message with matching from_agent -> mapped to Assistant
        let metadata = serde_json::json!({"from_agent": "agent-alpha"});
        let msg = make_msg_with_metadata(Role::System, "system from self", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "matching from_agent should override even System role to Assistant"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "system from self",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );

        // Tool message with matching from_agent -> mapped to Assistant
        let metadata = serde_json::json!({"from_agent": "agent-alpha"});
        let msg = make_msg_with_metadata(Role::Tool, "tool from self", Some(metadata.clone()));
        let mapped = apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "matching from_agent should override even Tool role to Assistant"
        );
        assert_eq!(
            match &mapped.content {
                Content::Text(t) => t.as_str(),
                _ => "",
            },
            "tool from self",
            "content should be preserved"
        );
        assert_eq!(
            mapped.metadata,
            Some(metadata),
            "metadata should be preserved"
        );
    }

    /// Same-agent reasoning rows (message_type="reasoning") in a DM
    /// session must SURVIVE into the perspective agent's context window
    /// and be re-stamped onto their canonical roles, so the agent retains
    /// working memory across DM turn boundaries (#988). Peer reasoning
    /// rows must still be filtered (token waste + malformed `Role::User`
    /// tool results, the original C2 from the #930 review).
    #[test]
    fn test_same_agent_reasoning_survives_in_dm_context() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Simulates a DM history where Bob made tool calls on his prior
        // turn (reasoning text + fs_read tool_use + tool_result), Alice
        // replied, and now Bob is taking another turn. The tool calls
        // were persisted as `Role::User` + `message_type: "reasoning"`
        // per the DM invariant. From Bob's perspective they should
        // reappear as structured assistant / tool messages.
        let history = vec![
            // Alice's first DM
            make_msg_with_meta(
                Role::User,
                Content::Text("Hello Bob!".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Bob's prior-turn reasoning prose
            make_msg_with_meta(
                Role::User,
                Content::Text("Let me read the file first.".to_string()),
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "run_id": "run-bob-1",
                }),
            ),
            // Bob's prior-turn tool call (fs_read)
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "fs_read".to_string(),
                    params: serde_json::json!({ "path": "/tmp/x.txt" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-bob-1",
                }),
            ),
            // Bob's prior-turn tool result
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-bob-1".to_string(),
                    result: serde_json::json!("file contents: foo bar baz"),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            // Bob's prior-turn user-facing reply (sent via send_message)
            make_msg_with_meta(
                Role::User,
                Content::Text("I read the file, baz is in there.".to_string()),
                serde_json::json!({ "from_agent": "bob", "message_type": "dm" }),
            ),
            // Alice's reply (this run's incoming DM)
            make_msg_with_meta(
                Role::User,
                Content::Text("Cool, where exactly?".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
        ];

        // Build context with perspective mapping (Bob's view).
        let messages =
            builder.build_with_perspective("System prompt.", &history, "", None, Some("bob"), None);

        // Expected (in order):
        //   [0] system
        //   [1] user      ("Hello Bob!")        — Alice
        //   [2] assistant ("Let me read...")    — Bob's prior reasoning text
        //   [3] assistant tool_calls=[fs_read]  — Bob's prior tool_use
        //   [4] tool      ("file contents...")  — Bob's prior tool_result
        //   [5] assistant ("I read the file...")— Bob's prior DM reply
        //   [6] user      ("Cool, where...")    — Alice's incoming DM
        assert_eq!(
            messages.len(),
            7,
            "expected 7 messages, got {}: shape = {:?}",
            messages.len(),
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        );
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content_str(), "Hello Bob!");

        // Bob's reasoning prose -> assistant text.
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content_str(), "Let me read the file first.");
        assert!(messages[2].tool_calls.is_none());

        // Bob's tool_use -> structured assistant tool_calls.
        assert_eq!(messages[3].role, "assistant");
        assert!(
            messages[3].content.is_none(),
            "tool_call assistant message must have content=None"
        );
        let calls = messages[3]
            .tool_calls
            .as_ref()
            .expect("Bob's prior tool_use must be reconstructed as structured tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tc-bob-1");
        assert_eq!(calls[0].function.name, "fs_read");

        // Bob's tool_result -> structured tool message.
        assert_eq!(messages[4].role, "tool");
        assert_eq!(messages[4].tool_call_id.as_deref(), Some("tc-bob-1"));
        assert!(messages[4].content_str().contains("foo bar baz"));

        // Bob's prior DM body -> assistant text.
        assert_eq!(messages[5].role, "assistant");
        assert_eq!(
            messages[5].content_str(),
            "I read the file, baz is in there."
        );

        // Alice's incoming DM stays user.
        assert_eq!(messages[6].role, "user");
        assert_eq!(messages[6].content_str(), "Cool, where exactly?");
    }

    /// A DM with reasoning rows from the PEER agent (Alice) must keep
    /// filtering them — peer isolation is the deliberate dual-perspective
    /// contract, and a peer's `Role::User` + `Content::ToolResult` still
    /// hits the rebuild catch-all (C2). Bob, building his own context,
    /// must not see Alice's tool calls / results / reasoning text.
    #[test]
    fn test_peer_reasoning_still_filtered_in_dm_context() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            // Alice's DM
            make_msg_with_meta(
                Role::User,
                Content::Text("Hi Bob, can you check the build?".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Alice's reasoning prose (must NOT reach Bob)
            make_msg_with_meta(
                Role::User,
                Content::Text("Internal: I should ask Bob to check.".to_string()),
                serde_json::json!({
                    "from_agent": "alice",
                    "message_type": "reasoning",
                    "run_id": "run-alice-1",
                }),
            ),
            // Alice's tool_use (must NOT reach Bob — peer isolation)
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "shell".to_string(),
                    params: serde_json::json!({ "command": "git status" }),
                },
                serde_json::json!({
                    "from_agent": "alice",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-alice-1",
                }),
            ),
            // Alice's tool_result (must NOT reach Bob; this is the
            // exact malformed-shape case from C2 — `Role::User` +
            // `Content::ToolResult`)
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-alice-1".to_string(),
                    result: serde_json::json!("On branch main"),
                },
                serde_json::json!({
                    "from_agent": "alice",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            // Bob's prior DM reply
            make_msg_with_meta(
                Role::User,
                Content::Text("Yes, on it.".to_string()),
                serde_json::json!({ "from_agent": "bob", "message_type": "dm" }),
            ),
        ];

        // Build context with perspective mapping (Bob's view).
        let messages =
            builder.build_with_perspective("System prompt.", &history, "", None, Some("bob"), None);

        // Bob should see ONLY: system, Alice's DM (user), his own DM
        // reply (assistant). Plus a synthesised trailing-user
        // placeholder because his own assistant turn would otherwise
        // sit at the tail of the messages array.
        // Alice's reasoning prose / tool_use / tool_result must all be
        // filtered.
        let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(messages[0].role, "system");
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "context must end with user; got roles={roles:?}"
        );

        // No tool_calls / tool_call_id / tool roles must appear at all
        // — that would mean Alice's tool work leaked into Bob's view.
        assert!(
            !messages.iter().any(|m| m.role == "tool"),
            "peer's tool_result must not appear in Bob's context: roles={roles:?}"
        );
        assert!(
            !messages.iter().any(|m| m.tool_calls.is_some()),
            "peer's tool_calls must not appear in Bob's context: roles={roles:?}"
        );
        // Alice's reasoning prose body must not surface.
        assert!(
            !messages
                .iter()
                .any(|m| m.content_str().contains("I should ask Bob")),
            "Alice's reasoning text leaked into Bob's context"
        );
        // Bob's own DM reply must be present as assistant.
        assert!(
            messages
                .iter()
                .any(|m| m.role == "assistant" && m.content_str() == "Yes, on it."),
            "Bob's own prior DM reply missing from his context"
        );
    }

    /// Regression: NO perspective mapping (regular non-DM session) must
    /// be byte-identical to pre-fix behaviour. The reasoning filter
    /// only fires when `perspective_agent` is `Some`, and the rebuild
    /// path for `(Role::Assistant, Content::ToolCall)` /
    /// `(Role::Tool, Content::ToolResult)` is unchanged. This test
    /// drives a regular session with prior tool calls and pins the
    /// canonical shape so the #988 fix cannot regress non-DM behaviour.
    #[test]
    fn test_non_dm_tool_calls_unchanged_by_fix() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Regular session: assistant tool_call + tool_result on prior
        // turn, then user follow-up. No perspective mapping involved.
        let history = vec![
            make_msg(Role::User, "read /etc/hosts"),
            // tool_call_id metadata is the only field rebuild reads here.
            make_msg_with_meta(
                Role::Assistant,
                Content::ToolCall {
                    name: "fs_read".to_string(),
                    params: serde_json::json!({ "path": "/etc/hosts" }),
                },
                serde_json::json!({ "tool_call_id": "call_X" }),
            ),
            make_msg_with_meta(
                Role::Tool,
                Content::ToolResult {
                    tool_id: "call_X".to_string(),
                    result: serde_json::json!("127.0.0.1 localhost"),
                },
                serde_json::json!({ "ok": true }),
            ),
            make_msg(Role::Assistant, "done"),
        ];

        // No perspective — regular session.
        let messages = builder.build("You are helpful.", &history, "now ls", None);

        // system + user("read /etc/hosts") + assistant w/ tool_calls
        // + tool + assistant("done") + user("now ls") = 6
        assert_eq!(messages.len(), 6, "non-DM shape must be unchanged");
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content_str(), "read /etc/hosts");

        // Structured tool_calls preserved.
        assert_eq!(messages[2].role, "assistant");
        assert!(messages[2].content.is_none());
        let calls = messages[2].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_X");
        assert_eq!(calls[0].function.name, "fs_read");

        // Tool result preserved.
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call_X"));
        assert!(messages[3].content_str().contains("localhost"));

        assert_eq!(messages[4].role, "assistant");
        assert_eq!(messages[4].content_str(), "done");
        assert_eq!(messages[5].role, "user");
        assert_eq!(messages[5].content_str(), "now ls");
    }

    /// Without perspective mapping (non-DM sessions), reasoning messages
    /// are not filtered — they should not appear in non-DM sessions in
    /// practice, but if they do, they pass through unchanged.
    #[test]
    fn test_reasoning_messages_not_filtered_without_perspective() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg_with_meta(
                Role::User,
                Content::Text("Reasoning text".to_string()),
                serde_json::json!({ "message_type": "reasoning" }),
            ),
            make_msg(Role::Assistant, "Response"),
        ];

        // No perspective mapping
        let messages = builder.build("System.", &history, "Input", None);

        // Without perspective, reasoning filter does not fire. Shape after
        // normalize: two adjacent user messages (Hello + reasoning text)
        // merge into one, then assistant, then current input user =
        // system + user(merged) + assistant + user = 4.
        assert_eq!(
            messages.len(),
            4,
            "reasoning text surfaces (no filter) but adjacent users merge"
        );
        assert!(messages[1].content_str().contains("Hello"));
        assert!(messages[1].content_str().contains("Reasoning text"));
    }

    /// N1 from Tim's #1009 review: parallel tool calls inside a DM must
    /// survive the perspective filter and be regrouped into a single
    /// assistant tool_calls message by `normalize::group_tool_calls`.
    ///
    /// Mirrors `rebuild::tests::test_parallel_tool_calls_grouped` but
    /// drives the DM-perspective path: each tool_use row is persisted as
    /// `Role::User` + `message_type: "reasoning"`, gets re-stamped to
    /// `Role::Assistant` with structured `tool_calls`, and the three
    /// adjacent assistant tool_call messages collapse into one.
    #[test]
    fn test_parallel_tool_calls_in_dm_grouped() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Bob fired three tools in parallel on a prior DM turn. The
        // runtime persists each tool_use as its own Role::User row
        // (DM invariant) tagged `message_type: "reasoning"`, then
        // each tool_result with the same metadata shape — exactly
        // what `process_tool_results` writes (loop_impl.rs).
        let history = vec![
            // Alice's incoming DM
            make_msg_with_meta(
                Role::User,
                Content::Text("Status report please.".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Bob's three parallel tool_use rows
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "shell".to_string(),
                    params: serde_json::json!({ "command": "ls" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-A",
                }),
            ),
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "shell".to_string(),
                    params: serde_json::json!({ "command": "df -h" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-B",
                }),
            ),
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "echo".to_string(),
                    params: serde_json::json!({ "text": "done" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-C",
                }),
            ),
            // Three matching tool_result rows
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-A".to_string(),
                    result: serde_json::json!("file1.txt"),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-B".to_string(),
                    result: serde_json::json!("/dev/sda1 50G"),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-C".to_string(),
                    result: serde_json::json!("done"),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            // Bob's user-facing reply via send_message
            make_msg_with_meta(
                Role::User,
                Content::Text("All systems nominal.".to_string()),
                serde_json::json!({ "from_agent": "bob", "message_type": "dm" }),
            ),
            // Alice's follow-up — the trailing user turn
            make_msg_with_meta(
                Role::User,
                Content::Text("Anything alarming?".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
        ];

        // Build context with perspective mapping (Bob's view).
        let messages =
            builder.build_with_perspective("System prompt.", &history, "", None, Some("bob"), None);

        // Expected canonical shape:
        //   [0] system
        //   [1] user      ("Status report please.")     — Alice
        //   [2] assistant tool_calls=[tc-A, tc-B, tc-C] — three parallel calls grouped
        //   [3] tool      ("file1.txt")                 — tc-A result
        //   [4] tool      ("/dev/sda1 50G")             — tc-B result
        //   [5] tool      ("done")                      — tc-C result
        //   [6] assistant ("All systems nominal.")      — Bob's prior DM reply
        //   [7] user      ("Anything alarming?")        — Alice's follow-up
        assert_eq!(
            messages.len(),
            8,
            "expected 8 messages with grouped parallel tool_calls, got {}: roles = {:?}",
            messages.len(),
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        );
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content_str(), "Status report please.");

        // Three parallel tool_use rows from Bob must collapse into ONE
        // assistant message with three `tool_calls` entries — the
        // shape Anthropic / OpenAI both require for parallel tool use.
        assert_eq!(messages[2].role, "assistant");
        assert!(
            messages[2].content.is_none(),
            "grouped tool_calls assistant message must have content=None"
        );
        let calls = messages[2]
            .tool_calls
            .as_ref()
            .expect("grouped tool_calls array must exist");
        assert_eq!(
            calls.len(),
            3,
            "three parallel tool_use rows must merge into a single assistant message with 3 calls",
        );
        assert_eq!(calls[0].id, "tc-A");
        assert_eq!(calls[1].id, "tc-B");
        assert_eq!(calls[2].id, "tc-C");
        assert_eq!(calls[0].function.name, "shell");
        assert_eq!(calls[2].function.name, "echo");

        // Three structured tool-result messages, matched by tool_call_id.
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("tc-A"));
        assert!(messages[3].content_str().contains("file1.txt"));
        assert_eq!(messages[4].role, "tool");
        assert_eq!(messages[4].tool_call_id.as_deref(), Some("tc-B"));
        assert!(messages[4].content_str().contains("sda1"));
        assert_eq!(messages[5].role, "tool");
        assert_eq!(messages[5].tool_call_id.as_deref(), Some("tc-C"));
        assert!(messages[5].content_str().contains("done"));

        // Bob's user-facing reply, then Alice's follow-up.
        assert_eq!(messages[6].role, "assistant");
        assert_eq!(messages[6].content_str(), "All systems nominal.");
        assert_eq!(messages[7].role, "user");
        assert_eq!(messages[7].content_str(), "Anything alarming?");
    }

    /// Regression test for #1011 — partial tool-call persistence in a DM.
    ///
    /// When `run_tool_calls` returns `Err` (cancellation, tool-execution
    /// failure) AFTER `persist_assistant_tool_calls` ran but BEFORE
    /// `process_tool_results` ran (loop_impl.rs:391-403), the session
    /// captures the `tool_use` row WITHOUT a matching `tool_result` row.
    /// `finish_run`'s `Err(_)` arm then appends a `[Run failed: ...]`
    /// text bubble (agent/mod.rs:1091-1118) right after the orphan
    /// `tool_use`.
    ///
    /// Pre-#1009 the DM filter dropped the `tool_use` outright (it was
    /// tagged `message_type: "reasoning"`), so Anthropic never saw the
    /// orphan and never 400'd. Post-#1009 the same-agent `tool_use`
    /// survives — and would 400 with `tool_use ids were found without
    /// tool_result blocks immediately after` (#1011) UNLESS the
    /// `close_pending_tool_calls` pass in `normalize_for_llm` synthesises
    /// the missing `tool_result`.
    ///
    /// This test pins that the pipeline DOES synthesise it, so #1009
    /// incidentally fixes the #1011 partial-persistence shape rather
    /// than introducing a new 400.
    #[test]
    fn test_dm_partial_tool_call_persistence_synthesises_interrupted_result() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Bob's prior DM turn: emitted a tool_use, the tool was
        // cancelled / failed mid-flight, NO matching tool_result row,
        // then `[Run failed: ...]` text marker landed in the session.
        let history = vec![
            // Alice's DM
            make_msg_with_meta(
                Role::User,
                Content::Text("Read the config file please.".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Bob's tool_use — persisted by persist_assistant_tool_calls
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "fs_read".to_string(),
                    params: serde_json::json!({ "path": "/etc/config" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "toolu_01PiJySgUeQStkG6yUSaPPMZ",
                }),
            ),
            // No matching tool_result row — the tool execution path
            // returned Err before process_tool_results could fire.
            // `[Run failed: ...]` text bubble persisted by
            // finish_run's Err(_) arm with dm_marker_metadata
            // (message_type: "dm", from_agent: "bob").
            make_msg_with_meta(
                Role::User,
                Content::Text("[Run failed: Subagent LLM request rejected]".to_string()),
                serde_json::json!({ "from_agent": "bob", "message_type": "dm" }),
            ),
            // Alice's follow-up DM (the trigger for the next run).
            make_msg_with_meta(
                Role::User,
                Content::Text("What happened?".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
        ];

        // Build context with perspective mapping (Bob's view).
        let messages =
            builder.build_with_perspective("System prompt.", &history, "", None, Some("bob"), None);

        // The orphan tool_use must be paired with a synthetic
        // `[Tool execution was interrupted]` tool_result so the
        // canonical-shape invariant holds (#586) and Anthropic does
        // not 400.
        let assistant_with_calls = messages
            .iter()
            .find(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect(
                "Bob's tool_use must survive #1009 perspective filter and reach LLM context as assistant",
            );
        let calls = assistant_with_calls.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_01PiJySgUeQStkG6yUSaPPMZ");
        assert_eq!(calls[0].function.name, "fs_read");

        // The synthetic interrupted tool_result must exist with the
        // matching tool_call_id — this is what prevents Anthropic from
        // returning `tool_use ids were found without tool_result blocks
        // immediately after`.
        let synthetic_result = messages
            .iter()
            .find(|m| {
                m.role == "tool"
                    && m.tool_call_id.as_deref() == Some("toolu_01PiJySgUeQStkG6yUSaPPMZ")
            })
            .expect(
                "close_pending_tool_calls must synthesise an interrupted tool_result \
                 for the orphan tool_use — Anthropic 400's without it (#1011)",
            );
        assert!(
            synthetic_result.content_str().contains("interrupted"),
            "synthetic tool_result must use INTERRUPTED_TOOL_RESULT wording, got: {:?}",
            synthetic_result.content,
        );

        // Wire-shape sanity: the assistant tool_use must be IMMEDIATELY
        // followed by its tool_result (Anthropic strict-pairing).
        let tool_use_idx = messages
            .iter()
            .position(|m| m.role == "assistant" && m.tool_calls.is_some())
            .unwrap();
        assert_eq!(
            messages[tool_use_idx + 1].role,
            "tool",
            "tool_use must be immediately followed by tool_result: shape = {:?}",
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        );
        assert_eq!(
            messages[tool_use_idx + 1].tool_call_id.as_deref(),
            Some("toolu_01PiJySgUeQStkG6yUSaPPMZ"),
        );

        // Trailing user invariant.
        assert_eq!(messages.last().unwrap().role, "user");
    }

    /// #1011 candidate (1) regression: ASYMMETRIC reasoning metadata.
    ///
    /// The persistence sites for tool_use (loop_impl.rs:802) and
    /// tool_result (loop_impl.rs:925) both branch on the same `is_dm`
    /// flag in production, so they SHOULD always land in the session
    /// with consistent `message_type: "reasoning"` metadata. But if a
    /// future bug ever desynchronised them (tool_use without reasoning
    /// meta, or tool_result without reasoning meta), the perspective
    /// filter and rebuild path must still produce a wire-valid context.
    ///
    /// This test covers the worse half: tool_use is tagged but
    /// tool_result is NOT (so the result gets stripped by perspective
    /// filtering as a peer-attributed row, leaving the orphan tool_use).
    /// `close_pending_tool_calls` synthesises the missing
    /// `[Tool execution was interrupted]` to keep Anthropic happy.
    #[test]
    fn test_dm_asymmetric_reasoning_metadata_does_not_400() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg_with_meta(
                Role::User,
                Content::Text("Look up X".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Bob's tool_use WITH reasoning meta (survives the filter,
            // gets re-stamped to assistant by apply_perspective).
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "fs_read".to_string(),
                    params: serde_json::json!({ "path": "/x" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-asym",
                }),
            ),
            // Bob's tool_result WITHOUT reasoning meta — simulates the
            // hypothetical asymmetric-persistence bug. With no
            // `message_type: "reasoning"` flag, the perspective filter
            // does not target this row, but it also has no
            // `from_agent: "bob"` flag, so apply_perspective leaves it
            // as `Role::User` + `Content::ToolResult`, which the
            // rebuild path's catch-all renders as plain user text
            // (`to_display_string`) — the orphan tool_use case kicks
            // in and `close_pending_tool_calls` covers it.
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::User,
                content: Content::ToolResult {
                    tool_id: "tc-asym".to_string(),
                    result: serde_json::json!("X content"),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
            make_msg_with_meta(
                Role::User,
                Content::Text("And?".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
        ];

        let messages =
            builder.build_with_perspective("System prompt.", &history, "", None, Some("bob"), None);

        // The assistant tool_use must be paired with SOME tool_result
        // — either the rebuilt-from-history one (if rebuild routes it
        // there) or a synthetic interrupted one. Either way Anthropic
        // gets a strict-paired (tool_use, tool_result) and does NOT 400.
        let tool_use_idx = messages
            .iter()
            .position(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect("Bob's tool_use must reach the wire shape");
        assert_eq!(
            messages[tool_use_idx + 1].role,
            "tool",
            "tool_use must be immediately followed by tool_result regardless of \
             whether the result came from history or from the interrupted-synthesis path: \
             shape = {:?}",
            messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
        );
        assert_eq!(
            messages[tool_use_idx + 1].tool_call_id.as_deref(),
            Some("tc-asym"),
            "the paired tool_result must carry the tool_use's tool_call_id",
        );

        // Trailing user invariant.
        assert_eq!(messages.last().unwrap().role, "user");
    }
}
