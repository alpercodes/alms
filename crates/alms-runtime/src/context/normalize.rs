// SPDX-License-Identifier: Apache-2.0

//! Canonical message-shape pipeline for the context builder (issue #586).
//!
//! Pure free functions — no `&self`, no shared state. The pipeline runs as
//! the final stage of [`super::ContextBuilder::build_with_perspective`] and
//! produces the canonical shape documented at the top of [`super`]. Provider
//! adapters trust this shape at the `Vec<LlmMessage>` layer; any wire-level
//! quirks (e.g. Anthropic's `tool` -> `user` relabel) are the adapter's job.
//!
//! The orchestrator in [`super`] calls [`group_tool_calls`],
//! [`strip_orphaned_tool_results`], and finally [`normalize_for_llm`] in
//! that order; the ordering is load-bearing and the call sites in the
//! orchestrator carry the WHY of each step. The five sub-passes that
//! [`normalize_for_llm`] composes are private to this module — callers
//! should always go through the umbrella `normalize_for_llm` entry point.

use crate::llm_types::LlmMessage;
use tracing::{debug, warn};

/// Placeholder body used when closing an assistant tool_use whose matching
/// tool_result was not persisted (crash, cancel, truncation slicing across
/// the pair). Matches conventional agent-tool wording so cross-tool
/// logs correlate.
const INTERRUPTED_TOOL_RESULT: &str = "[Tool execution was interrupted]";

/// Placeholder body used when synthesising a trailing user turn for runs
/// that arrived with empty fresh input AND whose history does not end in a
/// user turn (e.g. a notification run where the `notification_input`
/// message was never persisted).
const CONTINUE_PLACEHOLDER: &str = "Please continue.";

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
pub(super) fn strip_orphaned_tool_results(messages: &mut Vec<LlmMessage>) {
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
pub(super) fn group_tool_calls(messages: &mut Vec<LlmMessage>) {
    let mut grouped: Vec<LlmMessage> = Vec::with_capacity(messages.len());

    for msg in messages.drain(..) {
        let is_tool_call_msg =
            msg.role == "assistant" && msg.tool_calls.is_some() && msg.content.is_none();

        if is_tool_call_msg {
            // Try to merge with the previous message if it's also an
            // assistant tool-call-only message.
            if let Some(prev) = grouped.last_mut() {
                let prev_is_tool_call =
                    prev.role == "assistant" && prev.tool_calls.is_some() && prev.content.is_none();

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

/// Enforce the canonical message-shape invariant documented at the top
/// of the parent module. Runs as the final step of
/// [`super::ContextBuilder::build_with_perspective`] after
/// [`group_tool_calls`] and the optional fresh-input push.
///
/// `has_fresh_input` is used only as a hint for the placeholder body:
/// when `false` we know the caller expects the agent to react to
/// whatever already sits in history (notification run, resumed run),
/// which shapes the log-level of any synthesis warning.
pub(super) fn normalize_for_llm(messages: &mut Vec<LlmMessage>, has_fresh_input: bool) {
    strip_mid_history_system_markers(messages);
    drop_empty_content_messages(messages);
    merge_consecutive_same_role(messages);
    close_pending_tool_calls(messages);
    ensure_trailing_user(messages, has_fresh_input);

    // Hard invariant assertions. These are defence against future
    // regressions in any of the individual passes above — they can all
    // fire a real bug without producing a wire-level rejection, which
    // is exactly what #586 exists to prevent.
    debug_assert!(
        !messages.is_empty(),
        "normalize: emitted empty message list"
    );
    let first_non_system = messages.iter().position(|m| m.role != "system");
    debug_assert!(
        first_non_system.is_some(),
        "normalize: no non-system message remains"
    );
    if let Some(idx) = first_non_system {
        debug_assert!(
            !messages[idx..].iter().any(|m| m.role == "system"),
            "normalize: system message remains after first non-system"
        );
        debug_assert_eq!(
            messages.last().map(|m| m.role.as_str()),
            Some("user"),
            "normalize: last message is not user"
        );
    }
}

/// Strip `role = "system"` messages that appear AFTER the first non-
/// system message. These are synthetic lifecycle markers (DM-ended,
/// job completion, subagent notifications) persisted to session
/// history by `persist_lifecycle_marker`. They are UI/SSE artefacts
/// and do not belong in the LLM context — the agent already gets the
/// payload via the `notification_input` user message (see
/// `gateway::runs::lifecycle`), and Anthropic's system-field
/// extraction would lift them out of order anyway.
fn strip_mid_history_system_markers(messages: &mut Vec<LlmMessage>) {
    let first_non_system = match messages.iter().position(|m| m.role != "system") {
        Some(idx) => idx,
        None => return,
    };

    let before = messages.len();
    let mut i = first_non_system;
    while i < messages.len() {
        if messages[i].role == "system" {
            messages.remove(i);
        } else {
            i += 1;
        }
    }

    let stripped = before - messages.len();
    if stripped > 0 {
        debug!(
            stripped,
            "Stripped mid-history system message(s) from LLM context"
        );
    }
}

/// Drop messages whose content is empty AND carry no structural payload
/// (no tool_calls, no tool_call_id). Empty-body assistant/user turns
/// either arose from upstream bugs or from a reasoning-only model that
/// produced no output; either way Anthropic and Bedrock reject them
/// (conventional provider hygiene) and no provider benefits from them.
pub(super) fn drop_empty_content_messages(messages: &mut Vec<LlmMessage>) {
    let before = messages.len();
    messages.retain(|m| {
        let has_text = m.content.as_deref().is_some_and(|s| !s.is_empty());
        let has_tool_calls = m.tool_calls.as_ref().is_some_and(|c| !c.is_empty());
        let has_tool_result = m.role == "tool" && m.tool_call_id.is_some();
        has_text || has_tool_calls || has_tool_result
    });
    let dropped = before - messages.len();
    if dropped > 0 {
        debug!(dropped, "Dropped empty-content message(s) from LLM context");
    }
}

/// Merge consecutive same-role user or assistant text messages. Leaves
/// tool-call/tool-result pairings alone: an assistant-with-`tool_calls`
/// is not merged into an adjacent assistant-text, and tool-role
/// messages are never merged (each carries its own tool_call_id).
///
/// Needed because interleaved DM reasoning + peer inbound + DM
/// perspective mapping can leave two adjacent user messages for the
/// same turn — Anthropic rejects that shape, OpenAI tolerates it but
/// some models respond poorly.
pub(super) fn merge_consecutive_same_role(messages: &mut Vec<LlmMessage>) {
    let mut merged: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    for msg in messages.drain(..) {
        let can_merge = match merged.last() {
            Some(prev) if prev.role == msg.role => match msg.role.as_str() {
                "user" | "assistant" => {
                    // Only merge pure-text messages; preserve tool_calls
                    // groupings (already handled by group_tool_calls).
                    prev.tool_calls.is_none() && msg.tool_calls.is_none()
                }
                _ => false,
            },
            _ => false,
        };

        if can_merge {
            let prev = merged.last_mut().expect("can_merge implies non-empty");
            let combined = match (prev.content.as_deref(), msg.content.as_deref()) {
                (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => {
                    format!("{a}\n\n{b}")
                }
                (Some(a), _) if !a.is_empty() => a.to_string(),
                (_, Some(b)) if !b.is_empty() => b.to_string(),
                _ => String::new(),
            };
            prev.content = Some(combined);
        } else {
            merged.push(msg);
        }
    }
    *messages = merged;
}

/// Close assistant tool_use entries whose matching tool_result was
/// never persisted (crash, cancel, truncation slicing mid-pair) with
/// synthetic [`INTERRUPTED_TOOL_RESULT`] messages. Mirrors the conventional
/// `message-v2.ts:804-814` — necessary because Anthropic requires
/// every `tool_use` to be followed by a matching `tool_result` in the
/// next user turn.
///
/// Runs AFTER [`strip_orphaned_tool_results`] (which handled the
/// inverse case — tool_results with no preceding tool_use) so the two
/// passes compose without interfering.
fn close_pending_tool_calls(messages: &mut Vec<LlmMessage>) {
    use std::collections::HashSet;

    // Collect all tool_call_ids that appear as tool_result messages.
    let resolved: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    // Walk through the message list and, for each assistant-with-
    // tool_calls, find tool_calls that never got a matching tool_result
    // and append synthetic interrupted results immediately after the
    // assistant message.
    let mut i = 0;
    let mut synthesised = 0usize;
    while i < messages.len() {
        let pending: Vec<String> = match &messages[i].tool_calls {
            Some(calls) if messages[i].role == "assistant" => calls
                .iter()
                .filter(|c| !resolved.contains(&c.id))
                .map(|c| c.id.clone())
                .collect(),
            _ => Vec::new(),
        };

        if pending.is_empty() {
            i += 1;
            continue;
        }

        // Check which of the pending ids are ALREADY closed by
        // immediately-following tool messages in the current array
        // (handles the case where later iterations already inserted
        // synthetic results).
        let mut still_open: Vec<String> = Vec::new();
        for id in &pending {
            let mut closed = false;
            for m in messages.iter().skip(i + 1) {
                if m.role == "tool" && m.tool_call_id.as_deref() == Some(id.as_str()) {
                    closed = true;
                    break;
                }
                // Stop scanning once we hit a non-tool message.
                if m.role != "tool" {
                    break;
                }
            }
            if !closed {
                still_open.push(id.clone());
            }
        }

        // Insert synthetic tool_results directly after the assistant
        // message, preserving the order of the original tool_calls.
        for (offset, id) in still_open.iter().enumerate() {
            messages.insert(
                i + 1 + offset,
                LlmMessage::tool_result(id.clone(), INTERRUPTED_TOOL_RESULT),
            );
            synthesised += 1;
        }
        i += 1 + still_open.len();
    }

    if synthesised > 0 {
        warn!(
            synthesised,
            "Closed {synthesised} pending tool_call(s) with synthetic \
             interrupted result(s) — upstream crash/cancel/truncation \
             left tool_use without matching tool_result"
        );
    }
}

/// Ensure the last non-system message has role `user`. Called after
/// `close_pending_tool_calls` so any assistant-with-tool_calls tail
/// has already been paired with tool_results.
///
/// If the tail is already a `user` message (the common case — fresh
/// input or a pre-persisted `notification_input` payload), do nothing.
/// Otherwise append [`CONTINUE_PLACEHOLDER`] as a fresh user turn so
/// the invariant holds. `has_fresh_input` shapes only the log level
/// of the warning emitted in the placeholder path — upstream should
/// normally supply either fresh input or a persisted notification
/// message, so arriving in the synthesis branch is a real (if
/// recoverable) signal.
fn ensure_trailing_user(messages: &mut Vec<LlmMessage>, has_fresh_input: bool) {
    // Find the last non-system message. If none exists, we must emit
    // a placeholder so the invariant holds — this is the "all system"
    // degenerate case.
    let last_non_system_role = messages
        .iter()
        .rev()
        .find(|m| m.role != "system")
        .map(|m| m.role.clone());

    match last_non_system_role.as_deref() {
        Some("user") => {}
        Some(_) | None => {
            // warn! flagged — upstream should normally supply either
            // fresh input or a persisted `notification_input` message.
            // Arriving here means neither was present, which is a
            // real (if recoverable) signal.
            if has_fresh_input {
                // Fresh input was pushed but did not end up as user.
                // That should never happen; tests will catch it.
                warn!(
                    "normalize: fresh input present but tail is not user — \
                     appending placeholder to preserve invariant"
                );
            } else {
                warn!(
                    "normalize: no trailing user turn in context — \
                     synthesising placeholder. Check that the caller \
                     supplied fresh input or a persisted notification_input \
                     message."
                );
            }
            messages.push(LlmMessage::user(CONTINUE_PLACEHOLDER));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ContextBuilder;
    use super::super::tests::{make_msg, make_msg_with_meta};
    use super::*;
    use crate::llm_types::{LlmMessage, ToolCall};
    use alms_core::config::ContextConfig;
    use alms_session::{Content, Role};

    fn invariant_config() -> ContextConfig {
        ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 32_000,
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

        group_tool_calls(&mut messages);

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

        strip_orphaned_tool_results(&mut messages);

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

        strip_orphaned_tool_results(&mut messages);

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

        strip_orphaned_tool_results(&mut messages);

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

        strip_orphaned_tool_results(&mut messages);

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
    ///
    /// #869: pre-#869 this test pinned the truncation point at
    /// `recent_window = 2`. The recent_window cap is gone; we now use a
    /// tiny `max_input_tokens` (1050: ~50 tokens of history budget after
    /// the 1000-token reserved buffer) so the budget walk drops the
    /// older messages and keeps only the (tool_result, assistant-text)
    /// tail, exercising the same orphan-at-head shape as before.
    #[test]
    fn test_build_never_leaves_tool_result_at_head_after_truncation() {
        let config = ContextConfig {
            strategy: "truncate".into(),
            max_input_tokens: 1050,
            summary_model: None,
            ..Default::default()
        };
        let builder = ContextBuilder::new(config);

        let history = vec![
            // --- token-budget walk drops these older entries first ---
            make_msg(
                Role::User,
                "please write the file with a fairly long description that consumes tokens",
            ),
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: Role::Assistant,
                content: Content::ToolCall {
                    name: "fs_write".to_string(),
                    params: serde_json::json!({
                        "path": "/tmp/x",
                        "content": "padded payload to push token usage past the budget",
                    }),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"tool_call_id": "functions_fs_write_4"})),
            },
            // --- kept by the budget walk ---
            alms_session::Message {
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
            alms_session::Message {
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
        merge_consecutive_same_role(&mut messages);
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
        drop_empty_content_messages(&mut messages);
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
        let history: Vec<alms_session::Message> = Vec::new();
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
}
