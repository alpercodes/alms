//! Context window builder for LLM requests.
//!
//! Manages what the LLM actually sees — assembles system prompt,
//! history (possibly compressed), and current input within a token budget.

use crate::llm_types::{LlmMessage, ToolCall};
use alms_core::config::ContextConfig;
use alms_core::truncate_to_char_boundary;
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
    /// `[system_prompt, (episodic?), (summary if needed), recent_messages, current_input]`
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
        self.build_with_perspective(
            system_prompt,
            history,
            current_input,
            existing_summary,
            None,
            None,
        )
    }

    /// Build the message list with optional perspective mapping and episodic
    /// context injection.
    ///
    /// When `perspective_agent` is `Some("agent-name")`, messages in the session
    /// are role-mapped based on `from_agent` metadata:
    /// - `from_agent == perspective_agent` -> `"assistant"` (the LLM's own previous responses)
    /// - `from_agent != perspective_agent` -> `"user"` (input from others)
    ///
    /// This is used for shared DM/group sessions where all messages are stored
    /// as `Role::User` and the actual role depends on who is reading.
    ///
    /// When `episodic_summaries` is `Some(text)`, the text is injected as a
    /// system message between the main system prompt and the session history.
    /// Its token cost comes from the caller's pre-computed budget (via
    /// `run_summary_budget`) and is subtracted from the available history
    /// budget so episodic content never starves the current conversation.
    pub fn build_with_perspective(
        &self,
        system_prompt: &str,
        history: &[Message],
        current_input: &str,
        existing_summary: Option<&str>,
        perspective_agent: Option<&str>,
        episodic_summaries: Option<&str>,
    ) -> Vec<LlmMessage> {
        let mut messages = Vec::new();

        // 1. System prompt (always included)
        let system_tokens = estimate_tokens(system_prompt);
        messages.push(LlmMessage::system(system_prompt));

        // 2. Episodic summaries from other sessions (injected between system
        // prompt and session history so current session gets LLM recency bias).
        let episodic_tokens = match episodic_summaries.filter(|s| !s.is_empty()) {
            Some(text) => {
                let tokens = estimate_tokens(text) + 4; // +4 for message overhead
                messages.push(LlmMessage::system(text));
                debug!(episodic_tokens = tokens, "Injected episodic summaries");
                tokens
            }
            None => 0,
        };

        // 3. Current input (always included)
        let input_tokens = estimate_tokens(current_input);

        // 4. Budget for history — episodic tokens are subtracted from the
        // available space so they do not eat into the history budget.
        //
        // I6: Buffer increased from 500 to 1000 tokens.  The estimate_tokens
        // heuristic (len/3) overestimates English but underestimates code and
        // JSON (~2-3 chars/token).  A larger buffer provides a stronger safety
        // margin against LLM API rejection for max_input_tokens breaches.
        let reserved = system_tokens + input_tokens + episodic_tokens + 1000;
        let history_budget = self.config.max_input_tokens.saturating_sub(reserved);

        // Filter out reasoning messages (message_type="reasoning") from
        // DM sessions before building context.  These are internal agent
        // reasoning (thinking text, tool calls, tool results) persisted as
        // Role::User to preserve the DM invariant.  They should not appear
        // in either agent's LLM context:
        //   - The reasoning agent already has them from the current run
        //   - The peer agent should never see them (token waste + malformed
        //     messages when perspective-mapped ToolResult hits the catch-all
        //     in session_msg_to_llm — fixes C2)
        let filtered_history: Vec<Message>;
        let effective_history =
            if perspective_agent.is_some() && history.iter().any(Self::is_reasoning_message) {
                let before = history.len();
                filtered_history = history
                    .iter()
                    .filter(|m| !Self::is_reasoning_message(m))
                    .cloned()
                    .collect();
                debug!(
                    filtered = before - filtered_history.len(),
                    "Filtered reasoning messages from DM context"
                );
                filtered_history.as_slice()
            } else {
                history
            };

        // Pre-map the history if perspective is set, then pass the mapped
        // messages through the standard strategies.
        let mapped_history: Vec<Message>;
        let history_ref = if let Some(agent) = perspective_agent {
            mapped_history = effective_history
                .iter()
                .map(|msg| self.apply_perspective(msg, agent))
                .collect();
            &mapped_history
        } else {
            effective_history
        };

        match self.config.strategy.as_str() {
            "full" => {
                self.build_full(history_ref, history_budget, &mut messages);
            }
            "truncate" => {
                self.build_truncate(history_ref, history_budget, &mut messages);
            }
            "sliding-summary" => {
                self.build_sliding_summary(
                    history_ref,
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
                self.build_truncate(history_ref, history_budget, &mut messages);
            }
        }

        // 5. Group consecutive assistant tool-call messages into single messages
        // with multiple tool_calls entries (required by OpenAI/Anthropic APIs).
        Self::group_tool_calls(&mut messages);

        // 5a. Strip orphaned tool_result messages.
        //
        // Truncation (and other selection strategies) can slice the history
        // between an assistant tool_use and its matching tool_result, leaving
        // a tool_result in the selected window whose paired assistant
        // tool_call has been dropped.  When such an orphan ends up at the
        // head of the message array, Anthropic rejects the request with
        // 400 "unexpected `tool_use_id` found in `tool_result` blocks" — and
        // OpenRouter-to-Claude proxying inherits the same failure mode.
        //
        // Sweep through the array and remove any tool_result whose
        // `tool_call_id` does not appear in a preceding assistant message's
        // `tool_calls`.  This is a minimal, targeted fix for the specific
        // 400; #586 tracks the full invariant-enforcement design.
        Self::strip_orphaned_tool_results(&mut messages);

        // 6. Current input (skip if empty — avoids sending a blank user message to the LLM)
        if !current_input.is_empty() {
            messages.push(LlmMessage::user(current_input));
        }

        debug!(
            "Context built: {} messages, ~{} tokens (budget: {})",
            messages.len(),
            self.estimate_total_tokens(&messages),
            self.config.max_input_tokens
        );

        messages
    }

    /// Returns `true` if the message is an internal reasoning message
    /// (persisted during DM agent loops with `message_type: "reasoning"`).
    ///
    /// These messages contain the agent's thinking text, tool calls, and tool
    /// results stored as `Role::User` to preserve the DM invariant. They
    /// should be filtered from LLM context to avoid token waste and malformed
    /// messages (tool results stored as `Role::User` cannot be correctly
    /// mapped by `session_msg_to_llm`).
    fn is_reasoning_message(msg: &Message) -> bool {
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
    fn apply_perspective(&self, msg: &Message, perspective_agent: &str) -> Message {
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
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall::new(
                        tool_call_id,
                        name.clone(),
                        params.to_string(),
                    )]),
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
                        truncate_to_char_boundary(&result_str, 2000),
                        result_str.len()
                    )
                } else {
                    result_str
                };
                LlmMessage::tool_result(tool_id.clone(), content)
            }
            (Role::System, _) => LlmMessage::system(msg.content.to_display_string()),
            (Role::User, _) => LlmMessage::user(msg.content.to_display_string()),
            (Role::Assistant, _) => LlmMessage::assistant(msg.content.to_display_string()),
            (Role::Tool, _) => {
                LlmMessage::tool_result(msg.id.clone(), msg.content.to_display_string())
            }
        }
    }

    /// Remove tool-result messages whose `tool_call_id` is not introduced by
    /// a preceding assistant message's `tool_calls`.
    ///
    /// Walks the message list once, building a running set of tool-call IDs
    /// emitted by assistant messages.  Any `role="tool"` message whose
    /// `tool_call_id` is absent from that set is an orphan — a tool result
    /// whose matching tool_use was truncated out of the context window — and
    /// is dropped.
    ///
    /// This is a targeted fix for the Anthropic 400:
    /// `unexpected tool_use_id found in tool_result blocks: <id>. Each
    /// tool_result block must have a corresponding tool_use block in the
    /// previous message.`  The failure surfaces when a `truncate`/`full`
    /// cut leaves a tool_result at the head of the selected history (post-
    /// system-extraction, that becomes `messages.0.content.0`).  OpenRouter-
    /// to-Claude proxying hits the same rejection.
    ///
    /// Scope is intentionally narrow — the broader invariant-enforcement
    /// work (#586) covers additional failure modes (non-empty array, leading
    /// system prefix, trailing user turn, alternating roles after merge,
    /// pending-tool-call tails) in a dedicated follow-up.
    fn strip_orphaned_tool_results(messages: &mut Vec<LlmMessage>) {
        use std::collections::HashSet;

        let mut known_ids: HashSet<String> = HashSet::new();
        let mut dropped = 0usize;

        // Single forward pass so a tool_result is only considered paired
        // with a tool_use that PRECEDES it in the final array.
        messages.retain(|msg| {
            if msg.role == "assistant"
                && let Some(ref calls) = msg.tool_calls
            {
                for call in calls {
                    known_ids.insert(call.id.clone());
                }
            }
            if msg.role == "tool" {
                let paired = msg
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|id| known_ids.contains(id));
                if !paired {
                    dropped += 1;
                    return false;
                }
            }
            true
        });

        if dropped > 0 {
            warn!(
                dropped,
                "Stripped {dropped} orphaned tool_result message(s) with no matching tool_use in \
                 the selected context window (truncation cut across a tool-call group)"
            );
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
pub(crate) fn estimate_llm_message_tokens(msg: &LlmMessage) -> usize {
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

// content_to_string has been removed.  Use Content::to_display_string()
// (defined in alms_session::types) instead.

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
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall::new(
                "call_abc123",
                "shell_exec",
                r#"{"command":"ls -la"}"#,
            )]),
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
            ..Default::default()
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
    fn test_build_skips_empty_input() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];

        // Empty input should not produce a trailing user message
        let messages = builder.build("You are helpful.", &history, "", None);

        // system + 2 history = 3 (no empty user message appended)
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
    }

    #[test]
    fn test_truncate_respects_window() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 3,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
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
        // Budget 1200 tokens with a 1000-token safety buffer leaves ~200 for history.
        // System ~5 tokens, input ~2 tokens => reserved ~1007.
        // Each message is ~20 tokens at chars/3, so only a few should fit.
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 1200,
            recent_window: 100, // allow many messages
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
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

        // system + input = 2 fixed messages; history budget ~193 tokens fits ~9-10 messages
        assert!(
            messages.len() >= 3,
            "should include at least one history message"
        );
        assert!(
            messages.len() <= 14,
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
            ..Default::default()
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
            ..Default::default()
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
        let result = content.to_display_string();
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
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "run ls"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"command": "ls"}),
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
            ..Default::default()
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
                    params: serde_json::json!({"command": "ls"}),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_A"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "shell_exec".to_string(),
                    params: serde_json::json!({"command": "df -h"}),
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

    // -----------------------------------------------------------------------
    // apply_perspective tests
    // -----------------------------------------------------------------------

    fn make_msg_with_metadata(
        role: Role,
        text: &str,
        metadata: Option<serde_json::Value>,
    ) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata,
        }
    }

    fn default_builder() -> ContextBuilder {
        ContextBuilder::new(ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        })
    }

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

    #[test]
    fn test_group_tool_calls_does_not_merge_across_text() {
        // If there's an assistant text message between two tool call groups,
        // they should NOT be merged.
        let mut messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("call_1", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::assistant("some text in between"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("call_2", "echo", "{}")]),
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

    // -- Episodic injection tests ------------------------------------------------

    #[test]
    fn test_episodic_injected_between_system_and_history() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];

        let episodic = "Previous session: helped debug CORS.";
        let messages = builder.build_with_perspective(
            "You are helpful.",
            &history,
            "What's up?",
            None,
            None,
            Some(episodic),
        );

        // system + episodic + 2 history + current input = 5
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, "system"); // main system prompt
        assert_eq!(messages[1].role, "system"); // episodic
        assert!(messages[1].content_str().contains("CORS"));
        assert_eq!(messages[2].role, "user"); // history
        assert_eq!(messages[3].role, "assistant"); // history
        assert_eq!(messages[4].role, "user"); // current input
    }

    #[test]
    fn test_episodic_none_produces_no_extra_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![make_msg(Role::User, "Hello")];

        let messages =
            builder.build_with_perspective("System.", &history, "Input", None, None, None);

        // system + 1 history + current input = 3 (no episodic)
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user"); // history
        assert_eq!(messages[2].role, "user"); // input
    }

    #[test]
    fn test_episodic_empty_string_produces_no_extra_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![make_msg(Role::User, "Hello")];

        let messages =
            builder.build_with_perspective("System.", &history, "Input", None, None, Some(""));

        // system + 1 history + current input = 3 (empty episodic is skipped)
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn test_episodic_budget_does_not_eat_history() {
        // Budget: 1150 tokens.  System ~2 tokens, input ~2 tokens, episodic ~37 tokens.
        // Reserved (with episodic) = 2 + 2 + 37 + 1000 = 1041.  History = 1150 - 1041 = 109 tokens.
        // Reserved (no episodic)   = 2 + 2 + 0  + 1000 = 1004.  History = 1150 - 1004 = 146 tokens.
        // Each message is ~7 tokens, so with-episodic fits ~15, without fits ~20.
        // This verifies that episodic tokens come from the total budget, not history.
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 1150,
            recent_window: 100,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Create many short history messages (~7 tokens each at chars/3)
        let history: Vec<Message> = (0..20)
            .map(|i| make_msg(Role::User, &format!("Short message number {i}")))
            .collect();

        // Without episodic
        let msgs_no_episodic =
            builder.build_with_perspective("System", &history, "Input", None, None, None);

        // With a ~100-char (~33 token) episodic blob
        let episodic = "This is a somewhat long episodic summary that takes up about one hundred characters in total size";
        let msgs_with_episodic =
            builder.build_with_perspective("System", &history, "Input", None, None, Some(episodic));

        // With episodic, fewer history messages should fit (episodic takes from total budget)
        let history_no = msgs_no_episodic.len() - 2; // subtract system + input
        let history_with = msgs_with_episodic.len() - 3; // subtract system + episodic + input
        assert!(
            history_with < history_no,
            "Episodic injection should reduce available history slots: \
             without={history_no}, with={history_with}"
        );
    }

    #[test]
    fn test_episodic_with_sliding_summary() {
        // When both episodic and sliding-summary are used, the context order
        // should be: system -> episodic -> sliding-summary -> recent history -> input.
        let config = ContextConfig {
            strategy: "sliding-summary".into(),
            max_input_tokens: 32000,
            recent_window: 3,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history: Vec<Message> = (0..6)
            .map(|i| make_msg(Role::User, &format!("msg {i}")))
            .collect();

        let episodic = "Cross-session: debugged CORS issue.";
        let sliding = "Earlier the user asked about config.";
        let messages = builder.build_with_perspective(
            "System",
            &history,
            "current",
            Some(sliding),
            None,
            Some(episodic),
        );

        // system + episodic + sliding-summary + 3 recent + current = 7
        assert_eq!(messages.len(), 7);
        assert_eq!(messages[0].role, "system"); // main system prompt
        assert_eq!(messages[1].role, "system"); // episodic
        assert!(messages[1].content_str().contains("CORS"));
        assert_eq!(messages[2].role, "system"); // sliding-summary
        assert!(messages[2].content_str().contains("Context summary"));
        assert_eq!(messages[6].role, "user"); // current input
    }

    /// Notification runs (DM-ended, subagent completion) on user-facing
    /// sessions pre-persist the notification input as a Role::User message
    /// with `notification_input: true` metadata, then call
    /// `run_on_session` with an empty input string.
    ///
    /// This test verifies that the context builder:
    ///
    /// 1. Includes the notification_input User message from session history
    /// 2. Does NOT append a trailing user message (input is empty)
    /// 3. The resulting messages array ends with a user message (the
    ///    notification from history) — required by Anthropic and beneficial
    ///    for all OpenRouter models
    ///
    /// This covers the OpenRouter code path where the CompletionRequest is
    /// sent directly as OpenAI chat completions JSON (no system message
    /// extraction). The Role::User notification ensures the LLM sees a
    /// clear conversation turn to respond to.
    #[test]
    fn test_notification_run_context_ends_with_user_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Simulate a user-facing session with prior conversation + notification
        let notification_text =
            "[DM conversation ended] Agent Bob ended the conversation. Transcript: ...";
        let mut notif_msg = make_msg(Role::User, notification_text);
        notif_msg.metadata = Some(serde_json::json!({
            "notification_input": true,
        }));

        let history = vec![
            make_msg(Role::User, "Please message Bob about the project"),
            make_msg(Role::Assistant, "I'll send a message to Bob for you."),
            // Synthetic DM-ended marker (persisted by notify_dm_ended_to_webchat)
            {
                let mut marker = make_msg(Role::System, "[DM conversation ended]");
                marker.metadata = Some(serde_json::json!({
                    "synthetic": true,
                    "type": "dm_ended_notification",
                }));
                marker
            },
            // Notification input (pre-persisted by execute_run as Role::User)
            notif_msg,
        ];

        // Empty input — run_on_session passes "" since the notification is
        // already in the session history
        let messages = builder.build("You are an agent.", &history, "", None);

        // system + 4 history messages (user, assistant, system marker, user notification) = 5
        assert_eq!(messages.len(), 5, "expected system + 4 history messages");
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "system"); // synthetic marker
        assert_eq!(messages[4].role, "user"); // notification input
        assert!(
            messages[4].content_str().contains("DM conversation ended"),
            "last message should be the notification input"
        );

        // The messages array ends with a user message — this is critical
        // for Anthropic (requires trailing user turn after system extraction)
        // and beneficial for OpenRouter (models respond better to user turns
        // than to trailing system messages).
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "context must end with a user message for all providers"
        );
    }

    /// Verify that the OLD approach (Role::System notification) would NOT
    /// produce a trailing user message — confirming the bug that was fixed.
    ///
    /// This is a regression test: if someone reverts the Role::User change
    /// in lifecycle.rs, the notification would become Role::System in
    /// session history, and the context would end with a system message
    /// (or the preceding assistant message after Anthropic extraction).
    #[test]
    fn test_system_notification_does_not_end_with_user() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Simulate the OLD behavior where notification was Role::System
        let history = vec![
            make_msg(Role::User, "Please message Bob"),
            make_msg(Role::Assistant, "I'll message Bob."),
            make_msg(Role::System, "[DM ended] notification text"),
        ];

        // Empty input (run_on_session path)
        let messages = builder.build("System prompt.", &history, "", None);

        // system + 3 history = 4
        assert_eq!(messages.len(), 4);
        // Last message is the system notification — NOT a user message.
        // This would cause Anthropic rejection (no trailing user turn)
        // and poor model behavior on OpenRouter.
        assert_eq!(
            messages.last().unwrap().role,
            "system",
            "old Role::System approach ends with system, not user"
        );
    }

    /// Helper: create a message with metadata.
    fn make_msg_with_meta(role: Role, content: Content, metadata: serde_json::Value) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content,
            timestamp: Timestamp::now(),
            metadata: Some(metadata),
        }
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

        // system + 3 history + input = 5
        assert_eq!(
            messages.len(),
            5,
            "Without perspective, all messages pass through"
        );
    }

    // -- Orphaned tool_result stripping tests ------------------------------
    //
    // These pin the fix for the Anthropic 400 "unexpected tool_use_id found
    // in tool_result blocks" rejection that fires when truncation leaves a
    // tool_result at the head of the selected context window with no
    // matching tool_use in front of it.

    /// Directly exercises `strip_orphaned_tool_results`: a leading
    /// tool_result with no preceding assistant tool_use must be dropped.
    #[test]
    fn test_strip_orphaned_tool_results_drops_leading_orphan() {
        let mut messages = vec![
            LlmMessage::tool_result("functions_fs_write_4", "orphan result"),
            LlmMessage::user("follow-up question"),
        ];

        ContextBuilder::strip_orphaned_tool_results(&mut messages);

        assert_eq!(messages.len(), 1, "orphan tool_result must be dropped");
        assert_eq!(messages[0].role, "user");
    }

    /// Paired tool_use/tool_result survive untouched.
    #[test]
    fn test_strip_orphaned_tool_results_keeps_paired_pair() {
        let mut messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("call_A", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_A", "result A"),
            LlmMessage::user("thanks"),
        ];

        ContextBuilder::strip_orphaned_tool_results(&mut messages);

        assert_eq!(
            messages.len(),
            3,
            "paired tool_use/tool_result must stay intact"
        );
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[1].role, "tool");
        assert_eq!(messages[2].role, "user");
    }

    /// Middle-of-array orphan (never emitted by a preceding assistant) is
    /// also dropped — the Anthropic rejection fires after system extraction
    /// too, which could promote a mid-history orphan to `messages.0`.
    #[test]
    fn test_strip_orphaned_tool_results_drops_mid_array_orphan() {
        let mut messages = vec![
            LlmMessage::user("hello"),
            LlmMessage::tool_result("never_called_123", "orphan"),
            LlmMessage::assistant("some reply"),
        ];

        ContextBuilder::strip_orphaned_tool_results(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    /// Partial parallel tool-call coverage — if assistant emits tool_calls
    /// [A, B] and only tool_result B was truncated in, tool_result B is
    /// kept (A was introduced by the preceding assistant).  tool_result A
    /// would also be kept here because A is known.  Orphan-only if the
    /// introducer is missing.
    #[test]
    fn test_strip_orphaned_tool_results_parallel_group_kept_when_introduced() {
        let mut messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![
                    ToolCall::new("call_A", "echo", "{}"),
                    ToolCall::new("call_B", "echo", "{}"),
                ]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_B", "result B only"),
        ];

        ContextBuilder::strip_orphaned_tool_results(&mut messages);

        assert_eq!(
            messages.len(),
            2,
            "tool_result whose tool_use was introduced must be preserved"
        );
    }

    /// End-to-end regression for the reported 400:
    ///
    /// A session long enough to trigger `truncate` eviction of the
    /// paired assistant tool_use, leaving a leading `functions_fs_write_4`
    /// tool_result in the selected window.  After `build_with_perspective`
    /// the assembled context must NOT begin with a tool-role message —
    /// that is what Anthropic (direct or via OpenRouter→Claude) rejects as
    /// `messages.0.content.0: unexpected tool_use_id found in tool_result
    /// blocks`.
    #[test]
    fn test_build_never_leaves_tool_result_at_head_after_truncation() {
        // recent_window = 2 forces the truncate strategy to keep only the
        // two newest messages from a long history.  Arrange those last two
        // to be (tool_result, assistant-text) so the orphan would sit at
        // the front of the selected slice without the fix.
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32_000,
            recent_window: 2,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            // --- truncated out by recent_window=2 ---
            make_msg(Role::User, "please write the file"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "fs_write".to_string(),
                    params: serde_json::json!({"path": "/tmp/x"}),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "functions_fs_write_4"})),
            },
            // --- kept by recent_window=2 ---
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "functions_fs_write_4".to_string(),
                    result: serde_json::json!("ok"),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "Done — file written."),
        ];

        let messages = builder.build("You are helpful.", &history, "thanks!", None);

        // Head of the array must be a system message, and the first non-
        // system entry must NOT be a tool_result — Anthropic rejects that
        // shape after its system extraction step.
        assert_eq!(
            messages[0].role, "system",
            "context must start with the system prompt"
        );
        let first_non_system = messages
            .iter()
            .find(|m| m.role != "system")
            .expect("should have at least one non-system message");
        assert_ne!(
            first_non_system.role, "tool",
            "context must not lead with a tool_result (Anthropic 400 trigger)"
        );
    }
}
