// SPDX-License-Identifier: Apache-2.0

//! Context window builder for LLM requests.
//!
//! Manages what the LLM actually sees — assembles system prompt,
//! history (possibly compressed), and current input within a token budget.
//!
//! # Canonical message-shape invariant
//!
//! The invariant lives at two layers:
//!
//! - **Builder layer** (`Vec<LlmMessage>` out of
//!   [`ContextBuilder::build_with_perspective`]). Provider-agnostic. Uses
//!   the three-role alphabet `system` / `user` / `assistant` / `tool`.
//! - **Wire layer** (the actual request body each provider adapter emits).
//!   Uses the two-role alphabet the provider's API speaks. May require
//!   additional normalization after the adapter's own transforms (in
//!   particular, Anthropic's `tool` → `user` relabel — see
//!   `anthropic.rs`).
//!
//! The documentation below describes the builder layer only. Adapter
//! post-conditions are documented on the adapter entry-points.
//!
//! The output of [`ContextBuilder::build_with_perspective`] satisfies:
//!
//! 1. Non-empty.
//! 2. System messages appear only at the front (the "system prefix block").
//!    Mid-history `Role::System` markers are stripped — lifecycle markers
//!    (DM-ended, subagent completion, job notifications) surface through
//!    SSE/UI and do not belong in the LLM context.
//! 3. After the system prefix, no two adjacent messages share a role at
//!    the `LlmMessage` level: consecutive same-role pure-text turns are
//!    merged, and the tool-call / tool-result pairing from
//!    `normalize::group_tool_calls` is treated as a single logical turn.
//!    Note that `tool` and `user` are distinct roles here — a tail of
//!    `[tool_result, user_text]` is canonical; wire-level deduplication of
//!    the resulting adjacent `user` messages is the adapter's job.
//! 4. The last non-system message is `user`. Pending tool calls that are
//!    not followed by matching tool results are closed with synthetic
//!    `[Tool execution was interrupted]` results (the conventional pattern —
//!    see `message-v2.ts:804-814`), so no assistant-with-tool_calls tail
//!    ever reaches the provider adapters.
//! 5. No message has empty content (after stripping, assistant text-only
//!    messages with an empty body are dropped — conventional provider hygiene).
//!
//! Provider adapters may trust this canonical shape at the `Vec<LlmMessage>`
//! layer, but may need additional normalization after their own transforms.
//! The Anthropic adapter (`anthropic.rs`) relabels `role="tool"` to
//! `role="user"` and therefore runs its own `merge_consecutive_roles` pass
//! to restore wire-level alternation. The OpenAI-compatible adapter has no
//! role relabeling and serialises the canonical shape directly.

use crate::llm_types::LlmMessage;
use alms_core::config::ContextConfig;
use alms_session::Message;
use tracing::{debug, warn};

mod error_markers;
mod normalize;
mod perspective;
mod rebuild;
mod strategies;

// #1204: the compaction path in `agent::context::maybe_summarize` must key
// on the same canonical display-marker predicate the history-selection
// strategies use (#1203), so the two exemptions can never drift apart.
pub(crate) use error_markers::is_stripped_display_marker;

/// Token reserve subtracted from `max_input_tokens` when computing the
/// effective history budget, on top of the system prompt, current input,
/// and episodic summaries.
///
/// I6: Increased from 500 to 1000 tokens. The `estimate_tokens` heuristic
/// (`len / 3`) overestimates English but underestimates code and JSON
/// (~2-3 chars/token). A larger buffer provides a stronger safety margin
/// against LLM API rejection for `max_input_tokens` breaches.
///
/// Tim review (PR #1012, item 4): exposed as `pub(crate)` so the
/// `agent::context::AgentRuntime::build_context` overhead calculation
/// uses the same constant as the builder. A future edit that nudges
/// this value must propagate to every caller automatically — pre-#1012
/// the constant lived in two files and could silently desync, putting
/// the `maybe_summarize` trigger threshold and the actual builder
/// budget out of step.
pub(crate) const HISTORY_RESERVE: usize = 1000;

/// Builds the context window (Vec<LlmMessage>) for an LLM request.
pub struct ContextBuilder {
    config: ContextConfig,
    /// Workspace root used to resolve relative `spill_path` metadata when
    /// rebuilding tool-result messages from session history. When the
    /// referenced spill file no longer exists on disk (the per-run sweep
    /// has expired it), `rebuild::session_msg_to_llm` swaps the trailing
    /// recovery hint for an "expired" notice so an agent reading an older
    /// session doesn't get told to `fs_read` a path that returns ENOENT
    /// (#921 review fix #3).
    workspace_root: Option<std::path::PathBuf>,
}

impl ContextBuilder {
    pub fn new(config: ContextConfig) -> Self {
        Self {
            config,
            workspace_root: None,
        }
    }

    /// Set the workspace root used to resolve spill-path metadata when
    /// rebuilding tool-result messages from session history.
    ///
    /// Without a root, `rebuild::session_msg_to_llm` cannot tell whether a
    /// spill file referenced by a stored tool-result message has been swept
    /// (>7d retention) and falls back to leaving the original recovery
    /// hint intact — the LLM may try `fs_read` and get ENOENT, but the
    /// degradation is graceful.
    pub fn with_workspace_root(mut self, root: Option<std::path::PathBuf>) -> Self {
        self.workspace_root = root;
        self
    }

    /// Build the message list for an LLM call.
    ///
    /// Takes the full session history and produces a token-budgeted context window:
    /// `[system_prompt, (episodic?), (summary if needed), recent_messages, current_input]`
    ///
    /// `existing_summary` is only used when `strategy == "compact"`
    /// (or its deprecated alias `"sliding-summary"`, accepted for
    /// back-compat). Pass `None` for all other strategies.
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
        // The `HISTORY_RESERVE` constant (see module-level docs) sits on
        // top of system / input / episodic to keep a safety margin
        // against `estimate_tokens` underestimating code/JSON content.
        let reserved = system_tokens + input_tokens + episodic_tokens + HISTORY_RESERVE;
        let history_budget = self.config.max_input_tokens.saturating_sub(reserved);

        // Drop legacy lifecycle-layer `(run failed) ...` / `(run cancelled)`
        // `kind: "error"` `type: "run_boundary"` markers that sit
        // immediately after a runtime-layer `[Run failed: ...]` /
        // `[Run cancelled by user]` text bubble for the same conceptual run.
        //
        // Pre-#912 the gateway's `lifecycle.rs` `Cancelled`,
        // `CancelledWithToolCalls`, `FailedWithToolCalls`, and generic
        // `Err(_)` arms each wrote a `persist_error_marker` call AFTER
        // the runtime layer's `finish_run` had already persisted the
        // canonical `[Run failed: ...]` / `[Run cancelled by user]`
        // assistant bubble.  Both records reached the LLM context: the
        // bubble survived `strip_mid_history_system_markers` natively,
        // and the marker survived via `rebuild::session_msg_to_llm`'s
        // `kind: "error"` rewrite into a `[Error] ...` user message — so
        // the agent saw the same failure twice on every follow-up turn.
        //
        // #912 removed the four duplicate `persist_error_marker` calls,
        // which fixes the duplication for all NEW failed runs.  But
        // existing SQLite DBs that captured a failed run before #912
        // still have BOTH records — and the rewrite path means both
        // still surface to the LLM.  Filtering those legacy duplicates
        // here is the reconstruction-side fix for the legacy gap
        // (Tim's F2 finding on PR #930): the duplicate marker is dropped
        // during context build so legacy DBs match new-DB display
        // without touching user data.  Idempotent on post-#912 data
        // because new DBs never write the marker in the first place.
        //
        // The `runtime_init_error` marker (`type: "runtime_init_error"`)
        // is intentionally NOT matched: that path fires when
        // `AgentRuntime::new()` itself fails, before `finish_run` could
        // possibly run, so it has no runtime-layer counterpart and is
        // the only error record for those runs (kept by #912).
        let dedup_history: Vec<Message>;
        let history_after_dedup =
            if error_markers::has_legacy_duplicate_run_boundary_markers(history) {
                let before = history.len();
                dedup_history =
                    error_markers::filter_legacy_duplicate_run_boundary_markers(history);
                debug!(
                    filtered = before - dedup_history.len(),
                    "Filtered legacy duplicate run_boundary markers from context"
                );
                dedup_history.as_slice()
            } else {
                history
            };

        // Filter out PEER reasoning messages (message_type="reasoning"
        // with from_agent != perspective_agent) from DM sessions before
        // building context.  These are the peer agent's internal thinking
        // text, tool calls, and tool results persisted as Role::User to
        // preserve the DM invariant.  They should not reach this agent's
        // LLM context: token waste + malformed messages when the peer's
        // perspective-mapped ToolResult hits the catch-all in
        // rebuild::session_msg_to_llm (C2 in the #930 review).
        //
        // SAME-agent reasoning rows are retained (#988): they carry the
        // agent's own prior tool_use / tool_result history inside the
        // DM, which the agent needs for working memory across turn
        // boundaries.  Without this, every DM turn looked like turn 1
        // from the agent's perspective except for the textual
        // back-and-forth — e.g. a `fs_read` on turn N was invisible by
        // turn N+2, so the agent had to redo work or guess.
        // `apply_perspective` below re-stamps the surviving same-agent
        // rows onto their canonical roles so rebuild::session_msg_to_llm
        // reconstructs them as structured assistant / tool messages.
        let filtered_history: Vec<Message>;
        let effective_history = if let Some(agent) = perspective_agent
            && history_after_dedup
                .iter()
                .any(|m| perspective::is_peer_reasoning_message(m, agent))
        {
            let before = history_after_dedup.len();
            filtered_history = history_after_dedup
                .iter()
                .filter(|m| !perspective::is_peer_reasoning_message(m, agent))
                .cloned()
                .collect();
            debug!(
                filtered = before - filtered_history.len(),
                "Filtered peer reasoning messages from DM context"
            );
            filtered_history.as_slice()
        } else {
            history_after_dedup
        };

        // Pre-map the history if perspective is set, then pass the mapped
        // messages through the standard strategies.
        let mapped_history: Vec<Message>;
        let history_ref = if let Some(agent) = perspective_agent {
            mapped_history = effective_history
                .iter()
                .map(|msg| perspective::apply_perspective(msg, agent))
                .collect();
            &mapped_history
        } else {
            effective_history
        };

        let workspace_root = self.workspace_root.as_deref();
        match self.config.strategy.as_str() {
            "full" => {
                strategies::build_full(history_ref, history_budget, &mut messages, workspace_root);
            }
            "truncate" => {
                strategies::build_truncate(
                    history_ref,
                    history_budget,
                    &mut messages,
                    workspace_root,
                );
            }
            // #869: `compact` is the canonical name; `sliding-summary` is
            // accepted as a runtime alias for any path that bypassed the
            // deserialise / `normalize_episodic` rewrites (env vars,
            // hand-built `ContextConfig` literals).
            "compact" | "sliding-summary" => {
                // PR #1012 / Codex review medium #2: derive retain_budget
                // from the EFFECTIVE history budget, not raw
                // `max_input_tokens`. This keeps `retain_pct` aligned
                // with `maybe_summarize`'s trigger calculation (both now
                // measure against the assembled context window after
                // system / input / episodic / reserve overhead) and
                // preserves the gap-floor invariant
                // (`retain + 0.10 <= trigger`) at runtime. Pre-fix, large
                // overhead silently flipped retain to be larger than the
                // effective trigger.
                let retain_budget =
                    (self.config.compact_retain_pct * history_budget as f32) as usize;
                strategies::build_compact(
                    history_ref,
                    history_budget,
                    &mut messages,
                    existing_summary,
                    retain_budget,
                    workspace_root,
                );
            }
            _ => {
                warn!(
                    "Unknown context strategy '{}', using truncate",
                    self.config.strategy
                );
                strategies::build_truncate(
                    history_ref,
                    history_budget,
                    &mut messages,
                    workspace_root,
                );
            }
        }

        // 5. Group consecutive assistant tool-call messages into single messages
        // with multiple tool_calls entries (required by OpenAI/Anthropic APIs).
        normalize::group_tool_calls(&mut messages);

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
        normalize::strip_orphaned_tool_results(&mut messages);

        // 6. Current input (skip if empty — avoids sending a blank user message to the LLM)
        let has_fresh_input = !current_input.is_empty();
        if has_fresh_input {
            messages.push(LlmMessage::user(current_input));
        }

        // 7. Enforce the canonical message-shape invariant (see module docs).
        // Runs as the final step so every upstream path (perspective mapping,
        // episodic injection, all three selection strategies) produces the
        // same shape for the provider adapters.
        normalize::normalize_for_llm(&mut messages, has_fresh_input);

        debug!(
            "Context built: {} messages, ~{} tokens (budget: {})",
            messages.len(),
            self.estimate_total_tokens(&messages),
            self.config.max_input_tokens
        );

        messages
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

/// Estimate tokens for a persisted [`Message`] without paying the full
/// cost of [`rebuild::session_msg_to_llm`] (which canonicalises tool
/// calls / tool results, resolves spill paths, etc.).
///
/// Used by `maybe_summarize` (#869) to compute the uncovered-tail token
/// estimate that drives threshold-based compaction. Slightly less precise
/// than `estimate_llm_message_tokens` because it works on the persisted
/// content shape directly, but it doesn't need to be exact — the figure
/// is compared against `compact_trigger_pct` of the effective history
/// budget (PR #1012), where a few percent of slack is normal.
pub(crate) fn estimate_session_message_tokens(msg: &Message) -> usize {
    use alms_session::Content;
    match &msg.content {
        Content::Text(s) => estimate_tokens(s),
        Content::ToolCall { name, params } => {
            estimate_tokens(name) + estimate_tokens(&params.to_string()) + 10
        }
        Content::ToolResult { tool_id, result } => {
            estimate_tokens(tool_id) + estimate_tokens(&result.to_string()) + 10
        }
        // Images are token-priced server-side by the provider; the
        // text/`alt` fallback is the cheapest reasonable proxy.
        Content::Image { url, alt } => {
            estimate_tokens(url) + alt.as_deref().map(estimate_tokens).unwrap_or(0)
        }
    }
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
    use alms_session::{Content, Role};

    pub(super) fn make_msg(role: Role, text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: None,
        }
    }

    pub(super) fn make_msg_with_metadata(
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

    pub(super) fn make_msg_with_meta(
        role: Role,
        content: Content,
        metadata: serde_json::Value,
    ) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content,
            timestamp: Timestamp::now(),
            metadata: Some(metadata),
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
        use crate::llm_types::ToolCall;
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
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
        ];

        // Empty input + assistant-tail history: no real user message to
        // append, but the canonical invariant synthesises a trailing
        // user placeholder so the shape reaches the provider in one piece.
        let messages = builder.build("You are helpful.", &history, "", None);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "user");
        assert!(messages[3].content_str().contains("Please continue"));
    }

    // -- Episodic injection tests ------------------------------------------------

    #[test]
    fn test_episodic_injected_between_system_and_history() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
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
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![make_msg(Role::User, "Hello")];

        let messages =
            builder.build_with_perspective("System.", &history, "Input", None, None, None);

        // Two consecutive user turns (history "Hello" + input "Input")
        // merge into one under the canonical invariant.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content_str().contains("Hello"));
        assert!(messages[1].content_str().contains("Input"));
    }

    #[test]
    fn test_episodic_empty_string_produces_no_extra_message() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![make_msg(Role::User, "Hello")];

        let messages =
            builder.build_with_perspective("System.", &history, "Input", None, None, Some(""));

        // Same shape as the `None` case: merging collapses two consecutive
        // user turns into one — empty episodic string is skipped, so there
        // is no second system entry to separate them.
        assert_eq!(messages.len(), 2);
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
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Create many short history messages (~7 tokens each at chars/3).
        // Alternate roles so normalize does not merge them — we need the
        // actual count-dropoff to show that episodic consumes budget.
        let history: Vec<Message> = (0..20)
            .map(|i| {
                let role = if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                make_msg(role, &format!("Short message number {i}"))
            })
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
    fn test_episodic_with_compact_strategy() {
        // When both episodic and compact (#869) are used, the context order
        // should be: system -> episodic -> compact-summary -> recent history -> input.
        // The legacy `"sliding-summary"` value is also accepted at the
        // dispatch layer as a back-compat alias.
        let config = ContextConfig {
            strategy: "compact".into(),
            max_input_tokens: 32_000,
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

        // system + episodic + compact-summary + 6 recent + current = 10.
        // Pre-#869 this was capped at `recent_window = 3`; with the
        // threshold model the verbatim tail comfortably fits all six
        // tiny messages within the default `compact_retain_pct = 0.40`
        // applied to the effective history budget (PR #1012):
        // `0.40 × (max_input_tokens − reserved overhead)`
        // ≈ `0.40 × (32_000 − 1019)` ≈ 12,392 tokens of headroom.
        assert_eq!(messages.len(), 10);
        assert_eq!(messages[0].role, "system"); // main system prompt
        assert_eq!(messages[1].role, "system"); // episodic
        assert!(messages[1].content_str().contains("CORS"));
        assert_eq!(messages[2].role, "system"); // compact-summary
        assert!(messages[2].content_str().contains("Context summary"));
        assert_eq!(messages.last().unwrap().role, "user"); // current input
        // Invariant: no mid-history system messages survive.
        assert!(messages[3..].iter().all(|m| m.role != "system"));
    }

    /// Tim review nit (PR #1012, item 2; tracked for v0.3.0 removal
    /// in #1017): the runtime dispatch arm for
    /// `strategy: "sliding-summary"` is a belt-and-braces fallback for
    /// any path that bypassed `ContextConfig::Deserialize`,
    /// `normalize_episodic`, and the gateway PATCH rewrite — e.g. an
    /// in-process caller building a `ContextConfig` literal. A future
    /// refactor that drops the alias arm without also dropping every
    /// upstream rewrite would be silently caught by this test.
    ///
    /// We construct the config via direct struct literal (NOT
    /// `ContextConfig::default()`-then-mutate, which is the same shape,
    /// but explicit struct literal makes the intent clear) and exercise
    /// the dispatch by passing a non-empty `existing_summary`. The
    /// compact path is the only one that injects the summary block as
    /// a leading system message; truncate / full strip it. So if the
    /// alias arm were dropped without an upstream guard, the summary
    /// block would silently disappear from the assembled context.
    #[test]
    fn test_sliding_summary_alias_routes_through_compact() {
        let config = ContextConfig {
            // Hand-built literal — bypasses Deserialize / normalize.
            strategy: "sliding-summary".into(),
            max_input_tokens: 32_000,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history: Vec<Message> = (0..4)
            .map(|i| {
                if i % 2 == 0 {
                    make_msg(Role::User, &format!("q {i}"))
                } else {
                    make_msg(Role::Assistant, &format!("a {i}"))
                }
            })
            .collect();

        let summary = "Earlier the user asked about config changes.";
        let messages = builder.build("System", &history, "current", Some(summary));

        // Expected layout when the alias dispatched through `build_compact`:
        // [0] = system prompt
        // [1] = compact-summary system block (the existing_summary text)
        // [2..6] = 4 verbatim history messages
        // [6] = current input (user)
        assert_eq!(
            messages.len(),
            7,
            "summary block + 4 history + 1 input + system"
        );
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system");
        assert!(
            messages[1].content_str().contains("Context summary"),
            "compact path injects summary block; truncate / full do not.              Got: {:?}",
            messages[1].content_str()
        );
        assert!(
            messages[1].content_str().contains("config changes"),
            "summary block must carry the existing_summary text"
        );
        assert_eq!(messages.last().unwrap().role, "user");
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

        // The mid-history synthetic marker gets stripped by the #586
        // canonical-shape invariant: system + user + assistant + user
        // (notification input) = 4.
        assert_eq!(
            messages.len(),
            4,
            "mid-history synthetic marker must be stripped"
        );
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "user");
        assert!(
            messages[3].content_str().contains("DM conversation ended"),
            "last message should be the notification input"
        );
        // No stray system marker mid-stream.
        assert!(
            messages[1..].iter().all(|m| m.role != "system"),
            "no mid-history system marker may survive normalization"
        );
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "context must end with a user message for all providers"
        );
    }

    /// Even if a lifecycle bug leaves ONLY a Role::System notification in
    /// history (no `notification_input` user message), the canonical
    /// invariant must still emit a messages array that ends with user.
    /// Pre-#586 this path produced a trailing system message and Anthropic
    /// rejected the request.
    #[test]
    fn test_system_notification_still_ends_with_user() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            make_msg(Role::User, "Please message Bob"),
            make_msg(Role::Assistant, "I'll message Bob."),
            make_msg(Role::System, "[DM ended] notification text"),
        ];

        let messages = builder.build("System prompt.", &history, "", None);

        // Expect: system + user + assistant + synthesised user placeholder.
        // Mid-history system marker gets stripped; trailing-user synthesis
        // kicks in because the tail after stripping is assistant.
        assert_eq!(messages[0].role, "system");
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "invariant: context must end with user even when notification \
             was incorrectly persisted as Role::System"
        );
        assert!(
            messages[1..].iter().all(|m| m.role != "system"),
            "mid-history system marker must be stripped"
        );
    }
}
