//! Channel trait and message types for messaging platform adapters

use crate::{AlmsResult, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Unique identifier for a chat/channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub i64);

/// Unique identifier for a user
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub i64);

/// Unique identifier for a message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub i64);

/// An incoming message from any channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// The chat/channel this message came from
    pub chat_id: ChatId,
    /// The user who sent this message
    pub user_id: UserId,
    /// The message content (text)
    pub text: String,
    /// When the message was received
    pub timestamp: Timestamp,
    /// The original message ID from the platform
    pub message_id: MessageId,
    /// Optional command if this was a command message
    pub command: Option<Command>,
    /// Platform-specific data (JSON)
    pub platform_data: Option<serde_json::Value>,
}

impl IncomingMessage {
    pub fn new(chat_id: ChatId, user_id: UserId, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            user_id,
            text: text.into(),
            timestamp: Timestamp::now(),
            message_id: MessageId(0),
            command: None,
            platform_data: None,
        }
    }

    /// Parse commands from the message text (e.g., /start, /help)
    pub fn parse_command(&mut self) {
        let trimmed = self.text.trim();
        if let Some(cmd) = Command::parse(trimmed) {
            self.command = Some(cmd);
        }
    }
}

/// A parsed command from a message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    /// The command name (e.g., "start", "help")
    pub name: String,
    /// Command arguments
    pub args: Vec<String>,
    /// Full command text
    pub full_text: String,
}

impl Command {
    /// Parse a command from text like "/start arg1 arg2"
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();

        // Check if it starts with /
        if !trimmed.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let cmd_part = parts[0];
        // Remove the leading /
        let name = cmd_part.strip_prefix('/').unwrap_or(cmd_part).to_string();

        // Get arguments (everything after the command)
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

        Some(Self {
            name,
            args,
            full_text: trimmed.to_string(),
        })
    }
}

/// An outgoing message to be sent to a channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// The chat/channel to send to
    pub chat_id: ChatId,
    /// The message content
    pub text: String,
    /// Optional message to reply to
    pub reply_to: Option<MessageId>,
    /// Platform-specific options (JSON)
    pub options: Option<serde_json::Value>,
}

impl OutgoingMessage {
    pub fn new(chat_id: ChatId, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            reply_to: None,
            options: None,
        }
    }

    pub fn reply_to(mut self, message_id: MessageId) -> Self {
        self.reply_to = Some(message_id);
        self
    }
}

/// A message that can be either incoming or outgoing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelMessage {
    Incoming(IncomingMessage),
    Outgoing(OutgoingMessage),
}

/// Configuration for a channel adapter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// The bot token or API key
    pub token: String,
    /// Webhook URL (if using webhooks)
    pub webhook_url: Option<String>,
    /// Polling interval in seconds (if using polling)
    pub poll_interval_secs: u64,
    /// Whether to use webhook or polling
    pub use_webhook: bool,
    /// Additional platform-specific config
    pub extra: Option<serde_json::Value>,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            webhook_url: None,
            poll_interval_secs: 5,
            use_webhook: false,
            extra: None,
        }
    }
}

/// The Channel trait defines the interface for messaging platform adapters
#[async_trait]
pub trait Channel: Send + Sync {
    /// Get the name of this channel (e.g., "telegram", "discord")
    fn name(&self) -> &str;

    /// Initialize the channel with configuration
    async fn initialize(&mut self, config: ChannelConfig) -> AlmsResult<()>;

    /// Send a message to the channel
    async fn send_message(&self, message: OutgoingMessage) -> AlmsResult<MessageId>;

    /// Send a simple text message
    async fn send_text(
        &self,
        chat_id: ChatId,
        text: impl Into<String> + Send,
    ) -> AlmsResult<MessageId> {
        self.send_message(OutgoingMessage::new(chat_id, text)).await
    }

    /// Start receiving updates (returns a stream receiver)
    async fn receive_updates(&self) -> AlmsResult<mpsc::Receiver<IncomingMessage>>;

    /// Start the channel (begin polling or webhook)
    async fn start(&self) -> AlmsResult<()>;

    /// Stop the channel
    async fn stop(&self) -> AlmsResult<()>;
}

/// A boxed channel for dynamic dispatch
pub type BoxedChannel = Box<dyn Channel>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_command() {
        let cmd = Command::parse("/start").unwrap();
        assert_eq!(cmd.name, "start");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn parse_command_with_args() {
        let cmd = Command::parse("/run a b").unwrap();
        assert_eq!(cmd.name, "run");
        assert_eq!(cmd.args, vec!["a", "b"]);
    }

    #[test]
    fn parse_non_command() {
        assert!(Command::parse("hello").is_none());
    }

    #[test]
    fn parse_empty() {
        assert!(Command::parse("").is_none());
    }

    #[test]
    fn parse_slash_only() {
        // "/" splits into one part "/", strip_prefix yields ""
        let cmd = Command::parse("/").unwrap();
        assert_eq!(cmd.name, "");
    }

    #[test]
    fn parse_command_full_text() {
        let cmd = Command::parse("/deploy prod --force").unwrap();
        assert_eq!(cmd.full_text, "/deploy prod --force");
    }

    #[test]
    fn parse_command_extra_spaces() {
        let cmd = Command::parse("  /start  arg1  ").unwrap();
        assert_eq!(cmd.name, "start");
        assert_eq!(cmd.args, vec!["arg1"]);
        assert_eq!(cmd.full_text, "/start  arg1");
    }

    #[test]
    fn incoming_parse_command_sets_field() {
        let mut msg = IncomingMessage::new(ChatId(1), UserId(2), "/start");
        msg.parse_command();
        assert!(msg.command.is_some());
        assert_eq!(msg.command.unwrap().name, "start");
    }

    #[test]
    fn incoming_parse_no_command() {
        let mut msg = IncomingMessage::new(ChatId(1), UserId(2), "hello");
        msg.parse_command();
        assert!(msg.command.is_none());
    }
}
