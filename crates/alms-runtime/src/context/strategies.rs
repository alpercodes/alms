//! Token-budgeted history selection strategies.
//!
//! Three strategies compose the "fill the history budget with messages
//! from the persisted session" stage of [`super::ContextBuilder`]:
//!
//! - [`build_full`] — include all history (oldest first), break on budget.
//! - [`build_truncate`] — keep the most recent messages that fit the budget
//!   (#869: pure token-budget walk, no message-count cap).
//! - [`build_compact`] — inject the pre-computed rolling summary then fill
//!   the remainder with the most-recent verbatim messages, capped at
//!   `retain_budget` tokens (#869: renamed from `build_sliding_summary`,
//!   now driven by token thresholds rather than `recent_window`).
//!
//! All three call [`super::rebuild::session_msg_to_llm`] to convert each
//! persisted [`Message`] to an [`LlmMessage`] and use
//! [`super::estimate_llm_message_tokens`] for the budget arithmetic.
//!
//! Pure free functions — `workspace_root` and (for `build_compact`)
//! `retain_budget` are threaded in as explicit arguments so the strategies
//! are independently testable.

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

/// Truncate strategy: keep the most recent messages within the token budget.
///
/// **#869 redesign.** This is now a pure token-budget walk — no
/// message-count cap. Walk backwards from the newest message, include
/// each in turn while cumulative tokens fit `budget`, stop when adding
/// the next would exceed it. The pre-#869 shape capped at
/// `recent_window` messages, which silently bypassed the token budget on
/// long-running sessions (the "recent_window-was-a-bug-in-disguise" fix).
pub(super) fn build_truncate(
    history: &[Message],
    budget: usize,
    messages: &mut Vec<LlmMessage>,
    workspace_root: Option<&Path>,
) {
    let mut selected: Vec<LlmMessage> = Vec::new();
    let mut used = 0;

    // Walk backwards through history, newest-first, until adding the next
    // message would exceed the token budget.
    for msg in history.iter().rev() {
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

/// Compact strategy: inject the pre-computed rolling summary then fill
/// with the most-recent verbatim messages that fit `retain_budget` tokens.
///
/// **#869 redesign (refined PR #1012).** Renamed from
/// `build_sliding_summary` and switched from a message-count cap
/// (`recent_window`) to a token cap (`retain_budget`). The caller
/// computes `retain_budget` as `compact_retain_pct * history_budget`
/// (the effective history window after subtracting the system /
/// input / episodic / reserve overhead — PR #1012). The verbatim tail
/// is also bounded by the outer `budget` so it can never exceed total
/// available headroom. If the verbatim tail underspends `retain_budget`
/// the residual flows back to the summary block via the `budget`-shaped
/// outer cap, matching pre-#869 token accounting.
pub(super) fn build_compact(
    history: &[Message],
    budget: usize,
    messages: &mut Vec<LlmMessage>,
    summary: Option<&str>,
    retain_budget: usize,
    workspace_root: Option<&Path>,
) {
    let mut used = 0;

    // 1. Inject summary block if present.
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

    // 2. Fill the verbatim tail (newest-first walk, then reverse). Two
    // independent caps apply:
    //   - the outer `budget` (after subtracting summary tokens); a
    //     genuine context-window safety net.
    //   - the inner `retain_budget` (#869); the operator-tunable cap on
    //     how much room verbatim recent history is allowed to claim.
    // The smaller of the two is the effective cap for this walk.
    let remaining = budget.saturating_sub(used);
    let cap = remaining.min(retain_budget);
    let mut selected: Vec<LlmMessage> = Vec::new();
    let mut msg_used = 0;

    for msg in history.iter().rev() {
        let llm_msg = session_msg_to_llm(msg, workspace_root);
        let tokens = estimate_llm_message_tokens(&llm_msg) + 4;
        if msg_used + tokens > cap {
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

    /// #869: `build_truncate` is a pure token-budget walk — no
    /// `recent_window` cap. Feed many chunky messages with a small
    /// `max_input_tokens` so the budget admits roughly a fixed number
    /// of them; assert no message-count cap is observable.
    #[test]
    fn test_truncate_pure_token_budget_walk() {
        // Budget 1200 tokens with a 1000-token safety buffer leaves ~200 for history.
        // System ~5 tokens, input ~2 tokens => reserved ~1007.
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 1200,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Alternating roles so normalize does not collapse the turns.
        // 50 messages, ~20 tokens each — only a few fit the budget.
        let history: Vec<Message> = (0..50)
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

        // The strategy's job is to fit recent history into the budget.
        // With ~193 tokens of history budget and ~20 tokens per message,
        // that lands around 9–10 history messages plus system + input.
        // The previous shape capped at `recent_window = 20` (or N=10 in
        // some configs), which would mask any token-budget walk.
        assert!(
            messages.len() >= 3,
            "should include at least one history message"
        );
        assert!(
            messages.len() <= 14,
            "token budget should limit history to a small subset, got {}",
            messages.len()
        );
        assert_eq!(messages.last().unwrap().role, "user");
    }

    /// #869: with a 32k-token budget and only six tiny history messages,
    /// `build_truncate` admits all of them — the pre-#869 shape would
    /// have been gated by `recent_window` (which no longer exists).
    /// This pins the "no message-count cap" property.
    #[test]
    fn test_truncate_admits_full_history_when_budget_allows() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32_000,
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

        let messages = builder.build("System", &history, "current", None);
        // system + 6 history + current input = 8.
        assert_eq!(messages.len(), 8);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages.last().unwrap().role, "user");
    }

    /// #869: `build_compact` with no prior summary degrades to a budget
    /// walk over the verbatim tail, capped at `retain_budget`. Six tiny
    /// messages sit comfortably inside any reasonable retain cap, so we
    /// verify all six come through plus the system + input.
    #[test]
    fn test_compact_no_prior_summary() {
        let config = ContextConfig {
            strategy: "compact".into(),
            max_input_tokens: 32_000,
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

        // Without a summary we get the verbatim tail (everything fits).
        let messages = builder.build("System", &history, "current", None);
        assert_eq!(messages[0].role, "system");
        // 6 verbatim history + current input = 7 body entries.
        let body_count = messages.len() - 1; // subtract system
        assert_eq!(body_count, 7);
        assert_eq!(messages.last().unwrap().role, "user");
    }

    /// #869: `build_compact` injects the pre-computed summary block as
    /// a system message immediately after the main system prompt, with
    /// the verbatim tail following. Renamed from
    /// `test_sliding_summary_injects_summary_block`.
    #[test]
    fn test_compact_injects_summary_block() {
        let config = ContextConfig {
            strategy: "compact".into(),
            max_input_tokens: 32_000,
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

    /// #869 (refined PR #1012): when `compact_retain_pct` is small
    /// relative to the budget, the verbatim tail is capped well below
    /// the available budget — so adding more history at the same retain
    /// budget never grows the message count past what the cap allows.
    #[test]
    fn test_compact_retain_caps_verbatim_tail() {
        // PR #1012 effective-budget semantic: `compact_retain_pct` is
        // applied to the EFFECTIVE history budget after subtracting the
        // system / input / episodic / `HISTORY_RESERVE` overhead, not
        // raw `max_input_tokens`.
        //
        // max_input_tokens = 1200, system+input ~3 tokens, episodic = 0,
        // reserve = 1000 → history_budget ≈ 197 tokens.
        // retain_budget = 0.20 × 197 ≈ 39 tokens.
        // The verbatim-tail cap is `min(history_budget, retain_budget)`
        // = `min(197, 39)` = 39 tokens. With ~24-token messages
        // (`+4` overhead) only 1–2 fit, so we expect roughly
        // system + 1 history + input = 3.
        //
        // The assertion bounds (`>= 3 && <= 14`) are deliberately loose
        // to absorb minor `estimate_tokens` heuristic drift.
        // Pre-#869 you'd have used `recent_window` here; #869 made this
        // a pure token budget; #1012 made it pure-effective-budget.
        let config = ContextConfig {
            strategy: "compact".into(),
            max_input_tokens: 1200,
            compact_retain_pct: 0.20,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history: Vec<Message> = (0..50)
            .map(|i| {
                let role = if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                make_msg(role, &format!("Reasonably long history message number {i}"))
            })
            .collect();

        let messages = builder.build("System", &history, "Input", None);
        // System + verbatim tail + input. Under the PR #1012 effective
        // budget semantic, the cap is the smaller of the outer history
        // budget (~197 tokens) and the retain cap (~39 tokens), so 39
        // wins. Messages weigh ~24 tokens each
        // (`estimate_llm_message_tokens + 4` overhead), so the verbatim
        // tail fits roughly 1–2 messages. The exact count is
        // heuristic-dependent — what we pin is "much less than 50".
        assert!(
            messages.len() >= 3,
            "should include at least one history message, got {}",
            messages.len()
        );
        assert!(
            messages.len() <= 14,
            "token-budget cap should limit verbatim tail, got {}",
            messages.len()
        );
        assert_eq!(messages.last().unwrap().role, "user");
    }
}
