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
            let tokens = estimate_tokens(llm_msg.content_str()) + 4;
            if msg_used + tokens > remaining {
                break;
            }
            msg_used += tokens;
            selected.push(llm_msg);
        }

        selected.reverse();
        messages.extend(selected);
    }

    /// Convert a session Message to an LlmMessage
    fn session_msg_to_llm(&self, msg: &Message) -> LlmMessage {
        match msg.role {
            Role::System => LlmMessage::system(content_to_string(&msg.content)),
            Role::User => LlmMessage::user(content_to_string(&msg.content)),
            Role::Assistant => LlmMessage::assistant(content_to_string(&msg.content)),
            Role::Tool => LlmMessage::tool_result(msg.id.clone(), content_to_string(&msg.content)),
        }
    }

    fn estimate_total_tokens(&self, messages: &[LlmMessage]) -> usize {
        messages
            .iter()
            .map(|m| estimate_tokens(m.content_str()) + 4) // 4 tokens overhead per message
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

/// Convert session Content to a string for LLM context
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
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 100, // very small budget
            recent_window: 100,    // allow many messages
            summary_interval: 30,
            summary_model: None,
        };
        let builder = ContextBuilder::new(config);

        // Create messages with substantial text
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

        // Should have fewer than 20 history messages due to token budget
        assert!(messages.len() < 22); // system + some history + input
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
}
