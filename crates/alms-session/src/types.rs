use alms_core::{AgentId, SessionId, Timestamp};

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
    /// Maximum context tokens (default: 128000)
    pub max_context_tokens: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 24 * 60 * 60, // 24 hours
            auto_archive: true,
            archive_ttl_secs: 30 * 24 * 60 * 60, // 30 days
            max_messages: 10000,
            max_context_tokens: 128000,
        }
    }
}
