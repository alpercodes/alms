//! Context window builder for LLM requests.
//!
//! Manages what the LLM actually sees — assembles system prompt,
//! history (possibly compressed), and current input within a token budget.

use crate::llm_types::{FunctionCall, LlmMessage, ToolCall};
use alms_core::config::ContextConfig;
use alms_session::{Content, Message, Role};
use tracing::{debug, warn};

/// Builds the context window (Vec<LlmMessage>) for an LLM request.
pub struct ContextBuilder {
    config: ContextConfig,
}

impl ContextBuilder {
    pub fn new(config: ContextConfig) -> Self {
        Self { config }
    }

    /// Build the message list for an LLM call.
    ///
    /// Takes the full session history and produces a token-budgeted context window:
    /// `[system_prompt, (summary if needed), recent_messages, current_input]`
    ///
    /// `existing_summary` is only used when `strategy == "sliding-summary"`.
    /// Pass `None` for all other strategies.
    pub fn build(
        &self,
        system_prompt: &str,
        history: &[Message],
        current_input: &str,
        existing_summary: Option<&str>,
    ) -> Vec<LlmMessage> {
        let mut messages = Vec::new();

        // 1. System prompt (always included)
        let system_tokens = estimate_tokens(system_prompt);
        messages.push(LlmMessage::system(system_prompt));

        // 2. Current input (always included)
        let input_tokens = estimate_tokens(current_input);

        // 3. Budget for history
        let reserved = system_tokens + input_tokens + 500; // 500 token buffer for response
        let history_budget = self.config.max_input_tokens.saturating_sub(reserved);

        match self.config.strategy.as_str() {
            "full" => {
                self.build_full(history, history_budget, &mut messages);
            }
            "truncate" => {
                self.build_truncate(history, history_budget, &mut messages);
            }
            "sliding-summary" => {
                self.build_sliding_summary(
                    history,
                    history_budget,
                    &mut messages,
                    existing_summary,
                );
            }
            _ => {
                warn!(
                    "Unknown context strategy '{}', using truncate",
                    self.config.strategy
                );
                self.build_truncate(history, history_budget, &mut messages);
            }
        }

        // 3.5. Group consecutive assistant tool-call messages into single messages
        // with multiple tool_calls entries (required by OpenAI/Anthropic APIs).
        Self::group_tool_calls(&mut messages);

        // 4. Current input
        messages.push(LlmMessage::user(current_input));

        debug!(
            "Context built: {} messages, ~{} tokens (budget: {})",
            messages.len(),
            self.estimate_total_tokens(&messages),
            self.config.max_input_tokens
        );

        messages
    }

    /// Full strategy: include all history (oldest to newest), skip if over budget
    fn build_full(&self, history: &[Message], budget: usize, messages: &mut Vec<LlmMessage>) {
        let mut used = 0;
        for msg in history {
            let llm_msg = self.session_msg_to_llm(msg);
            let tokens = estimate_llm_message_tokens(&llm_msg);
            if used + tokens > budget {
                warn!(
                    "Full context strategy exceeded budget at message {}/{}, truncating",
                    messages.len(),
                    history.len()
                );
                break;
            }
            used += tokens;
            messages.push(llm_msg);
        }
    }

    /// Truncate strategy: keep the most recent messages within budget.
    /// Walks backwards from the newest message.
    fn build_truncate(&self, history: &[Message], budget: usize, messages: &mut Vec<LlmMessage>) {
        let mut selected: Vec<LlmMessage> = Vec::new();
        let mut used = 0;
        let max_messages = self.config.recent_window;

        // Walk backwards through history
        for msg in history.iter().rev() {
            if selected.len() >= max_messages {
                break;
            }
            let llm_msg = self.session_msg_to_llm(msg);
            let tokens = estimate_llm_message_tokens(&llm_msg);
            if used + tokens > budget {
                break;
            }
            used += tokens;
            selected.push(llm_msg);
        }

        // Reverse to get chronological order
        selected.reverse();
        messages.extend(selected);
    }

    /// Sliding-summary strategy: inject the pre-computed rolling summary then fill
    /// with the most-recent messages that fit within the remaining budget.
    fn build_sliding_summary(
        &self,
        history: &[Message],
        budget: usize,
        messages: &mut Vec<LlmMessage>,
        summary: Option<&str>,
    ) {
        let mut used = 0;

        // 1. Inject summary block if present
        if let Some(text) = summary.filter(|s| !s.is_empty()) {
            let tokens = estimate_tokens(text) + 4;
            if tokens < budget {
                messages.push(LlmMessage::system(format!(
                    "[Context summary of earlier conversation]\n{}",
                    text
                )));
                used += tokens;
            }
        }

        // 2. Fill remaining budget with most-recent messages (newest-first walk, then reverse)
        let remaining = budget.saturating_sub(used);
        let mut selected: Vec<LlmMessage> = Vec::new();
        let mut msg_used = 0;
        let max_messages = self.config.recent_window;

        for msg in history.iter().rev() {
            if selected.len() >= max_messages {
                break;
            }
            let llm_msg = self.session_msg_to_llm(msg);
            let tokens = estimate_llm_message_tokens(&llm_msg) + 4;
            if msg_used + tokens > remaining {
                break;
            }
            msg_used += tokens;
            selected.push(llm_msg);
        }

        selected.reverse();
        messages.extend(selected);
    }

    /// Convert a session Message to an LlmMessage.
    /// Reconstructs structured tool call/result messages from persisted format
    /// so the LLM has full visibility of previous tool executions across runs.
    fn session_msg_to_llm(&self, msg: &Message) -> LlmMessage {
        match (&msg.role, &msg.content) {
            // Reconstruct structured assistant message with tool_calls
            (Role::Assistant, Content::ToolCall { name, params }) => {
                let tool_call_id = msg
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("tool_call_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                LlmMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: tool_call_id,
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: params.to_string(),
                        },
                    }]),
                    tool_call_id: None,
                }
            }
            // Reconstruct tool result with correct tool_call_id
            (Role::Tool, Content::ToolResult { tool_id, result }) => {
                let result_str = result.to_string();
                // Truncate long tool outputs in context
                let content = if result_str.len() > 2000 {
                    format!(
                        "{}... [truncated, {} bytes total]",
                        &result_str[..2000],
                        result_str.len()
                    )
                } else {
                    result_str
                };
                LlmMessage::tool_result(tool_id.clone(), content)
            }
            (Role::System, _) => LlmMessage::system(content_to_string(&msg.content)),
            (Role::User, _) => LlmMessage::user(content_to_string(&msg.content)),
            (Role::Assistant, _) => LlmMessage::assistant(content_to_string(&msg.content)),
            (Role::Tool, _) => {
                LlmMessage::tool_result(msg.id.clone(), content_to_string(&msg.content))
            }
        }
    }

    /// Merge consecutive assistant messages that carry tool_calls (and no text
    /// content) into a single message with all tool_calls combined.
    ///
    /// Persisted tool calls are stored as individual messages, but LLM APIs
    /// expect a single assistant message with an array of all parallel tool
    /// calls. This post-processing step restores that grouping.
    fn group_tool_calls(messages: &mut Vec<LlmMessage>) {
        let mut grouped: Vec<LlmMessage> = Vec::with_capacity(messages.len());

        for msg in messages.drain(..) {
            let is_tool_call_msg =
                msg.role == "assistant" && msg.tool_calls.is_some() && msg.content.is_none();

            if is_tool_call_msg {
                // Try to merge with the previous message if it's also an
                // assistant tool-call-only message.
                if let Some(prev) = grouped.last_mut() {
                    let prev_is_tool_call = prev.role == "assistant"
                        && prev.tool_calls.is_some()
                        && prev.content.is_none();

                    if prev_is_tool_call {
                        // Merge: append this message's tool_calls to the previous
                        if let Some(new_calls) = msg.tool_calls {
                            prev.tool_calls.as_mut().unwrap().extend(new_calls);
                        }
                        continue;
                    }
                }
            }

            grouped.push(msg);
        }

        *messages = grouped;
    }

    fn estimate_total_tokens(&self, messages: &[LlmMessage]) -> usize {
        messages
            .iter()
            .map(|m| estimate_llm_message_tokens(m) + 4) // 4 tokens overhead per message
            .sum()
    }
}

/// Rough token estimate for mixed content (natural language, JSON, code).
/// ~3 chars/token is a safer approximation than 4 for JSON-heavy tool output.
/// A proper tokenizer (tiktoken) can be added later without changing the interface.
pub fn estimate_tokens(text: &str) -> usize {
    // ~3 chars per token: slightly overestimates for pure English (~4 chars/token)
    // but more accurate for JSON/code (~2-3 chars/token). Overestimating is safer
    // than underestimating — better to leave headroom than overshoot the context window.
    text.len().div_ceil(3)
}

/// Estimate tokens for a full LlmMessage, including tool_calls if present.
/// Plain text messages use `content_str()`. Tool call messages (content: None,
/// tool_calls: Some) estimate from the serialized tool call JSON instead.
fn estimate_llm_message_tokens(msg: &LlmMessage) -> usize {
    let content_tokens = estimate_tokens(msg.content_str());
    let tool_call_tokens = msg
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|tc| {
                    // Account for id, function name, and arguments JSON
                    estimate_tokens(&tc.id)
                        + estimate_tokens(&tc.function.name)
                        + estimate_tokens(&tc.function.arguments)
                        + 10 // overhead for JSON structure: {"id":...,"function":{...}}
                })
                .sum::<usize>()
        })
        .unwrap_or(0);
    content_tokens + tool_call_tokens
}

/// Convert session Content to a plain-text string.
///
/// Used by `read_subagent_session_tool` for formatting subagent messages and as a
/// fallback in `session_msg_to_llm` for unexpected role/content combinations (e.g.
/// `Content::Image`). The `ToolCall`/`ToolResult` branches here are NOT used by
/// `session_msg_to_llm` — those are handled by dedicated match arms that produce
/// structured LLM messages instead.
pub(crate) fn content_to_string(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::ToolCall { name, params } => {
            format!("Tool call: {}({})", name, params)
        }
        Content::ToolResult { tool_id, result } => {
            let result_str = result.to_string();
            // Truncate long tool outputs in context
            if result_str.len() > 2000 {
                format!(
                    "Tool result {}: {}... [truncated, {} bytes total]",
                    tool_id,
                    &result_str[..2000],
                    result_str.len()
                )
            } else {
                format!("Tool result {}: {}", tool_id, result_str)
            }
        }
        Content::Image { url, .. } => {
            format!("[Image: {}]", url)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::Timestamp;

    fn make_msg(role: Role, text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: None,
        }
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello"), 2); // ceil(5/3) = 2
        assert_eq!(estimate_tokens("hello world"), 4); // ceil(11/3) = 4
    }

    #[test]
    fn test_estimate_llm_message_tokens_text_only() {
        let msg = LlmMessage::user("hello world");
        assert_eq!(
            estimate_llm_message_tokens(&msg),
            estimate_tokens("hello world")
        );
    }

    #[test]
    fn test_estimate_llm_message_tokens_tool_call() {
        let msg = LlmMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_abc123".to_string(),
                function: FunctionCall {
                    name: "shell_exec".to_string(),
                    arguments: r#"{"argv":["ls","-la"]}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        };
        let tokens = estimate_llm_message_tokens(&msg);
        // Should be > 0 even though content is None
        assert!(
            tokens > 0,
            "tool call messages must have non-zero token estimate"
        );
        // Sanity: id + name + arguments + overhead should be reasonable
        assert!(
            tokens > 10,
            "expected at least ~10 tokens for a tool call, got {tokens}"
        );
    }

    #[test]
    fn test_build_simple() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];

        let messages = builder.build("You are helpful.", &history, "What's up?", None);

        // system + 2 history + current input = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "user");
    }

    #[test]
    fn test_truncate_respects_window() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 3,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        // Create 10 messages
        let history: Vec<Message> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    make_msg(Role::User, &format!("Question {}", i))
                } else {
                    make_msg(Role::Assistant, &format!("Answer {}", i))
                }
            })
            .collect();

        let messages = builder.build("System", &history, "Final question", None);

        // system + 3 recent + current = 5
        assert_eq!(messages.len(), 5);
        // The 3 recent messages should be the last 3 from history
        assert_eq!(messages[1].content_str(), "Answer 7");
        assert_eq!(messages[2].content_str(), "Question 8");
        assert_eq!(messages[3].content_str(), "Answer 9");
    }

    #[test]
    fn test_truncate_respects_token_budget() {
        // Budget 600 tokens with a 500-token response buffer leaves ~100 for history.
        // Each message is ~20 tokens at chars/3, so only a few should fit.
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 600,
            recent_window: 100, // allow many messages
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        // Create messages with substantial text (~57 chars each → ~19 tokens at chars/3)
        let history: Vec<Message> = (0..20)
            .map(|i| {
                make_msg(
                    Role::User,
                    &format!(
                        "This is a reasonably long message number {} with some content",
                        i
                    ),
                )
            })
            .collect();

        let messages = builder.build("System prompt", &history, "Input", None);

        // system + input = 2 fixed messages; history budget ~93 tokens fits ~4-5 messages
        assert!(
            messages.len() >= 3,
            "should include at least one history message"
        );
        assert!(
            messages.len() <= 8,
            "token budget should limit history to a few messages"
        );
    }

    #[test]
    fn test_sliding_summary_no_prior_summary() {
        let config = ContextConfig {
            strategy: "sliding-summary".into(),
            max_input_tokens: 32000,
            recent_window: 3,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        let history: Vec<Message> = (0..6)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();

        // Without a summary it should behave like truncate (keep last 3)
        let messages = builder.build("System", &history, "current", None);
        assert_eq!(messages[0].role, "system");
        // 3 history + current
        let body_count = messages.len() - 2; // subtract system + current
        assert_eq!(body_count, 3);
    }

    #[test]
    fn test_sliding_summary_injects_summary_block() {
        let config = ContextConfig {
            strategy: "sliding-summary".into(),
            max_input_tokens: 32000,
            recent_window: 3,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        let history: Vec<Message> = (0..6)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();

        let messages = builder.build(
            "System",
            &history,
            "current",
            Some("Earlier the user greeted."),
        );

        // system + summary_block + 3 recent + current = 6
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system"); // summary injected as second system message
        assert!(messages[1].content_str().contains("[Context summary"));
        assert!(
            messages[1]
                .content_str()
                .contains("Earlier the user greeted.")
        );
    }

    #[test]
    fn test_tool_result_truncation() {
        let long_result = "x".repeat(5000);
        let content = Content::ToolResult {
            tool_id: "test".into(),
            result: serde_json::Value::String(long_result),
        };
        let result = content_to_string(&content);
        assert!(result.contains("[truncated"));
        assert!(result.len() < 3000);
    }

    #[test]
    fn test_tool_call_reconstructed_in_context() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "run ls"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"argv": ["ls"]}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_123"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_123".to_string(),
                    result: serde_json::json!("file1.txt\nfile2.txt"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "Here are the files."),
        ];

        let messages = builder.build("System", &history, "now what?", None);

        // system + 4 history + current = 6
        assert_eq!(messages.len(), 6);

        // Tool call message: assistant with tool_calls array
        let tc_msg = &messages[2];
        assert_eq!(tc_msg.role, "assistant");
        assert!(tc_msg.content.is_none());
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_123");
        assert_eq!(calls[0].function.name, "shell_exec");

        // Tool result message: role=tool with tool_call_id
        let tr_msg = &messages[3];
        assert_eq!(tr_msg.role, "tool");
        assert_eq!(tr_msg.tool_call_id.as_deref(), Some("call_123"));
        assert!(tr_msg.content_str().contains("file1.txt"));
    }

    #[test]
    fn test_parallel_tool_calls_grouped() {
        let config = ContextConfig {
            strategy: "full".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        // Simulate 3 parallel tool calls persisted as separate messages,
        // followed by 3 tool results — the typical pattern for join_all.
        let history = vec![
            make_msg(Role::User, "list files and check disk"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"argv": ["ls"]}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_A"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"argv": ["df", "-h"]}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_B"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "echo".to_string(),
                    params: serde_json::json!({"text": "done"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_C"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_A".to_string(),
                    result: serde_json::json!("file1.txt"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_B".to_string(),
                    result: serde_json::json!("/dev/sda1 50G"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_C".to_string(),
                    result: serde_json::json!("done"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "Here's what I found."),
        ];

        let messages = builder.build("System", &history, "next?", None);

        // system + 1 user + 1 grouped assistant + 3 tool results + 1 assistant text + current = 8
        // (3 tool calls collapsed into 1)
        assert_eq!(messages.len(), 8);

        // The grouped assistant message should have 3 tool_calls
        let tc_msg = &messages[2];
        assert_eq!(tc_msg.role, "assistant");
        assert!(tc_msg.content.is_none());
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].id, "call_A");
        assert_eq!(calls[1].id, "call_B");
        assert_eq!(calls[2].id, "call_C");
        assert_eq!(calls[0].function.name, "shell_exec");
        assert_eq!(calls[2].function.name, "echo");

        // Tool results should follow individually
        assert_eq!(messages[3].role, "tool");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call_A"));
        assert_eq!(messages[4].role, "tool");
        assert_eq!(messages[4].tool_call_id.as_deref(), Some("call_B"));
        assert_eq!(messages[5].role, "tool");
        assert_eq!(messages[5].tool_call_id.as_deref(), Some("call_C"));

        // Final assistant text
        assert_eq!(messages[6].role, "assistant");
        assert_eq!(messages[6].content_str(), "Here's what I found.");
    }

    #[test]
    fn test_group_tool_calls_does_not_merge_across_text() {
        // If there's an assistant text message between two tool call groups,
        // they should NOT be merged.
        let mut messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    function: FunctionCall {
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            LlmMessage::assistant("some text in between"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_2".to_string(),
                    function: FunctionCall {
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                tool_call_id: None,
            },
        ];

        ContextBuilder::group_tool_calls(&mut messages);

        // Should remain 3 separate messages — not merged
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0].tool_calls.as_ref().unwrap().len(),
            1,
            "first tool call should not absorb second"
        );
        assert_eq!(
            messages[2].tool_calls.as_ref().unwrap().len(),
            1,
            "second tool call should stay separate"
        );
    }
}
