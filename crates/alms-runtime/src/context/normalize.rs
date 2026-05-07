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
