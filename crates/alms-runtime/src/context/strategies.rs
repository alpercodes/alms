//! Token-budgeted history selection strategies.
//!
//! Three strategies compose the "fill the history budget with messages
//! from the persisted session" stage of [`super::ContextBuilder`]:
//!
//! - [`build_full`] — include all history (oldest first), break on budget
//! - [`build_truncate`] — keep the most recent messages within budget
//! - [`build_sliding_summary`] — inject the pre-computed rolling summary
//!   then fill the remainder with the most-recent messages
//!
//! All three call [`super::rebuild::session_msg_to_llm`] to convert each
//! persisted [`Message`] to an [`LlmMessage`] and use
//! [`super::estimate_llm_message_tokens`] for the budget arithmetic.
//!
//! Pure free functions — `recent_window` and `workspace_root` are threaded
//! in as explicit arguments so the strategies are independently testable.

use crate::llm_types::LlmMessage;
use alms_session::Message;
use std::path::Path;
use tracing::warn;

use super::rebuild::session_msg_to_llm;
use super::{estimate_llm_message_tokens, estimate_tokens};

/// Full strategy: include all history (oldest to newest), skip if over budget.
pub(super) fn build_full(
    history: &[Message],
    budget: usize,
    messages: &mut Vec<LlmMessage>,
    workspace_root: Option<&Path>,
) {
    let mut used = 0;
    for msg in history {
        let llm_msg = session_msg_to_llm(msg, workspace_root);
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
pub(super) fn build_truncate(
    history: &[Message],
    budget: usize,
    messages: &mut Vec<LlmMessage>,
    recent_window: usize,
    workspace_root: Option<&Path>,
) {
    let mut selected: Vec<LlmMessage> = Vec::new();
    let mut used = 0;

    // Walk backwards through history
    for msg in history.iter().rev() {
        if selected.len() >= recent_window {
            break;
        }
        let llm_msg = session_msg_to_llm(msg, workspace_root);
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
pub(super) fn build_sliding_summary(
    history: &[Message],
    budget: usize,
    messages: &mut Vec<LlmMessage>,
    summary: Option<&str>,
    recent_window: usize,
    workspace_root: Option<&Path>,
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

    for msg in history.iter().rev() {
        if selected.len() >= recent_window {
            break;
        }
        let llm_msg = session_msg_to_llm(msg, workspace_root);
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

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::make_msg;
    use alms_core::config::ContextConfig;
    use alms_session::{Message, Role};

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

        // Alternating roles so normalize does not collapse the turns.
        let history: Vec<Message> = (0..20)
            .map(|i| {
                let role = if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                make_msg(
                    role,
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
        assert_eq!(messages.last().unwrap().role, "user");
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

        // Alternating user/assistant history so normalize's merge pass
        // does not collapse turns into a single user block.
        let history: Vec<Message> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    make_msg(Role::User, &format!("q {i}"))
                } else {
                    make_msg(Role::Assistant, &format!("a {i}"))
                }
            })
            .collect();

        // Without a summary it should behave like truncate (keep last 3)
        let messages = builder.build("System", &history, "current", None);
        assert_eq!(messages[0].role, "system");
        // 3 kept by recent_window + current input = 4 body entries
        let body_count = messages.len() - 1; // subtract system
        assert_eq!(body_count, 4);
        assert_eq!(messages.last().unwrap().role, "user");
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
            .map(|i| {
                if i % 2 == 0 {
                    make_msg(Role::User, &format!("q {i}"))
                } else {
                    make_msg(Role::Assistant, &format!("a {i}"))
                }
            })
            .collect();

        let messages = builder.build(
            "System",
            &history,
            "current",
            Some("Earlier the user greeted."),
        );

        // system prefix is preserved as a contiguous block; normalize
        // strips only mid-history system markers, not the leading run.
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system");
        assert!(messages[1].content_str().contains("[Context summary"));
        assert!(
            messages[1]
                .content_str()
                .contains("Earlier the user greeted.")
        );
        assert_eq!(messages.last().unwrap().role, "user");
    }
}
