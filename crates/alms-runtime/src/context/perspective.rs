//! DM-perspective role mapping and reasoning-message filter.
//!
//! For shared sessions (DM/group), all messages are stored as `Role::User`
//! and the actual role depends on who is reading the session.
//! [`apply_perspective`] flips messages whose `from_agent` metadata matches
//! the reading agent into `Role::Assistant` so the LLM sees its own
//! previous responses correctly.
//!
//! [`is_reasoning_message`] gates the reasoning-message filter that runs
//! immediately before perspective mapping in [`super::ContextBuilder`]:
//! reasoning messages (thinking text / tool calls / tool results persisted
//! as `Role::User` with `message_type: "reasoning"` to preserve the DM
//! invariant) must be filtered from the LLM context to avoid token waste
//! and malformed messages (a perspective-mapped `ToolResult` stored as
//! `Role::User` cannot be correctly handled by `rebuild::session_msg_to_llm`
//! — fixes C2 in the #930 review).
//!
//! Pure free functions — no `&self`, no shared state.

use alms_session::{Message, Role};

/// Returns `true` if the message is an internal reasoning message
/// (persisted during DM agent loops with `message_type: "reasoning"`).
///
/// These messages contain the agent's thinking text, tool calls, and tool
/// results stored as `Role::User` to preserve the DM invariant. They
/// should be filtered from LLM context to avoid token waste and malformed
/// messages (tool results stored as `Role::User` cannot be correctly
/// mapped by `rebuild::session_msg_to_llm`).
pub(super) fn is_reasoning_message(msg: &Message) -> bool {
    msg.metadata
        .as_ref()
        .and_then(|m| m.get("message_type"))
        .and_then(|v| v.as_str())
        == Some("reasoning")
}

/// Apply perspective mapping to a message.
///
/// For shared sessions (DM/group), all messages are stored as `Role::User`.
/// When building context for a specific agent, messages from that agent
/// should be mapped to `Role::Assistant` so the LLM sees them as its own
/// previous responses.
pub(super) fn apply_perspective(msg: &Message, perspective_agent: &str) -> Message {
    let from_agent = msg
        .metadata
        .as_ref()
        .and_then(|m| m.get("from_agent"))
        .and_then(|v| v.as_str());

    match from_agent {
        Some(sender) if sender == perspective_agent => {
            // This agent's own message -> map to Assistant
            let mut mapped = msg.clone();
            mapped.role = Role::Assistant;
            mapped
        }
        _ => {
            // Someone else's message (or no metadata) -> keep original role
            msg.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::{
        default_builder, make_msg, make_msg_with_meta, make_msg_with_metadata,
    };
    use alms_core::config::ContextConfig;
    use alms_session::{Content, Role};

    #[test]
    fn test_apply_perspective_no_metadata_stays_user() {
        let builder = default_builder();
        let msg = make_msg(Role::User, "hello from nowhere");
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();
        let msg = make_msg_with_metadata(
            Role::User,
            "I said this",
            Some(serde_json::json!({"from_agent": "agent-alpha"})),
        );
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::Assistant,
            "own message should map to Assistant"
        );
    }

    #[test]
    fn test_apply_perspective_different_agent_stays_user() {
        let builder = default_builder();
        let msg = make_msg_with_metadata(
            Role::User,
            "someone else said this",
            Some(serde_json::json!({"from_agent": "agent-beta"})),
        );
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
        assert_eq!(
            mapped.role,
            Role::User,
            "other agent's message should stay User"
        );
    }

    #[test]
    fn test_apply_perspective_metadata_without_from_agent_stays_user() {
        let builder = default_builder();
        let msg = make_msg_with_metadata(
            Role::User,
            "some random metadata",
            Some(serde_json::json!({"other_key": "other_value"})),
        );
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();

        // System message without metadata -> stays System
        let msg = make_msg_with_metadata(Role::System, "system prompt text", None);
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();

        // Tool message without metadata -> stays Tool
        let msg = make_msg_with_metadata(Role::Tool, "tool output", None);
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();

        let metadata = serde_json::json!({"from_agent": "agent-beta"});
        let msg =
            make_msg_with_metadata(Role::Assistant, "I replied earlier", Some(metadata.clone()));
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();

        let metadata = serde_json::json!({"from_agent": "agent-alpha"});
        let msg =
            make_msg_with_metadata(Role::Assistant, "I already replied", Some(metadata.clone()));
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let builder = default_builder();

        // System message with matching from_agent -> mapped to Assistant
        let metadata = serde_json::json!({"from_agent": "agent-alpha"});
        let msg = make_msg_with_metadata(Role::System, "system from self", Some(metadata.clone()));
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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
        let mapped = builder.apply_perspective(&msg, "agent-alpha");
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

    /// Reasoning messages (message_type="reasoning") persisted in DM sessions
    /// should be filtered from the LLM context when perspective mapping is
    /// active. This prevents malformed messages (C2) and token waste (S4).
    #[test]
    fn test_reasoning_messages_filtered_from_dm_context() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            // Alice sends a normal DM message
            make_msg_with_meta(
                Role::User,
                Content::Text("Hello Bob!".to_string()),
                serde_json::json!({ "from_agent": "alice", "message_type": "dm" }),
            ),
            // Bob's reasoning text (persisted as Role::User with message_type="reasoning")
            make_msg_with_meta(
                Role::User,
                Content::Text("Let me think about this...".to_string()),
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "run_id": "run-123",
                }),
            ),
            // Bob's reasoning tool call (persisted as Role::User with message_type="reasoning")
            make_msg_with_meta(
                Role::User,
                Content::ToolCall {
                    name: "send_message".to_string(),
                    params: serde_json::json!({ "to": "alice", "text": "Hi!" }),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "tool_call_id": "tc-1",
                }),
            ),
            // Bob's reasoning tool result (persisted as Role::User with message_type="reasoning")
            make_msg_with_meta(
                Role::User,
                Content::ToolResult {
                    tool_id: "tc-1".to_string(),
                    result: serde_json::json!("Message sent"),
                },
                serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "reasoning",
                    "ok": true,
                }),
            ),
            // Bob's actual DM reply (from send_message — this is the real message)
            make_msg_with_meta(
                Role::User,
                Content::Text("Hi Alice!".to_string()),
                serde_json::json!({ "from_agent": "bob", "message_type": "dm" }),
            ),
        ];

        // Build context with perspective mapping (as Bob would see it)
        let messages = builder.build_with_perspective(
            "System prompt.",
            &history,
            "What's up?",
            None,
            Some("bob"),
            None,
        );

        // system + alice's msg (user) + bob's reply (assistant) + input = 4
        // The 3 reasoning messages should be filtered out.
        assert_eq!(
            messages.len(),
            4,
            "Expected 4 messages (system + 2 DM + input), got {}. Reasoning should be filtered.",
            messages.len()
        );
        assert_eq!(messages[0].role, "system");
        // Alice's message stays user (from Bob's perspective)
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content_str(), "Hello Bob!");
        // Bob's DM reply becomes assistant (from Bob's perspective)
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content_str(), "Hi Alice!");
        // Current input
        assert_eq!(messages[3].role, "user");
        assert_eq!(messages[3].content_str(), "What's up?");
    }

    /// Without perspective mapping (non-DM sessions), reasoning messages
    /// are not filtered — they should not appear in non-DM sessions in
    /// practice, but if they do, they pass through unchanged.
    #[test]
    fn test_reasoning_messages_not_filtered_without_perspective() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
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
}
