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

use crate::llm_types::{LlmMessage, ToolCall};
use alms_core::config::ContextConfig;
use alms_core::truncate_to_char_boundary;
use alms_session::{Content, Message, Role};
use tracing::{debug, warn};

mod normalize;

/// Builds the context window (Vec<LlmMessage>) for an LLM request.
pub struct ContextBuilder {
    config: ContextConfig,
    /// Workspace root used to resolve relative `spill_path` metadata when
    /// rebuilding tool-result messages from session history. When the
    /// referenced spill file no longer exists on disk (the per-run sweep
    /// has expired it), `session_msg_to_llm` swaps the trailing recovery
    /// hint for an "expired" notice so an agent reading an older session
    /// doesn't get told to `fs_read` a path that returns ENOENT (#921 review
    /// fix #3).
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
    /// Without a root, `session_msg_to_llm` cannot tell whether a spill
    /// file referenced by a stored tool-result message has been swept
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
        // and the marker survived via `session_msg_to_llm`'s `kind:
        // "error"` rewrite into a `[Error] ...` user message — so the
        // agent saw the same failure twice on every follow-up turn.
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
        let history_after_dedup = if Self::has_legacy_duplicate_run_boundary_markers(history) {
            let before = history.len();
            dedup_history = Self::filter_legacy_duplicate_run_boundary_markers(history);
            debug!(
                filtered = before - dedup_history.len(),
                "Filtered legacy duplicate run_boundary markers from context"
            );
            dedup_history.as_slice()
        } else {
            history
        };

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
        let effective_history = if perspective_agent.is_some()
            && history_after_dedup.iter().any(Self::is_reasoning_message)
        {
            let before = history_after_dedup.len();
            filtered_history = history_after_dedup
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
            history_after_dedup
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

    /// Returns `true` when the message is an error marker persisted by
    /// `persist_error_marker` in the gateway (issue #874).
    ///
    /// Error markers are `Role::System` synthetic markers tagged with
    /// `metadata.kind == "error"`. They surface mid-run failures (LLM
    /// 4xx/5xx, run cancellation, runtime construction error) into the
    /// agent's context so a follow-up turn like "why did that fail?"
    /// gives the LLM the error text without re-quoting.
    fn is_error_marker(msg: &Message) -> bool {
        if msg.role != Role::System {
            return false;
        }
        msg.metadata
            .as_ref()
            .and_then(|m| m.get("kind"))
            .and_then(|v| v.as_str())
            == Some("error")
    }

    /// Returns `true` when the message is the lifecycle-layer
    /// `(run failed) ...` / `(run cancelled)` `kind: "error"` marker
    /// removed in #912.
    ///
    /// Identified by `metadata.kind == "error"` AND `metadata.type ==
    /// "run_boundary"` — exactly the shape persisted by the four
    /// `persist_error_marker` call sites in `gateway::runs::lifecycle`'s
    /// `Cancelled`, `CancelledWithToolCalls`, `FailedWithToolCalls`, and
    /// generic `Err(_)` arms before #912.  Crucially does NOT match
    /// `type: "runtime_init_error"` — that marker has no runtime-layer
    /// counterpart and is the only error record for runtime-construction
    /// failures, so it must be kept.
    fn is_legacy_lifecycle_error_marker(msg: &Message) -> bool {
        if !Self::is_error_marker(msg) {
            return false;
        }
        msg.metadata
            .as_ref()
            .and_then(|m| m.get("type"))
            .and_then(|v| v.as_str())
            == Some("run_boundary")
    }

    /// Returns `true` when the message is the runtime-layer
    /// `[Run failed: ...]` or `[Run cancelled by user]` text bubble
    /// persisted by `AgentRuntime::finish_run` on the `Err(_)` and
    /// `Err(Cancelled)` arms.
    ///
    /// Matches both the regular-session shape (`Role::Assistant` text)
    /// and the DM-session shape (`Role::User` with `from_agent`
    /// metadata) — both bubbles share the literal `[Run failed:` /
    /// `[Run cancelled by user]` content prefix.
    fn is_runtime_layer_failure_bubble(msg: &Message) -> bool {
        // Runtime-layer bubbles are never persisted as `Role::System`,
        // so a `System` here is necessarily a marker, not a bubble.
        if msg.role == Role::System {
            return false;
        }
        match &msg.content {
            Content::Text(t) => {
                t.starts_with("[Run failed:") || t.starts_with("[Run cancelled by user]")
            }
            _ => false,
        }
    }

    /// Returns `true` when `history` contains a legacy lifecycle-layer
    /// `kind: "error"` `type: "run_boundary"` marker immediately after
    /// (or before) a runtime-layer failure bubble — the duplicate
    /// pattern from pre-#912 SQLite DBs.  Used as a cheap pre-check to
    /// avoid building a fresh `Vec<Message>` when the common case
    /// (post-#912 data, no duplicates) holds.
    fn has_legacy_duplicate_run_boundary_markers(history: &[Message]) -> bool {
        for (idx, msg) in history.iter().enumerate() {
            if !Self::is_legacy_lifecycle_error_marker(msg) {
                continue;
            }
            // Pre-#912 the runtime-layer bubble is written first, then
            // the lifecycle-layer marker — so the bubble is at idx-1.
            // We also check idx+1 defensively in case a future ordering
            // change inverts that.
            let prev_is_bubble = idx
                .checked_sub(1)
                .and_then(|i| history.get(i))
                .is_some_and(Self::is_runtime_layer_failure_bubble);
            let next_is_bubble = history
                .get(idx + 1)
                .is_some_and(Self::is_runtime_layer_failure_bubble);
            if prev_is_bubble || next_is_bubble {
                return true;
            }
        }
        false
    }

    /// Drop legacy lifecycle-layer `kind: "error"` `type: "run_boundary"`
    /// markers that sit adjacent to a runtime-layer failure bubble,
    /// returning a fresh `Vec<Message>` with the duplicates removed.
    ///
    /// Caller is responsible for the cheap pre-check
    /// (`has_legacy_duplicate_run_boundary_markers`) — calling this on
    /// post-#912 data simply clones the slice unchanged, but allocating
    /// a fresh `Vec` per build call when there's nothing to filter is
    /// wasteful.
    fn filter_legacy_duplicate_run_boundary_markers(history: &[Message]) -> Vec<Message> {
        history
            .iter()
            .enumerate()
            .filter(|(idx, msg)| {
                if !Self::is_legacy_lifecycle_error_marker(msg) {
                    return true;
                }
                let prev_is_bubble = idx
                    .checked_sub(1)
                    .and_then(|i| history.get(i))
                    .is_some_and(Self::is_runtime_layer_failure_bubble);
                let next_is_bubble = history
                    .get(idx + 1)
                    .is_some_and(Self::is_runtime_layer_failure_bubble);
                // Drop only when adjacent to a bubble.  Markers that
                // appear without an adjacent bubble (corrupted DB,
                // unusual write order, future code path) are kept so
                // the agent still sees the failure on follow-up.
                !(prev_is_bubble || next_is_bubble)
            })
            .map(|(_, msg)| msg.clone())
            .collect()
    }

    /// Convert a session Message to an LlmMessage.
    /// Reconstructs structured tool call/result messages from persisted format
    /// so the LLM has full visibility of previous tool executions across runs.
    fn session_msg_to_llm(&self, msg: &Message) -> LlmMessage {
        // Error markers (#874) are converted to a `user` message with a
        // `[Error]` prefix BEFORE the per-role match below, so they
        // survive `strip_mid_history_system_markers` and reach the LLM.
        // Without this, the existing canonical-shape pass would drop
        // them and the agent would never see the prior failure.
        if Self::is_error_marker(msg) {
            return LlmMessage::user(format!("[Error] {}", msg.content.to_display_string()));
        }

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
                // Heal legacy `params: ""` poison written by the
                // pre-#967 persistence path: a no-args Anthropic tool
                // call (where `arguments == ""`) was stored as
                // `Value::String("")` instead of `Value::Object({})`,
                // and re-serializing that on the wire produced
                // `tool_use.input: ""` which Anthropic 400s. The
                // adapters' `normalize_tool_args` would also catch this
                // (wrapping under `_raw`), but normalizing here means
                // the rebuilt arguments are the truthful `{}` rather
                // than `{"_raw":"\"\""}`.
                let params_str = if matches!(params, serde_json::Value::String(s) if s.is_empty()) {
                    "{}".to_string()
                } else {
                    params.to_string()
                };
                LlmMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall::new(tool_call_id, name.clone(), params_str)]),
                    tool_call_id: None,
                }
            }
            // Reconstruct tool result with correct tool_call_id
            (Role::Tool, Content::ToolResult { tool_id, result }) => {
                let result_str = result.to_string();
                // When the in-loop truncation service (#851) already
                // capped this message, the persisted bytes are exactly
                // what the live agent saw. Skip the legacy 2000-byte
                // re-truncation in that case — re-truncating would shrink
                // the head+tail preview to ~2 KB and discard the spill-
                // path hint the agent needs to recover the full bytes.
                let truncated_in_loop = msg
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("truncated_in_loop"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let content = if truncated_in_loop {
                    // Detect a swept spill file (#921 review fix #3): if the
                    // recorded `spill_path` no longer exists on disk (>7 day
                    // retention sweep has expired it), rewrite the trailing
                    // recovery hint so the agent doesn't try to `fs_read` a
                    // path that returns ENOENT.
                    let spill_path = msg
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("spill_path"))
                        .and_then(|v| v.as_str());
                    if let Some(rel) = spill_path
                        && let Some(ref root) = self.workspace_root
                        && Self::spill_file_missing(root, rel)
                    {
                        Self::rewrite_hint_as_expired(&result_str, rel)
                    } else {
                        result_str
                    }
                } else if result_str.len() > 2000 {
                    // Legacy fallback for tool-result messages persisted
                    // before #851 (or when the in-loop service is
                    // disabled by config). Keeps the pre-#851 wire shape
                    // byte-identical so existing fixtures continue to
                    // pass.
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

    /// Resolve the relative `spill_path` from a tool-result message's
    /// metadata against `workspace_root` and return `true` when the file
    /// does NOT exist on disk (i.e. the retention sweep has expired it).
    ///
    /// `rel` is the path emitted by `tool_output_truncate::truncate` —
    /// either workspace-relative (when a workspace root was passed in at
    /// truncate time) or an absolute path (no workspace root). We try
    /// `workspace_root.join(rel)` first; when that doesn't exist *and* the
    /// raw `rel` parses as an absolute path, we try that too. Either path
    /// existing is enough to consider the spill "live"; both missing means
    /// it has been swept (or never made it to disk).
    fn spill_file_missing(workspace_root: &std::path::Path, rel: &str) -> bool {
        let joined = workspace_root.join(rel);
        if joined.exists() {
            return false;
        }
        let raw = std::path::Path::new(rel);
        if raw.is_absolute() && raw.exists() {
            return false;
        }
        true
    }

    /// Rewrite the trailing recovery hint in a head+tail-truncated tool
    /// result to indicate the spill file is no longer available.
    ///
    /// `original` is the persisted preview that ends with the
    /// `[The tool output was truncated to N KB. Full output saved to:
    ///  \`<rel>\`...]` block produced by
    /// `tool_output_truncate::build_preview`. We trim that trailing block
    /// (everything from the last `[The tool output was truncated to`
    /// occurrence onward) and append a short "expired" notice. When the
    /// marker is absent for any reason we just append the notice — the
    /// degradation is safe even if the original shape is unexpected.
    fn rewrite_hint_as_expired(original: &str, rel: &str) -> String {
        const MARKER: &str = "[The tool output was truncated to";
        let trimmed = match original.rfind(MARKER) {
            Some(idx) => original[..idx].trim_end_matches('\n').to_string(),
            None => original.trim_end_matches('\n').to_string(),
        };
        format!(
            "{trimmed}\n\n[The tool output was truncated. The full-output spill file \
             (`{rel}`) is no longer available — retention period has expired. Only \
             this preview survives.]\n"
        )
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
    use super::normalize;
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

    /// Regression test for #967 — legacy sessions persisted by the
    /// pre-fix code path stored a no-args Anthropic tool call as
    /// `Content::ToolCall { params: Value::String("") }`. Re-serializing
    /// that on the wire produced `tool_use.input: ""` which Anthropic
    /// rejects (`"Input should be an object"`), wedging the
    /// conversation across runs. The rebuild path now heals the poison
    /// so already-persisted sessions can recover without manual DB
    /// surgery.
    #[test]
    fn test_legacy_empty_string_params_heals_to_empty_object() {
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
            make_msg(Role::User, "list files"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                // The exact poison shape from #967.
                content: Content::ToolCall {
                    name: "fs_list".to_string(),
                    params: serde_json::Value::String(String::new()),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_poisoned"})),
            },
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "call_poisoned".to_string(),
                    result: serde_json::json!("[]"),
                },
                timestamp: Timestamp::now(),
                metadata: Some(serde_json::json!({"ok": true})),
            },
            make_msg(Role::Assistant, "ok done"),
        ];

        let messages = builder.build("System", &history, "and now?", None);

        let tc_msg = messages
            .iter()
            .find(|m| m.role == "assistant" && m.tool_calls.is_some())
            .expect("rebuilt assistant tool_call message must exist");
        let calls = tc_msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].function.arguments, "{}",
            "legacy `params: \"\"` poison must heal to `arguments: \"{{}}\"` so the \
             Anthropic adapter serializes `tool_use.input: {{}}` and not `\"\"`",
        );

        // And confirm the parse round-trip: reconstructed `arguments`
        // must parse back to a JSON object so subsequent normalize
        // passes are no-ops.
        let parsed: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert!(parsed.is_object());
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

        normalize::group_tool_calls(&mut messages);

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
            recent_window: 20,
            summary_interval: 30,
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
            recent_window: 100,
            summary_interval: 30,
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

        // system + episodic + sliding-summary + 3 recent + current = 7
        assert_eq!(messages.len(), 7);
        assert_eq!(messages[0].role, "system"); // main system prompt
        assert_eq!(messages[1].role, "system"); // episodic
        assert!(messages[1].content_str().contains("CORS"));
        assert_eq!(messages[2].role, "system"); // sliding-summary
        assert!(messages[2].content_str().contains("Context summary"));
        assert_eq!(messages.last().unwrap().role, "user"); // current input
        // Invariant: no mid-history system messages survive.
        assert!(messages[3..].iter().all(|m| m.role != "system"));
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
            recent_window: 20,
            summary_interval: 30,
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

    /// Helper: build an error-marker message matching the shape persisted
    /// by `gateway::runs::markers::persist_error_marker` (#874).
    fn make_error_marker(text: &str, status: &str, error: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "kind": "error",
                "type": "run_boundary",
                "status": status,
                "error": error,
            })),
        }
    }

    /// Run-failed error markers (#874) must reach the LLM context as a
    /// `user` message so a follow-up turn ("why did that fail?") gives the
    /// agent the error text without re-quoting. Without the
    /// `is_error_marker` rewrite in `session_msg_to_llm`, the standard
    /// `strip_mid_history_system_markers` pass would drop the marker and
    /// the agent would never see the prior failure.
    #[test]
    fn error_marker_survives_as_user_message() {
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
            make_msg(Role::User, "List files in /tmp"),
            make_error_marker(
                "(run failed) Anthropic 500: server error",
                "failed",
                "Anthropic 500: server error",
            ),
        ];

        let messages = builder.build(
            "You are a helpful agent.",
            &history,
            "why did that fail?",
            None,
        );

        // No mid-history system messages survive normalization.
        assert!(
            messages[1..].iter().all(|m| m.role != "system"),
            "no system message may appear after the system prefix"
        );

        // The error marker must reach the LLM as a user message tagged
        // with the [Error] prefix so the agent can answer the follow-up.
        let error_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content.as_deref().is_some_and(|s| {
                    s.contains("[Error]") && s.contains("Anthropic 500: server error")
                })
        });
        assert!(
            error_visible,
            "error marker must be rewritten to a `user` [Error] message so the LLM sees it; got: {:?}",
            messages
                .iter()
                .map(|m| (m.role.clone(), m.content.clone().unwrap_or_default()))
                .collect::<Vec<_>>()
        );

        // The trailing user-input invariant must hold: the user's actual
        // follow-up question is the last message.
        let last = messages.last().expect("non-empty");
        assert_eq!(last.role, "user");
        assert!(
            last.content
                .as_deref()
                .is_some_and(|s| s.contains("why did that fail?")),
            "trailing user message must be the fresh input, not the rewritten error marker"
        );
    }

    /// Cancellation markers (#874) follow the same rewrite path as
    /// run-failed markers — the `kind: "error"` flag is what matters,
    /// not the specific `status`/`error_kind` value.
    #[test]
    fn cancelled_marker_survives_as_user_message() {
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
            make_msg(Role::User, "Run a long-running task"),
            make_error_marker("(run cancelled)", "cancelled", "user cancelled"),
        ];

        let messages = builder.build("System.", &history, "try again", None);

        let cancelled_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Error]") && s.contains("(run cancelled)"))
        });
        assert!(
            cancelled_visible,
            "cancellation marker must be rewritten to [Error] user message"
        );
    }

    /// Non-error system markers (e.g. job notifications, completed
    /// run_boundary) must continue to be stripped — only `kind: "error"`
    /// markers get the rewrite. This protects the existing canonical
    /// invariant (no mid-history system messages reach the LLM).
    #[test]
    fn non_error_system_markers_still_stripped() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // A completed run_boundary marker (no `kind: "error"`) should be
        // stripped — only failed/cancelled markers carry `kind: "error"`.
        let completed_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("(run completed)".to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "type": "run_boundary",
                "status": "completed",
            })),
        };

        let history = vec![
            make_msg(Role::User, "hi"),
            make_msg(Role::Assistant, "hello"),
            completed_marker,
        ];

        let messages = builder.build("System.", &history, "next?", None);

        // The completed marker must NOT appear in the context.
        assert!(
            !messages.iter().any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("(run completed)"))
            }),
            "non-error system markers must continue to be stripped"
        );
    }

    // -- #912 follow-up: legacy duplicate run_boundary marker filter ---------

    /// Regression test for PR #930 follow-up F2 — Tim's "forward-only dedup"
    /// finding.  Pre-#912 SQLite DBs persist BOTH the runtime-layer
    /// `[Run failed: ...]` assistant bubble AND the lifecycle-layer
    /// `(run failed) ...` `kind: "error"` `type: "run_boundary"` system
    /// marker for every failed run.  #912 stops new failed runs from writing
    /// the marker, but existing DBs still surface both records during
    /// context reload.  This test pins the reconstruction-side filter that
    /// drops the duplicate marker so legacy DBs match new-DB display
    /// behaviour without a data migration.
    #[test]
    fn legacy_duplicate_run_boundary_marker_dropped_when_adjacent_to_runtime_bubble() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Mimic the exact pre-#912 SQLite shape: runtime-layer bubble at
        // index 1 (Role::Assistant), then lifecycle-layer marker at index
        // 2 (Role::System with kind: "error", type: "run_boundary",
        // status: "failed").  Both records carry the same conceptual
        // failure event.
        let history = vec![
            make_msg(Role::User, "do something"),
            make_msg(Role::Assistant, "[Run failed: Provider error]"),
            make_error_marker("(run failed) Provider error", "failed", "Provider error"),
        ];

        let messages = builder.build(
            "You are a helpful agent.",
            &history,
            "why did that fail?",
            None,
        );

        // The runtime-layer assistant bubble must survive as the
        // canonical record (Atlas + Alper's decision on #912).
        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run failed: Provider error]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "the runtime-layer `[Run failed: ...]` bubble must survive as the canonical record; got {bubble_count} occurrences in {messages:?}"
        );

        // The lifecycle-layer marker must NOT surface as a `[Error]
        // (run failed) ...` user message — pre-fix it would, because
        // `session_msg_to_llm` rewrites `kind: "error"` markers into
        // `[Error] ...` user messages BEFORE
        // `strip_mid_history_system_markers` runs.  Post-fix the
        // marker is dropped during context build, so the rewrite never
        // fires and the LLM sees only the bubble.
        let legacy_marker_visible = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|s| s.contains("[Error]") && s.contains("(run failed)"))
        });
        assert!(
            !legacy_marker_visible,
            "legacy lifecycle-layer marker must not surface to the LLM when an adjacent runtime-layer bubble carries the same event; messages: {messages:?}"
        );
    }

    /// Same legacy-duplicate test, but for the cancellation pair —
    /// `[Run cancelled by user]` runtime bubble + `(run cancelled)`
    /// `kind: "error"` `status: "cancelled"` lifecycle marker.  Mirrors
    /// the pre-#912 shape for the `Cancelled` and
    /// `CancelledWithToolCalls` arms.
    #[test]
    fn legacy_duplicate_cancelled_marker_dropped_when_adjacent_to_runtime_bubble() {
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
            make_msg(Role::User, "long-running task"),
            make_msg(Role::Assistant, "[Run cancelled by user]"),
            make_error_marker("(run cancelled)", "cancelled", "user cancelled"),
        ];

        let messages = builder.build("System.", &history, "try again", None);

        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run cancelled by user]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "the runtime-layer `[Run cancelled by user]` bubble must survive; got {bubble_count} in {messages:?}"
        );

        let legacy_marker_visible = messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|s| s.contains("[Error]") && s.contains("(run cancelled)"))
        });
        assert!(
            !legacy_marker_visible,
            "legacy cancellation marker must not surface to the LLM when an adjacent bubble exists; messages: {messages:?}"
        );
    }

    /// New (post-#912) DBs only have the runtime-layer bubble — no
    /// lifecycle-layer marker is written for the four removed call
    /// sites.  The filter must be a no-op on this shape: the bubble
    /// reaches the LLM as a regular conversation turn, and the agent
    /// can answer "why did that fail?" using its content directly (no
    /// `[Error]` rewrite needed because there is no marker to rewrite).
    #[test]
    fn post_912_history_with_only_runtime_bubble_unchanged_by_filter() {
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
            make_msg(Role::User, "do something"),
            make_msg(Role::Assistant, "[Run failed: Provider error]"),
        ];

        let messages = builder.build("System.", &history, "why?", None);

        let bubble_count = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Run failed: Provider error]"))
            })
            .count();
        assert_eq!(
            bubble_count, 1,
            "post-#912 history must keep the single runtime-layer bubble untouched"
        );
    }

    /// `runtime_init_error` markers (`type: "runtime_init_error"`)
    /// fire when `AgentRuntime::new()` itself fails — strictly before
    /// `finish_run` could possibly run, so they have NO runtime-layer
    /// counterpart.  The legacy-duplicate filter must NOT match these
    /// markers (matching them would drop the only error record for
    /// runtime-construction failures).  This is the negative-space
    /// guard for `is_legacy_lifecycle_error_marker`.
    #[test]
    fn runtime_init_error_marker_not_matched_by_legacy_dedup() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        // Mimic the shape persisted by lifecycle.rs's
        // runtime_init_error path (kept by #912).  No runtime-layer
        // bubble is present because the runtime never started.
        let init_error_marker = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("(runtime initialization failed) Provider error".to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "synthetic": true,
                "kind": "error",
                "type": "runtime_init_error",
                "status": "failed",
                "error": "Provider error",
                "error_kind": "runtime_init",
            })),
        };

        let history = vec![make_msg(Role::User, "first turn"), init_error_marker];

        let messages = builder.build("System.", &history, "follow-up", None);

        // The init-error marker must surface to the LLM as a `[Error]
        // ...` user message (existing #874 behaviour) — the legacy
        // dedup filter must NOT have touched it because its `type` is
        // `runtime_init_error`, not `run_boundary`.
        let init_error_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content.as_deref().is_some_and(|s| {
                    s.contains("[Error]") && s.contains("runtime initialization failed")
                })
        });
        assert!(
            init_error_visible,
            "runtime_init_error marker must still reach the LLM — it has no runtime-layer counterpart; messages: {messages:?}"
        );
    }

    /// A bare lifecycle-layer marker with NO adjacent runtime-layer
    /// bubble (e.g. corrupted DB, partial write, future code path that
    /// re-introduces the marker without a bubble) must be kept — the
    /// dedup filter only fires on the duplicate pattern, not on every
    /// `kind: "error"` `type: "run_boundary"` marker it sees.  This
    /// preserves the agent's ability to answer "why did that fail?"
    /// even when the runtime-layer write was lost.
    #[test]
    fn bare_run_boundary_marker_without_adjacent_bubble_kept() {
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
            make_msg(Role::User, "do something"),
            // A bare lifecycle-layer marker with NO preceding bubble.
            // Not the duplicate pattern — the filter must keep it.
            make_error_marker("(run failed) lonely marker", "failed", "lonely error"),
        ];

        let messages = builder.build("System.", &history, "why?", None);

        let marker_visible = messages.iter().any(|m| {
            m.role == "user"
                && m.content
                    .as_deref()
                    .is_some_and(|s| s.contains("[Error]") && s.contains("lonely marker"))
        });
        assert!(
            marker_visible,
            "a bare run_boundary marker without an adjacent bubble must still surface as `[Error] ...` so the agent can answer follow-ups; messages: {messages:?}"
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

        normalize::strip_orphaned_tool_results(&mut messages);

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

        normalize::strip_orphaned_tool_results(&mut messages);

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

        normalize::strip_orphaned_tool_results(&mut messages);

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

        normalize::strip_orphaned_tool_results(&mut messages);

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

    // -- Canonical message-shape invariant (#586) -------------------------
    //
    // These tests pin the guarantees enforced by `normalize_for_llm` so
    // the Anthropic / OpenAI adapters can trust the shape of the message
    // list without re-running per-provider sanity passes.

    fn invariant_config() -> ContextConfig {
        ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32_000,
            recent_window: 50,
            summary_interval: 30,
            summary_model: None,
            ..Default::default()
        }
    }

    fn assert_invariant(messages: &[LlmMessage]) {
        assert!(!messages.is_empty(), "messages must be non-empty");
        let first_non_system = messages
            .iter()
            .position(|m| m.role != "system")
            .expect("at least one non-system message required");
        assert!(
            !messages[first_non_system..]
                .iter()
                .any(|m| m.role == "system"),
            "no mid-history system message may survive"
        );
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "last message must be user"
        );
        assert!(
            !messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|s| s.is_empty())
                    && m.tool_calls.is_none()
                    && m.tool_call_id.is_none()),
            "no empty-content messages allowed"
        );
    }

    #[test]
    fn test_normalize_empty_input_synthesizes_trailing_user() {
        // History has a user message already — the build with empty input
        // ends with a user turn naturally. No synthesis needed, but the
        // invariant still must hold.
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![make_msg(Role::User, "ping")];
        let messages = builder.build("sys", &history, "", None);
        assert_invariant(&messages);
        assert_eq!(messages.last().unwrap().content_str(), "ping");
    }

    #[test]
    fn test_normalize_empty_input_assistant_tail_gets_placeholder() {
        // History ends with assistant text and current input is empty —
        // normalize must append a placeholder user turn.
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg(Role::User, "do X"),
            make_msg(Role::Assistant, "done"),
        ];
        let messages = builder.build("sys", &history, "", None);
        assert_invariant(&messages);
        assert_eq!(
            messages.last().unwrap().role,
            "user",
            "assistant-tail history must gain a trailing user placeholder"
        );
        // Synthesised placeholders are short and recognisable.
        assert!(
            messages
                .last()
                .unwrap()
                .content_str()
                .contains("Please continue"),
            "placeholder should use the CONTINUE_PLACEHOLDER stub; got: {:?}",
            messages.last().unwrap().content
        );
    }

    #[test]
    fn test_normalize_empty_input_pending_tool_calls_closes_with_synthetic_result() {
        // An assistant tool_use whose matching tool_result is missing
        // from history must be closed with a synthetic
        // `[Tool execution was interrupted]` result before the trailing-
        // user synthesis runs.
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg(Role::User, "run echo"),
            Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "echo".to_string(),
                    params: serde_json::json!({}),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "call_pending"})),
            },
            // No matching Role::Tool result persisted (simulates crash/cancel).
        ];

        let messages = builder.build("sys", &history, "", None);
        assert_invariant(&messages);

        // The synthetic interrupted tool_result must exist paired with call_pending.
        let synth = messages
            .iter()
            .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call_pending"))
            .expect("synthetic interrupted tool_result must be injected");
        assert!(
            synth.content_str().contains("interrupted"),
            "synthetic result must use INTERRUPTED_TOOL_RESULT wording"
        );
    }

    #[test]
    fn test_normalize_strips_mid_history_system_markers() {
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg(Role::User, "hi"),
            make_msg(Role::Assistant, "hello"),
            // Synthetic mid-history marker (lifecycle event).
            make_msg(Role::System, "[DM conversation ended]"),
            make_msg(Role::User, "what happened?"),
        ];
        let messages = builder.build("sys", &history, "", None);
        assert_invariant(&messages);
        assert!(
            messages[1..].iter().all(|m| m.role != "system"),
            "mid-history system markers must be stripped"
        );
        // User content preserved.
        assert!(messages.iter().any(|m| m.content_str() == "hi"));
        assert!(messages.iter().any(|m| m.content_str() == "what happened?"));
    }

    #[test]
    fn test_normalize_merges_consecutive_user_messages() {
        // Direct helper test so we isolate the merge logic from the rest
        // of the builder (which pushes system + current_input around it).
        let mut messages = vec![
            LlmMessage::user("first"),
            LlmMessage::user("second"),
            LlmMessage::assistant("reply"),
            LlmMessage::user("third"),
        ];
        normalize::merge_consecutive_same_role(&mut messages);
        assert_eq!(messages.len(), 3, "two consecutive users must merge to one");
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content_str().contains("first"));
        assert!(messages[0].content_str().contains("second"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn test_normalize_preserves_system_prefix_block() {
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg(Role::User, "hi"),
            make_msg(Role::Assistant, "hello"),
        ];
        let messages = builder.build_with_perspective(
            "main system prompt",
            &history,
            "follow-up",
            None,
            None,
            Some("prior session summary"),
        );
        assert_invariant(&messages);
        // Two system messages at the head, then no more.
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "system");
        assert_eq!(messages[2].role, "user");
        assert!(messages[2..].iter().all(|m| m.role != "system"));
    }

    #[test]
    fn test_normalize_drops_empty_content_messages() {
        // Direct helper test: empty-body assistant/user turns get dropped
        // but tool-call-only assistant and tool-role messages survive.
        let mut messages = vec![
            LlmMessage::user("hi"),
            LlmMessage::assistant(""),
            LlmMessage::user(""),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("c1", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", "result"),
            LlmMessage::user("thanks"),
        ];
        normalize::drop_empty_content_messages(&mut messages);
        assert_eq!(
            messages.len(),
            4,
            "empty-body user/assistant must be dropped"
        );
        assert_eq!(messages[0].content_str(), "hi");
        assert!(messages[1].tool_calls.is_some());
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[3].content_str(), "thanks");
    }

    #[test]
    fn test_anthropic_adapter_trusts_canonical_shape() {
        // Feed the Anthropic adapter a message list in canonical shape
        // (system prefix + alternating + trailing user + no empties) and
        // verify system extraction + trailing user role + non-empty array.
        use crate::anthropic::to_anthropic_request;
        let req = crate::llm_types::CompletionRequest::new("claude-sonnet").with_messages(vec![
            LlmMessage::system("You are helpful."),
            LlmMessage::user("hi"),
            LlmMessage::assistant("hello"),
            LlmMessage::user("what time is it?"),
        ]);
        let areq = to_anthropic_request(&req);
        assert_eq!(
            areq.system.as_ref().and_then(|s| s.as_text()),
            Some("You are helpful."),
            "single system prefix must be extracted to top-level system"
        );
        assert!(
            !areq.messages.is_empty(),
            "adapter must never emit an empty messages array"
        );
        assert_eq!(
            areq.messages.last().map(|m| m.role.as_str()),
            Some("user"),
            "adapter post-condition: trailing user turn"
        );
    }

    #[test]
    fn test_anthropic_adapter_never_sends_empty_messages_array() {
        // Regression: even a degenerate history (just a user turn) must
        // still produce a non-empty messages array after system extraction.
        let builder = ContextBuilder::new(invariant_config());
        let history: Vec<Message> = Vec::new();
        let messages = builder.build("sys", &history, "hello", None);
        assert_invariant(&messages);
        let req = crate::llm_types::CompletionRequest::new("claude-sonnet").with_messages(messages);
        let areq = crate::anthropic::to_anthropic_request(&req);
        assert!(!areq.messages.is_empty());
        assert_eq!(areq.messages.last().map(|m| m.role.as_str()), Some("user"));
    }

    #[test]
    fn test_dm_perspective_then_normalize() {
        // DM session with reasoning filter + perspective mapping + normalize.
        // The perspective mapping can break alternation (two adjacent user
        // messages post-map); normalize must merge them and still end with
        // a user turn.
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg_with_meta(
                Role::User,
                Content::Text("hi from alice".to_string()),
                serde_json::json!({"from_agent": "alice", "message_type": "dm"}),
            ),
            make_msg_with_meta(
                Role::User,
                Content::Text("one more thing from alice".to_string()),
                serde_json::json!({"from_agent": "alice", "message_type": "dm"}),
            ),
        ];
        let messages = builder.build_with_perspective("sys", &history, "", None, Some("bob"), None);
        assert_invariant(&messages);
        // Two alice messages collapse into one user turn; no placeholder
        // synthesis needed because the tail is already user.
        let user_turns = messages.iter().filter(|m| m.role == "user").count();
        assert_eq!(
            user_turns, 1,
            "two adjacent alice messages must merge into one user turn"
        );
    }

    /// Extends the original notification test: the invariant must hold even
    /// when the `notification_input` message was never persisted (lifecycle
    /// failure — e.g. the append_message before run_on_session errored).
    #[test]
    fn test_notification_run_context_ends_with_user_even_without_notification_input() {
        let builder = ContextBuilder::new(invariant_config());
        let history = vec![
            make_msg(Role::User, "please message Bob"),
            make_msg(Role::Assistant, "I'll message Bob."),
            // A synthetic Role::System marker (lifecycle marker) landed
            // but the notification_input Role::User was NOT persisted.
            {
                let mut marker = make_msg(Role::System, "[DM conversation ended]");
                marker.metadata = Some(serde_json::json!({
                    "synthetic": true,
                    "type": "dm_ended_notification",
                }));
                marker
            },
        ];
        let messages = builder.build("sys", &history, "", None);
        assert_invariant(&messages);
        // Marker stripped, assistant tail detected, placeholder synthesised.
        assert_eq!(messages.last().unwrap().role, "user");
        assert!(messages[1..].iter().all(|m| m.role != "system"));
    }

    // -- #851: in-loop truncation interaction with session_msg_to_llm ---------

    /// When a tool result message carries `truncated_in_loop: true` in its
    /// metadata, `session_msg_to_llm` must NOT apply its legacy 2000-byte
    /// re-truncation — the persisted bytes ARE the bytes the live agent
    /// saw, including the spill-path hint, and re-truncating would shred
    /// the recovery instructions.
    #[test]
    fn session_msg_to_llm_skips_re_truncation_when_in_loop_flag_set() {
        let builder = default_builder();
        let preview = format!("preview head\n{}\npreview tail", "x".repeat(3000));
        // Sanity: the preview is well above 2000 bytes so the legacy path
        // would trigger.
        assert!(preview.len() > 2000);

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": ".alms/tool-output/run1/tool_call_abc.txt",
                "original_bytes": 100_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        // The serialised tool-result string must contain the full preview
        // body — no `... [truncated, N bytes total]` suffix.
        assert!(
            !llm.content_str().contains("[truncated,"),
            "in-loop-truncated message must not be re-truncated by session_msg_to_llm"
        );
        assert!(
            llm.content_str().len() > 2000,
            "preview must survive past the legacy 2000-byte cap"
        );
    }

    /// Symmetric counterpart: when the in-loop flag is absent, the legacy
    /// 2000-byte re-truncation still fires. This pins the
    /// backward-compatibility guarantee for tool result messages persisted
    /// before #851 (or by deployments where the in-loop service is
    /// disabled in `alms.toml`).
    #[test]
    fn session_msg_to_llm_still_truncates_when_flag_absent() {
        let builder = default_builder();
        let big = "x".repeat(5000);

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(big.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        assert!(
            body.contains("[truncated,"),
            "legacy path must still truncate when truncated_in_loop is absent"
        );
        // 2000-byte preview + suffix string -> stays under, say, 2500
        // bytes for the entire message.
        assert!(
            body.len() < 2500,
            "legacy path caps at 2000 bytes plus suffix"
        );
    }

    // -- #921 review fix #3: stale spill_path detection -----------------------

    /// When a tool-result message references a `spill_path` that has been
    /// swept (>7d retention), `session_msg_to_llm` must rewrite the
    /// recovery hint to indicate the file is no longer available so the
    /// LLM doesn't try to `fs_read` an ENOENT path.
    #[test]
    fn session_msg_to_llm_rewrites_hint_when_spill_file_missing() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let builder =
            default_builder().with_workspace_root(Some(workspace_dir.path().to_path_buf()));

        // Persisted preview with the full hint text from `build_preview`.
        let preview = format!(
            "head data\n\n... [50000 bytes / 1 lines omitted] ...\n\ntail data\n\n[The tool output \
             was truncated to 32 KB. Full output saved to: `{rel}` (50000 bytes, 1 lines). Use \
             `fs_grep` to search the full content or `fs_read` with `offset`/`limit` to view \
             specific sections.]\n",
            rel = "tool-output/run1/tool_call_abc.txt"
        );

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                // This relative path does NOT exist under workspace_dir.
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        // The original "Use `fs_grep`..." hint must be gone.
        assert!(
            !body.contains("Use `fs_grep`"),
            "stale fs_grep hint must be removed when spill is missing"
        );
        // The replacement notice must mention "retention" and "expired".
        assert!(
            body.contains("retention period has expired"),
            "expired hint must be present: {body}"
        );
        // The head/tail body itself must survive the rewrite.
        assert!(body.contains("head data"));
        assert!(body.contains("tail data"));
    }

    /// Symmetric counterpart: when the spill file IS still present on
    /// disk, the recovery hint must remain intact so the agent can
    /// `fs_read` it normally.
    #[test]
    fn session_msg_to_llm_keeps_hint_when_spill_file_present() {
        let workspace_dir = tempfile::tempdir().unwrap();
        // Materialise the spill file under workspace_dir so the existence
        // check passes.
        let spill_dir = workspace_dir.path().join("tool-output").join("run1");
        std::fs::create_dir_all(&spill_dir).unwrap();
        let spill_file = spill_dir.join("tool_call_abc.txt");
        std::fs::write(&spill_file, b"original payload").unwrap();

        let builder =
            default_builder().with_workspace_root(Some(workspace_dir.path().to_path_buf()));

        let preview = format!(
            "head data\n\n... omitted ...\n\ntail data\n\n[The tool output was truncated to 32 KB. \
             Full output saved to: `{rel}` (50000 bytes, 1 lines). Use `fs_grep` to search the full \
             content or `fs_read` with `offset`/`limit` to view specific sections.]\n",
            rel = "tool-output/run1/tool_call_abc.txt"
        );

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        let body = llm.content_str();
        // The recovery hint must be intact.
        assert!(
            body.contains("Use `fs_grep`"),
            "fs_grep hint must survive when spill file exists"
        );
        // The expired notice must NOT appear.
        assert!(!body.contains("retention period has expired"));
    }

    /// Without a workspace root configured the builder cannot resolve
    /// relative spill paths, so it MUST leave the hint unchanged (graceful
    /// degradation: the agent may try `fs_read` and fail, but the LLM is
    /// not given an inaccurate "expired" notice).
    #[test]
    fn session_msg_to_llm_leaves_hint_alone_without_workspace_root() {
        let builder = default_builder(); // no workspace_root

        let preview =
            "head\n\n... omitted ...\n\ntail\n\n[The tool output was truncated to 32 KB. \
             Full output saved to: `tool-output/run1/tool_call_abc.txt` (50000 bytes, 1 lines). \
             Use `fs_grep` to search the full content or `fs_read` with `offset`/`limit` to view \
             specific sections.]\n"
                .to_string();

        let msg = Message {
            id: "m1".to_string(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "call_abc".to_string(),
                result: serde_json::Value::String(preview.clone()),
            },
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "ok": true,
                "tool_invocation_id": "abc",
                "truncated_in_loop": true,
                "spill_path": "tool-output/run1/tool_call_abc.txt",
                "original_bytes": 50_000,
                "original_lines": 1,
            })),
        };

        let llm = builder.session_msg_to_llm(&msg);
        // Without a workspace root the existence check is skipped, so the
        // hint must survive intact — including the spill path reference
        // and the `Use \`fs_grep\`` recovery instruction. (The session
        // round-trip JSON-encodes the inner string, so we only check that
        // the original recovery hint substring is present, not byte
        // equality with the bare preview.)
        let body = llm.content_str();
        assert!(
            body.contains("Use `fs_grep`"),
            "fs_grep hint must survive when workspace_root is unset: {body}"
        );
        assert!(
            !body.contains("retention period has expired"),
            "expired notice must NOT appear when workspace_root is unset"
        );
    }
}
