//! Shared DM message filter — identifies synthetic markers that should be
//! excluded from DM conversation output.
//!
//! This centralises the filtering logic so that `read_messages`, `read_session`,
//! and any future DM-reading code paths stay consistent. The reference
//! behaviour matches `format_dm_conversation_history()` in `alms-gateway`.

use alms_session::{Content, Message};

/// Known `message_type` values that represent synthetic system markers.
/// Real DM messages carry `"message_type": "dm"` — those must NOT be filtered.
const SYNTHETIC_MESSAGE_TYPES: &[&str] = &["dm_ended"];

/// Returns `true` if the message is a synthetic marker that should be
/// filtered out of DM conversation output.
///
/// Synthetic markers include:
/// - Non-text content (tool calls, tool results, images).
/// - Messages with empty or whitespace-only text (e.g. `dm_ended` marker bodies).
/// - Messages whose `message_type` metadata matches a known synthetic type
///   (currently: `dm_ended`). Note: `"message_type": "dm"` is a *real* DM
///   message and is explicitly preserved.
/// - Messages flagged as `synthetic: true` in metadata.
pub fn is_synthetic_marker(msg: &Message) -> bool {
    // Non-text content (tool calls, tool results, images) are internal
    // bookkeeping and should not appear in DM conversation output.
    let text = match &msg.content {
        Content::Text(t) => t.as_str(),
        _ => return true,
    };

    // Empty text bodies are metadata-only markers (e.g. dm_ended).
    if text.trim().is_empty() {
        return true;
    }

    // Check metadata for marker indicators.
    if let Some(ref meta) = msg.metadata {
        // Only filter messages whose `message_type` is a known synthetic
        // marker. Real DM messages have `"message_type": "dm"` and must
        // NOT be filtered.
        if let Some(msg_type) = meta.get("message_type").and_then(|v| v.as_str())
            && SYNTHETIC_MESSAGE_TYPES.contains(&msg_type)
        {
            return true;
        }
        // Synthetic notification markers.
        if meta.get("synthetic").and_then(|v| v.as_bool()) == Some(true) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::Role;

    fn msg_with_meta(text: &str, meta: Option<serde_json::Value>) -> Message {
        Message {
            id: "test".into(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: meta,
        }
    }

    #[test]
    fn real_dm_message_is_not_filtered() {
        // Production format: real DM messages have "message_type": "dm"
        let msg = msg_with_meta(
            "Hello!",
            Some(serde_json::json!({
                "from_agent": "alice",
                "from_agent_id": "00000000-0000-0000-0000-000000000001",
                "message_type": "dm",
            })),
        );
        assert!(
            !is_synthetic_marker(&msg),
            "Real DM message with message_type=dm must NOT be filtered"
        );
    }

    #[test]
    fn real_message_without_message_type_is_not_filtered() {
        let msg = msg_with_meta("Hello!", Some(serde_json::json!({"from_agent": "alice"})));
        assert!(!is_synthetic_marker(&msg));
    }

    #[test]
    fn dm_ended_marker_is_filtered() {
        let msg = msg_with_meta(
            "",
            Some(serde_json::json!({
                "message_type": "dm_ended",
                "ended_by": "alice",
                "reason": "ignored",
            })),
        );
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn dm_ended_with_text_is_filtered() {
        let msg = msg_with_meta(
            "some leftover text",
            Some(serde_json::json!({"message_type": "dm_ended"})),
        );
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn empty_text_is_filtered() {
        let msg = msg_with_meta("", None);
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn whitespace_only_text_is_filtered() {
        let msg = msg_with_meta("   \n  ", None);
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn synthetic_true_is_filtered() {
        let msg = msg_with_meta("notification", Some(serde_json::json!({"synthetic": true})));
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn tool_result_is_filtered() {
        let msg = Message {
            id: "test".into(),
            role: Role::Tool,
            content: Content::ToolResult {
                tool_id: "t1".into(),
                result: serde_json::json!("ok"),
            },
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };
        assert!(is_synthetic_marker(&msg));
    }

    #[test]
    fn tool_call_content_is_filtered() {
        let msg = Message {
            id: "test".into(),
            role: Role::Assistant,
            content: Content::ToolCall {
                name: "echo".into(),
                params: serde_json::json!({}),
            },
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };
        assert!(is_synthetic_marker(&msg));
    }
}
