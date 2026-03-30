use alms_core::{AgentId, RunId, SessionId, Timestamp};

/// Per-session episodic summary for cross-session memory.
///
/// Stores one evolving summary per `(agent_id, session_id)` pair, updated at
/// run boundaries.  These summaries are injected into the system prompt of
/// *other* sessions so the agent has cross-session awareness.
///
/// Distinct from [`ContextSummary`], which is used for within-session rolling
/// compression by the `sliding-summary` context strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// The agent this summary belongs to.
    pub agent_id: AgentId,
    /// The session being summarised.
    pub session_id: SessionId,
    /// The accumulated summary text.
    pub summary: String,
    /// The run that last updated this summary (for debugging / auditing).
    pub last_run_id: Option<RunId>,
    /// When this summary was last updated.
    pub updated_at: Timestamp,
    /// Human-readable source label (e.g. "User chat", "Telegram chat").
    ///
    /// Derived from the session's `context_id` at summary generation time.
    /// Used by the episodic injection formatter to produce distinct headers
    /// like `**User chat (last active: ...)**` instead of generic `**Session ...**`.
    ///
    /// `None` indicates an unknown or excluded source — entries with `None`
    /// are filtered out at injection time as a defense-in-depth measure.
    pub source_label: Option<String>,
}

/// Rolling context summary for a session (used by the sliding-summary strategy).
///
/// Tracks how many messages from the start of the session history have been
/// compressed into `text`, so the runtime always knows where the "recent window"
/// begins.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextSummary {
    /// The accumulated summary text.
    pub text: String,
    /// Number of messages from the session history covered by this summary.
    pub messages_covered: usize,
    /// Timestamp of the last summary update.
    pub updated_at: Option<Timestamp>,
}
use serde::{Deserialize, Serialize};

/// Session state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent_id: AgentId,
    pub context_id: String,
    pub created_at: Timestamp,
    pub last_activity: Timestamp,
    pub status: SessionStatus,
}

impl Session {
    pub fn new(agent_id: AgentId, context_id: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: SessionId::new(),
            agent_id,
            context_id: context_id.into(),
            created_at: now,
            last_activity: now,
            status: SessionStatus::Active,
        }
    }

    pub fn touch(&mut self) {
        self.last_activity = Timestamp::now();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Active,
    Idle,
    Archived,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Idle => write!(f, "idle"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

/// Message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: Content,
    pub timestamp: Timestamp,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Text(String),
    ToolCall {
        name: String,
        params: serde_json::Value,
    },
    ToolResult {
        tool_id: String,
        result: serde_json::Value,
    },
    Image {
        url: String,
        alt: Option<String>,
    },
}

impl Content {
    /// Convert this content value to a plain-text display string.
    ///
    /// Used for human-readable rendering of session messages (e.g. in tool
    /// outputs, summary generation, and context fallback paths).
    ///
    /// **Note:** `ToolResult` output is truncated to 2000 bytes with a
    /// `[truncated, N bytes total]` suffix.  If a caller needs the full
    /// output it should match on `Content::ToolResult` directly.
    pub fn to_display_string(&self) -> String {
        match self {
            Content::Text(text) => text.clone(),
            Content::ToolCall { name, params } => {
                format!("Tool call: {}({})", name, params)
            }
            Content::ToolResult { tool_id, result } => {
                let result_str = result.to_string();
                // Truncate long tool outputs in context
                if result_str.len() > 2000 {
                    format!(
                        "Tool result {}: {}... [truncated, {} bytes total]",
                        tool_id,
                        &result_str[..2000],
                        result_str.len()
                    )
                } else {
                    format!("Tool result {}: {}", tool_id, result_str)
                }
            }
            Content::Image { url, .. } => {
                format!("[Image: {}]", url)
            }
        }
    }
}

/// Session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Idle timeout in seconds (default: 24 hours)
    pub idle_timeout_secs: u64,
    /// Archive after idle timeout (default: true)
    pub auto_archive: bool,
    /// Delete archived sessions after seconds (default: 30 days)
    pub archive_ttl_secs: u64,
    /// Maximum messages per session (default: 10000)
    pub max_messages: usize,
    /// Maximum total tokens to retain in the session's history (storage limit).
    /// Must be >= the LLM per-request budget (`ContextConfig::max_input_tokens`).
    pub max_context_tokens: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 24 * 60 * 60, // 24 hours
            auto_archive: true,
            archive_ttl_secs: 30 * 24 * 60 * 60, // 30 days
            max_messages: 10000,
            max_context_tokens: 256_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_content_serialization_roundtrip() {
        let img_with_alt = Content::Image {
            url: "https://example.com/photo.png".to_string(),
            alt: Some("A sunset".to_string()),
        };
        let json = serde_json::to_value(&img_with_alt).unwrap();
        assert_eq!(json["image"]["url"], "https://example.com/photo.png");
        assert_eq!(json["image"]["alt"], "A sunset");

        let roundtrip: Content = serde_json::from_value(json).unwrap();
        match &roundtrip {
            Content::Image { url, alt } => {
                assert_eq!(url, "https://example.com/photo.png");
                assert_eq!(alt.as_deref(), Some("A sunset"));
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn image_content_without_alt() {
        let img = Content::Image {
            url: "https://example.com/photo.png".to_string(),
            alt: None,
        };
        let json = serde_json::to_value(&img).unwrap();
        assert_eq!(json["image"]["url"], "https://example.com/photo.png");
        assert!(json["image"]["alt"].is_null());

        let roundtrip: Content = serde_json::from_value(json).unwrap();
        match &roundtrip {
            Content::Image { url, alt } => {
                assert_eq!(url, "https://example.com/photo.png");
                assert!(alt.is_none());
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn image_content_alt_key_absent() {
        // When the `alt` key is entirely missing from the JSON (not null, just absent),
        // serde should still deserialize successfully with alt = None.
        let json: serde_json::Value =
            serde_json::from_str(r#"{"image": {"url": "https://example.com/photo.png"}}"#).unwrap();
        let content: Content = serde_json::from_value(json).unwrap();
        match &content {
            Content::Image { url, alt } => {
                assert_eq!(url, "https://example.com/photo.png");
                assert!(alt.is_none());
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn to_display_string_text() {
        let c = Content::Text("hello world".to_string());
        assert_eq!(c.to_display_string(), "hello world");
    }

    #[test]
    fn to_display_string_tool_call() {
        let c = Content::ToolCall {
            name: "echo".into(),
            params: serde_json::json!({"msg": "hi"}),
        };
        let s = c.to_display_string();
        assert!(s.starts_with("Tool call: echo("));
    }

    #[test]
    fn to_display_string_tool_result_short() {
        let c = Content::ToolResult {
            tool_id: "t1".into(),
            result: serde_json::json!("ok"),
        };
        let s = c.to_display_string();
        assert!(s.contains("Tool result t1:"));
        assert!(!s.contains("[truncated"));
    }

    #[test]
    fn to_display_string_tool_result_truncated() {
        let long = "x".repeat(5000);
        let c = Content::ToolResult {
            tool_id: "t2".into(),
            result: serde_json::Value::String(long),
        };
        let s = c.to_display_string();
        assert!(s.contains("[truncated"));
        assert!(s.len() < 3000);
    }

    #[test]
    fn to_display_string_image() {
        let c = Content::Image {
            url: "https://example.com/img.png".into(),
            alt: Some("photo".into()),
        };
        assert_eq!(
            c.to_display_string(),
            "[Image: https://example.com/img.png]"
        );
    }
}
