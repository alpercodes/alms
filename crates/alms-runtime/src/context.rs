//! Context window builder for LLM requests.
//!
//! Manages what the LLM actually sees — assembles system prompt,
//! history (possibly compressed), and current input within a token budget.

use crate::llm_types::LlmMessage;
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
    /// [system_prompt, (summary if needed), recent_messages, current_input]
    pub fn build(
        &self,
        system_prompt: &str,
        history: &[Message],
        current_input: &str,
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
                // Fall back to truncate until summary generation is implemented
                self.build_truncate(history, history_budget, &mut messages);
            }
            _ => {
                warn!("Unknown context strategy '{}', using truncate", self.config.strategy);
                self.build_truncate(history, history_budget, &mut messages);
            }
        }

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
    fn build_full(
        &self,
        history: &[Message],
        budget: usize,
        messages: &mut Vec<LlmMessage>,
    ) {
        let mut used = 0;
        for msg in history {
            let llm_msg = self.session_msg_to_llm(msg);
            let tokens = estimate_tokens(llm_msg.content_str());
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
    fn build_truncate(
        &self,
        history: &[Message],
        budget: usize,
        messages: &mut Vec<LlmMessage>,
    ) {
        let mut selected: Vec<LlmMessage> = Vec::new();
        let mut used = 0;
        let max_messages = self.config.recent_window;

        // Walk backwards through history
        for msg in history.iter().rev() {
            if selected.len() >= max_messages {
                break;
            }
            let llm_msg = self.session_msg_to_llm(msg);
            let tokens = estimate_tokens(llm_msg.content_str());
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

    /// Convert a session Message to an LlmMessage
    fn session_msg_to_llm(&self, msg: &Message) -> LlmMessage {
        match msg.role {
            Role::System => LlmMessage::system(content_to_string(&msg.content)),
            Role::User => LlmMessage::user(content_to_string(&msg.content)),
            Role::Assistant => LlmMessage::assistant(content_to_string(&msg.content)),
            Role::Tool => LlmMessage::tool_result(
                msg.id.clone(),
                content_to_string(&msg.content),
            ),
        }
    }

    fn estimate_total_tokens(&self, messages: &[LlmMessage]) -> usize {
        messages
            .iter()
            .map(|m| estimate_tokens(m.content_str()) + 4) // 4 tokens overhead per message
            .sum()
    }
}

/// Rough token estimate: ~4 characters per token for English text.
/// This is intentionally simple. A proper tokenizer (tiktoken) can be added later
/// without changing the interface.
pub fn estimate_tokens(text: &str) -> usize {
    // chars/4 is a reasonable approximation for GPT-style tokenizers
    (text.len() + 3) / 4
}

/// Convert session Content to a string for LLM context
fn content_to_string(content: &Content) -> String {
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
        assert_eq!(estimate_tokens("hello"), 2); // (5+3)/4 = 2
        assert_eq!(estimate_tokens("hello world"), 3); // (11+3)/4 = 3
    }

    #[test]
    fn test_build_simple() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];

        let messages = builder.build("You are helpful.", &history, "What's up?");

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

        let messages = builder.build("System", &history, "Final question");

        // system + 3 recent + current = 5
        assert_eq!(messages.len(), 5);
        // The 3 recent messages should be the last 3 from history
        assert_eq!(messages[1].content_str(), "Answer 7");
        assert_eq!(messages[2].content_str(), "Question 8");
        assert_eq!(messages[3].content_str(), "Answer 9");
    }

    #[test]
    fn test_truncate_respects_token_budget() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 100, // very small budget
            recent_window: 100,     // allow many messages
            summary_interval: 30,
        };
        let builder = ContextBuilder::new(config);

        // Create messages with substantial text
        let history: Vec<Message> = (0..20)
            .map(|i| make_msg(Role::User, &format!("This is a reasonably long message number {} with some content", i)))
            .collect();

        let messages = builder.build("System prompt", &history, "Input");

        // Should have fewer than 20 history messages due to token budget
        assert!(messages.len() < 22); // system + some history + input
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
}
