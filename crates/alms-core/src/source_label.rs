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
}
