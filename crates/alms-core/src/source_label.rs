// SPDX-License-Identifier: Apache-2.0

//! Source label derivation from `context_id` strings.
//!
//! A `context_id` encodes the origin of a session (web chat, Telegram, DM,
//! scheduled job, etc.).  [`derive_source_label`] parses the prefix to produce
//! a human-readable [`SourceLabel`] or `None` for internal session types that
//! should be excluded from user-facing summaries.

/// Source classification derived from a `context_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLabel {
    /// Machine-readable type: "web", "telegram", "dm", "job", etc.
    pub source_type: String,
    /// Human-readable label shown in summaries: "User chat", "DM from bob", etc.
    pub source_label: String,
}

/// Derive a human-readable source label from a `context_id`.
///
/// `agent_name` is the name of the current agent -- used to determine the peer
/// in DM sessions (format: `dm:{name1}:{name2}`, alphabetically sorted).
///
/// Returns `None` for session types that should be excluded from summary
/// generation (subagent and episodic sessions).  DM sessions are included
/// with a label like "DM with {peer_name}".
pub fn derive_source_label(context_id: &str, agent_name: &str) -> Option<SourceLabel> {
    // DM sessions: "dm:{name1}:{name2}" (alphabetically sorted).
    // Determine the peer by finding the name that isn't ours.
    if context_id.starts_with("dm:") {
        if let Some(peer) = crate::dm_peer(context_id, agent_name) {
            return Some(SourceLabel {
                source_type: "dm".into(),
                source_label: format!("DM with {peer}"),
            });
        }
        // Malformed or agent not a participant -- exclude to be safe.
        return None;
    }

    // Subagent sessions -- excluded
    if context_id.starts_with("subagent_") {
        return None;
    }

    // Episodic sessions -- excluded (internal)
    if context_id.starts_with("episodic:") {
        return None;
    }

    // Notification sessions: "notifications:{agent_name}" -- excluded (internal)
    if context_id.starts_with("notifications:") {
        return None;
    }

    // Telegram sessions: "telegram_{agent}_{chatid}"
    if context_id.starts_with("telegram_") {
        return Some(SourceLabel {
            source_type: "telegram".into(),
            source_label: "Telegram chat".into(),
        });
    }

    // Job sessions: "job_{jobid}"
    if let Some(job_part) = context_id.strip_prefix("job_") {
        let label = if job_part.len() > 40 {
            let truncated = truncate_to_char_boundary(job_part, 37);
            format!("Scheduled job: {truncated}...")
        } else {
            format!("Scheduled job: {job_part}")
        };
        return Some(SourceLabel {
            source_type: "job".into(),
            source_label: label,
        });
    }

    // Default: web/API chat
    Some(SourceLabel {
        source_type: "web".into(),
        source_label: "User chat".into(),
    })
}

/// Truncate a string to at most `max` bytes, respecting UTF-8 char boundaries.
///
/// If the string is already `max` bytes or shorter it is returned unchanged.
/// Otherwise the largest char boundary at or below `max` is found so that the
/// result is always valid UTF-8.
pub fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find the largest char boundary <= max.
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

/// Keep at most the **last** `max` bytes of `s`, respecting UTF-8 char
/// boundaries.
///
/// The tail-anchored counterpart of [`truncate_to_char_boundary`]: that one
/// keeps the *oldest* `max` bytes, this one keeps the *newest*. Which end to
/// cut is a property of the data, not of the cap — for a log-shaped file that
/// only ever grows at the end (an append-only `memories.md`, #1308), a
/// head-anchored window freezes on the oldest content and never shows anything
/// written since.
///
/// If the string is already `max` bytes or shorter it is returned unchanged.
/// Otherwise the smallest char boundary at or above `s.len() - max` is used, so
/// the result is always valid UTF-8 and never *longer* than `max` bytes —
/// mirroring how [`truncate_to_char_boundary`] walks its end backwards rather
/// than forwards.
pub fn tail_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find the smallest char boundary >= len - max. `s.len()` is always a
    // boundary, so this terminates even if the tail is one multi-byte char.
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- derive_source_label ------------------------------------------------

    #[test]
    fn test_source_label_web_chat() {
        let label = derive_source_label("web-chat-2026-03-25", "myagent").unwrap();
        assert_eq!(label.source_type, "web");
        assert_eq!(label.source_label, "User chat");
    }

    #[test]
    fn test_source_label_telegram() {
        let label = derive_source_label("telegram_mybot_123456", "myagent").unwrap();
        assert_eq!(label.source_type, "telegram");
        assert_eq!(label.source_label, "Telegram chat");
    }

    #[test]
    fn test_source_label_job() {
        let label = derive_source_label("job_abc123", "myagent").unwrap();
        assert_eq!(label.source_type, "job");
        assert_eq!(label.source_label, "Scheduled job: abc123");
    }

    #[test]
    fn test_source_label_job_long_id() {
        let long_id = "a".repeat(50);
        let label = derive_source_label(&format!("job_{long_id}"), "myagent").unwrap();
        assert_eq!(label.source_type, "job");
        // Should be truncated to 37 chars + "..."
        assert!(label.source_label.ends_with("..."));
        assert!(label.source_label.len() <= 55);
    }

    #[test]
    fn test_source_label_dm_alice_perspective() {
        // alice sees "DM with bob"
        let label = derive_source_label("dm:alice:bob", "alice").unwrap();
        assert_eq!(label.source_type, "dm");
        assert_eq!(label.source_label, "DM with bob");
    }

    #[test]
    fn test_source_label_dm_bob_perspective() {
        // bob sees "DM with alice"
        let label = derive_source_label("dm:alice:bob", "bob").unwrap();
        assert_eq!(label.source_type, "dm");
        assert_eq!(label.source_label, "DM with alice");
    }

    #[test]
    fn test_source_label_dm_malformed_excluded() {
        // Malformed DM context_id (no second colon) should be excluded.
        assert!(derive_source_label("dm:alice", "alice").is_none());
    }

    #[test]
    fn test_source_label_subagent_excluded() {
        assert!(derive_source_label("subagent_task_123", "myagent").is_none());
    }

    #[test]
    fn test_source_label_episodic_excluded() {
        assert!(derive_source_label("episodic:myagent", "myagent").is_none());
    }

    #[test]
    fn test_source_label_notification() {
        assert!(derive_source_label("notifications:bob", "myagent").is_none());
    }

    #[test]
    fn test_source_label_unknown_defaults_to_web() {
        let label = derive_source_label("some-random-context", "myagent").unwrap();
        assert_eq!(label.source_type, "web");
        assert_eq!(label.source_label, "User chat");
    }

    // -- truncate_to_char_boundary ------------------------------------------

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate_to_char_boundary("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_no_op() {
        assert_eq!(truncate_to_char_boundary("hi", 10), "hi");
    }

    #[test]
    fn test_truncate_multibyte() {
        // Each emoji is 4 bytes. Truncating at 5 should give us just the first emoji.
        let s = "\u{1F600}\u{1F601}\u{1F602}"; // 3 emoji, 12 bytes
        let result = truncate_to_char_boundary(s, 5);
        assert_eq!(result, "\u{1F600}"); // 4 bytes, next boundary is at 8
    }

    // -- tail_to_char_boundary ----------------------------------------------

    /// The whole point of the function: it keeps the *other* end. Pinned
    /// against its head-anchored twin in one assertion so a future edit that
    /// collapses the two is a failing test rather than a silent regression
    /// in whatever reads the tail (#1308).
    #[test]
    fn test_tail_and_truncate_cut_opposite_ends() {
        let s = "0123456789";
        assert_eq!(truncate_to_char_boundary(s, 3), "012");
        assert_eq!(tail_to_char_boundary(s, 3), "789");
    }

    #[test]
    fn test_tail_no_op_when_already_short_enough() {
        // Strictly shorter, and exactly at the cap: the second is the row the
        // early return exists for at all -- without it `s.len() - max`
        // underflows for the first.
        assert_eq!(tail_to_char_boundary("hi", 10), "hi");
        assert_eq!(tail_to_char_boundary("hi", 2), "hi");
        assert_eq!(tail_to_char_boundary("", 0), "");
    }

    #[test]
    fn test_tail_zero_max_is_empty() {
        assert_eq!(tail_to_char_boundary("abc", 0), "");
    }

    /// The start is walked *forward* off a split codepoint, never backward:
    /// backward would keep the result valid UTF-8 too, but at `max + 1` bytes,
    /// which quietly breaks every caller that picked `max` as a hard budget.
    #[test]
    fn test_tail_multibyte_walks_forward_off_the_split() {
        let s = "aa\u{E9}b"; // 5 bytes: the 2-byte 'e-acute' sits at 2..4
        let result = tail_to_char_boundary(s, 2);
        assert_eq!(result, "b", "byte 3 is mid-char, so the start moves to 4");
        assert!(
            result.len() <= 2,
            "walking backward would return 3 bytes for a 2-byte budget"
        );
    }

    #[test]
    fn test_tail_multibyte_exact_boundary_is_not_moved() {
        // Each emoji is 4 bytes; len - 8 is already a boundary, so the tail is
        // the last two emoji exactly.
        let s = "\u{1F600}\u{1F601}\u{1F602}"; // 3 emoji, 12 bytes
        assert_eq!(tail_to_char_boundary(s, 8), "\u{1F601}\u{1F602}");
    }
}
