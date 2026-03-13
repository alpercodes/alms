//! Telegram Bot API client implementation

mod types;
use alms_core::channel::{ChatId, MessageId, UserId};
use alms_core::{AlmsResult, Channel, ChannelConfig, IncomingMessage, OutgoingMessage};
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{error, info, warn};
use types::*;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";
/// Telegram's hard limit for sendMessage text.
// NOTE: Telegram counts UTF-16 code units, but we split on UTF-8 bytes.
// This is conservative — we may over-split (send shorter chunks than needed)
// but will never exceed the limit.
const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

/// Telegram Bot API client
#[derive(Debug, Clone)]
pub struct TelegramChannel {
    token: String,
    client: Client,
    base_url: String,
    last_update_id: Arc<AtomicI64>,
    running: Arc<AtomicBool>,
    use_webhook: bool,
    webhook_url: Option<String>,
    /// Currently unused after removing the interval ticker (long-poll timeout
    /// is the wait mechanism). Retained for config API compatibility; may be
    /// repurposed as error backoff duration.
    #[allow(dead_code)]
    poll_interval_secs: u64,
}

impl TelegramChannel {
    /// Create a new Telegram channel (not initialized)
    pub fn new() -> Self {
        Self {
            token: String::new(),
            client: Client::new(),
            base_url: String::new(),
            last_update_id: Arc::new(AtomicI64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            use_webhook: false,
            webhook_url: None,
            poll_interval_secs: 5,
        }
    }

    /// Build the API URL for a method
    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", TELEGRAM_API_BASE, self.token, method)
    }

    /// Make a GET request to the Telegram API
    async fn get<T: serde::de::DeserializeOwned>(&self, method: &str) -> AlmsResult<T> {
        let url = self.api_url(method);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| alms_core::AlmsError::Channel(format!("HTTP error: {}", e)))?;

        let api_response: TelegramResponse<T> = response
            .json()
            .await
            .map_err(|e| alms_core::AlmsError::Channel(format!("JSON parse error: {}", e)))?;

        if api_response.ok {
            api_response
                .result
                .ok_or_else(|| alms_core::AlmsError::Channel("Empty result from API".to_string()))
        } else {
            Err(alms_core::AlmsError::Channel(
                api_response
                    .description
                    .unwrap_or_else(|| "Unknown API error".to_string()),
            ))
        }
    }

    /// Make a POST request to the Telegram API
    async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: &B,
    ) -> AlmsResult<T> {
        let url = self.api_url(method);
        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| alms_core::AlmsError::Channel(format!("HTTP error: {}", e)))?;

        let api_response: TelegramResponse<T> = response
            .json()
            .await
            .map_err(|e| alms_core::AlmsError::Channel(format!("JSON parse error: {}", e)))?;

        if api_response.ok {
            api_response
                .result
                .ok_or_else(|| alms_core::AlmsError::Channel("Empty result from API".to_string()))
        } else {
            Err(alms_core::AlmsError::Channel(
                api_response
                    .description
                    .unwrap_or_else(|| "Unknown API error".to_string()),
            ))
        }
    }

    /// Get bot information
    pub async fn get_me(&self) -> AlmsResult<User> {
        self.get("getMe").await
    }

    /// Send a raw message via the API.
    ///
    /// **Warning**: bypasses `split_message` — caller must ensure `text` fits
    /// within [`TELEGRAM_MAX_MESSAGE_LEN`] or handle splitting themselves.
    pub async fn send_raw_message(
        &self,
        chat_id: i64,
        text: impl Into<String>,
    ) -> AlmsResult<SentMessage> {
        let request = SendMessageRequest::new(chat_id, text);
        self.post("sendMessage", &request).await
    }

    /// Get updates via polling
    async fn get_updates(&self, offset: Option<i64>) -> AlmsResult<Vec<Update>> {
        let mut request = GetUpdatesRequest::new();
        if let Some(off) = offset {
            request = request.offset(off);
        }
        self.post("getUpdates", &request).await
    }

    /// Set webhook for receiving updates
    async fn set_webhook(
        &self,
        url: impl Into<String>,
        secret_token: Option<String>,
    ) -> AlmsResult<bool> {
        let request = SetWebhookRequest {
            url: url.into(),
            secret_token,
        };
        self.post("setWebhook", &request).await
    }

    /// Delete webhook (switch to polling)
    async fn delete_webhook(&self) -> AlmsResult<bool> {
        #[derive(Serialize)]
        struct DeleteWebhookRequest {
            drop_pending_updates: bool,
        }
        let request = DeleteWebhookRequest {
            drop_pending_updates: true,
        };
        self.post("deleteWebhook", &request).await
    }

    /// Convert a Telegram Update to an IncomingMessage
    fn convert_update(&self, update: Update) -> Option<IncomingMessage> {
        // Get the message (either regular or edited)
        let message = update.message.or(update.edited_message)?;

        // Only handle text messages for now
        let text = message.text?;
        let from = message.from?;

        let mut incoming = IncomingMessage {
            chat_id: ChatId(message.chat.id),
            user_id: UserId(from.id),
            text: text.clone(),
            timestamp: alms_core::Timestamp::now(),
            message_id: MessageId(message.message_id),
            command: None,
            platform_data: Some(serde_json::json!({
                "update_id": update.update_id,
                "chat_type": message.chat.chat_type,
                "username": from.username,
                "first_name": from.first_name,
            })),
        };

        // Parse commands
        incoming.parse_command();

        Some(incoming)
    }

    /// Send a single chunk via sendMessage, returning the sent message ID.
    #[tracing::instrument(skip(self, text), fields(chat_id, text_len = text.len()))]
    async fn send_chunk(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i64>,
    ) -> AlmsResult<MessageId> {
        let mut request = SendMessageRequest::new(chat_id, text);
        if let Some(id) = reply_to {
            request = request.reply_to(id);
        }
        let sent: SentMessage = self.post("sendMessage", &request).await?;
        Ok(MessageId(sent.message_id))
    }

    /// Run the polling loop
    async fn run_polling(&self, tx: mpsc::Sender<IncomingMessage>) -> AlmsResult<()> {
        info!("Starting Telegram polling loop");

        while self.running.load(Ordering::Relaxed) {
            let offset = self.last_update_id.load(Ordering::Relaxed);
            let offset_param = if offset > 0 { Some(offset + 1) } else { None };

            match self.get_updates(offset_param).await {
                Ok(updates) => {
                    for update in updates {
                        if update.update_id > self.last_update_id.load(Ordering::Relaxed) {
                            self.last_update_id
                                .store(update.update_id, Ordering::Relaxed);
                        }

                        if let Some(message) = self.convert_update(update)
                            && tx.send(message).await.is_err()
                        {
                            warn!("Message receiver dropped, stopping polling");
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    error!("Error getting updates: {}", e);
                    // Back off on error to avoid hammering a down API
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }

        info!("Telegram polling loop stopped");
        Ok(())
    }
}

/// Split `text` into chunks that each fit within `max_len` bytes.
///
/// Tries to break at paragraph boundaries (`\n\n`), then line boundaries (`\n`),
/// and falls back to a hard char-boundary split as a last resort.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    debug_assert!(max_len > 0);
    if text.len() <= max_len {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining);
            break;
        }

        // Search window: the first `max_len` bytes (must land on a char boundary).
        let window = &remaining[..floor_char_boundary(remaining, max_len)];

        // Try paragraph break.
        let split_at = window
            .rfind("\n\n")
            .map(|pos| pos + 2) // include the delimiter in the first chunk
            // Try line break.
            .or_else(|| window.rfind('\n').map(|pos| pos + 1))
            // Hard split at char boundary.
            .unwrap_or_else(|| floor_char_boundary(remaining, max_len));

        let (chunk, rest) = remaining.split_at(split_at);
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        remaining = rest;
    }

    chunks
}

/// Find the largest byte index ≤ `max` that falls on a UTF-8 char boundary.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

impl Default for TelegramChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn initialize(&mut self, config: ChannelConfig) -> AlmsResult<()> {
        if config.token.is_empty() {
            return Err(alms_core::AlmsError::InvalidConfig(
                "Telegram bot token is required".to_string(),
            ));
        }

        self.token = config.token;
        self.base_url = format!("{}{}", TELEGRAM_API_BASE, self.token);
        self.use_webhook = config.use_webhook;
        self.webhook_url = config.webhook_url;
        self.poll_interval_secs = config.poll_interval_secs;

        // Validate the token by getting bot info
        let me = self.get_me().await?;
        info!(
            "Telegram bot initialized: @{} (ID: {})",
            me.username.as_deref().unwrap_or("unknown"),
            me.id
        );

        Ok(())
    }

    async fn send_message(&self, message: OutgoingMessage) -> AlmsResult<MessageId> {
        let chunks = split_message(&message.text, TELEGRAM_MAX_MESSAGE_LEN);
        let reply_to = message.reply_to.map(|id| id.0);

        let mut last_id = MessageId(0);
        for (i, chunk) in chunks.iter().enumerate() {
            // Only the first chunk replies to the original message.
            let reply = if i == 0 { reply_to } else { None };
            last_id = self.send_chunk(message.chat_id.0, chunk, reply).await?;
        }
        Ok(last_id)
    }

    async fn receive_updates(&self) -> AlmsResult<mpsc::Receiver<IncomingMessage>> {
        let (tx, rx) = mpsc::channel(100);

        let channel = self.clone();
        tokio::spawn(async move {
            if let Err(e) = channel.run_polling(tx).await {
                error!("Polling error: {}", e);
            }
        });

        Ok(rx)
    }

    async fn start(&self) -> AlmsResult<()> {
        self.running.store(true, Ordering::Relaxed);

        if self.use_webhook {
            if let Some(ref url) = self.webhook_url {
                info!("Setting Telegram webhook to: {}", url);
                self.set_webhook(url, None).await?;
            } else {
                warn!("Webhook mode enabled but no URL provided");
            }
        } else {
            info!("Starting Telegram in polling mode");
            self.delete_webhook().await?;
        }

        Ok(())
    }

    async fn stop(&self) -> AlmsResult<()> {
        info!("Stopping Telegram channel");
        self.running.store(false, Ordering::Relaxed);

        if self.use_webhook {
            self.delete_webhook().await?;
        }

        Ok(())
    }
}

use serde::Serialize;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // -- Test helpers --

    fn make_user() -> User {
        User {
            id: 12345,
            is_bot: false,
            first_name: "Test".to_string(),
            last_name: None,
            username: Some("testuser".to_string()),
            language_code: None,
        }
    }

    fn make_chat() -> Chat {
        Chat {
            id: 67890,
            chat_type: "private".to_string(),
            title: None,
            username: None,
            first_name: Some("Test".to_string()),
            last_name: None,
        }
    }

    fn make_message(text: &str) -> Message {
        Message {
            message_id: 1,
            from: Some(make_user()),
            date: 1700000000,
            chat: make_chat(),
            text: Some(text.to_string()),
            entities: None,
        }
    }

    fn make_update(update_id: i64, text: &str) -> Update {
        Update {
            update_id,
            message: Some(make_message(text)),
            edited_message: None,
            callback_query: None,
        }
    }

    // -- convert_update tests --

    #[test]
    fn convert_update_text_message() {
        let channel = TelegramChannel::new();
        let update = make_update(1, "hello world");
        let incoming = channel.convert_update(update).unwrap();
        assert_eq!(incoming.chat_id, ChatId(67890));
        assert_eq!(incoming.user_id, UserId(12345));
        assert_eq!(incoming.text, "hello world");
        assert_eq!(incoming.message_id, MessageId(1));
    }

    #[test]
    fn convert_update_edited_message() {
        let channel = TelegramChannel::new();
        let update = Update {
            update_id: 2,
            message: None,
            edited_message: Some(make_message("edited text")),
            callback_query: None,
        };
        let incoming = channel.convert_update(update).unwrap();
        assert_eq!(incoming.text, "edited text");
    }

    #[test]
    fn convert_update_no_message() {
        let channel = TelegramChannel::new();
        let update = Update {
            update_id: 3,
            message: None,
            edited_message: None,
            callback_query: None,
        };
        assert!(channel.convert_update(update).is_none());
    }

    #[test]
    fn convert_update_no_text() {
        let channel = TelegramChannel::new();
        let mut msg = make_message("ignored");
        msg.text = None;
        let update = Update {
            update_id: 4,
            message: Some(msg),
            edited_message: None,
            callback_query: None,
        };
        assert!(channel.convert_update(update).is_none());
    }

    #[test]
    fn convert_update_no_from() {
        let channel = TelegramChannel::new();
        let mut msg = make_message("hello");
        msg.from = None;
        let update = Update {
            update_id: 5,
            message: Some(msg),
            edited_message: None,
            callback_query: None,
        };
        assert!(channel.convert_update(update).is_none());
    }

    #[test]
    fn convert_update_parses_command() {
        let channel = TelegramChannel::new();
        let update = make_update(6, "/start arg1");
        let incoming = channel.convert_update(update).unwrap();
        assert!(incoming.command.is_some());
        let cmd = incoming.command.unwrap();
        assert_eq!(cmd.name, "start");
        assert_eq!(cmd.args, vec!["arg1"]);
    }

    #[test]
    fn convert_update_platform_data() {
        let channel = TelegramChannel::new();
        let update = make_update(7, "hi");
        let incoming = channel.convert_update(update).unwrap();
        let pd = incoming.platform_data.unwrap();
        assert_eq!(pd["update_id"], 7);
        assert_eq!(pd["chat_type"], "private");
        assert_eq!(pd["username"], "testuser");
        assert_eq!(pd["first_name"], "Test");
    }

    // -- api_url test --

    #[test]
    fn api_url_builds_correctly() {
        let mut channel = TelegramChannel::new();
        channel.token = "tok123".to_string();
        assert_eq!(
            channel.api_url("sendMessage"),
            "https://api.telegram.org/bottok123/sendMessage"
        );
    }

    // -- Polling offset tests --

    #[test]
    fn offset_initial_is_none() {
        let channel = TelegramChannel::new();
        let offset = channel.last_update_id.load(Ordering::Relaxed);
        assert_eq!(offset, 0);
        // Mirrors run_polling logic: offset > 0 → Some(offset+1), else None
        let offset_param = if offset > 0 { Some(offset + 1) } else { None };
        assert_eq!(offset_param, None);
    }

    #[test]
    fn offset_after_store() {
        let channel = TelegramChannel::new();
        channel.last_update_id.store(42, Ordering::Relaxed);
        let offset = channel.last_update_id.load(Ordering::Relaxed);
        let offset_param = if offset > 0 { Some(offset + 1) } else { None };
        assert_eq!(offset_param, Some(43));
    }

    #[test]
    fn offset_monotonic() {
        let channel = TelegramChannel::new();
        // Mirrors run_polling: only store if update_id > current
        for id in [10, 5, 20] {
            let current = channel.last_update_id.load(Ordering::Relaxed);
            if id > current {
                channel.last_update_id.store(id, Ordering::Relaxed);
            }
        }
        assert_eq!(channel.last_update_id.load(Ordering::Relaxed), 20);
    }

    #[test]
    fn offset_batch() {
        let channel = TelegramChannel::new();
        for id in [100, 101, 102] {
            let current = channel.last_update_id.load(Ordering::Relaxed);
            if id > current {
                channel.last_update_id.store(id, Ordering::Relaxed);
            }
        }
        let offset = channel.last_update_id.load(Ordering::Relaxed);
        let offset_param = if offset > 0 { Some(offset + 1) } else { None };
        assert_eq!(offset_param, Some(103));
    }

    // -- split_message tests (existing) --

    #[test]
    fn short_message_unchanged() {
        let text = "Hello, world!";
        let chunks = split_message(text, 4096);
        assert_eq!(chunks, vec!["Hello, world!"]);
    }

    #[test]
    fn splits_at_paragraph_boundary() {
        let a = "a".repeat(100);
        let b = "b".repeat(100);
        let text = format!("{}\n\n{}", a, b);
        let chunks = split_message(&text, 150);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], format!("{}\n\n", a));
        assert_eq!(chunks[1], b.as_str());
    }

    #[test]
    fn splits_at_line_boundary() {
        let a = "a".repeat(100);
        let b = "b".repeat(100);
        let text = format!("{}\n{}", a, b);
        let chunks = split_message(&text, 150);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], format!("{}\n", a));
        assert_eq!(chunks[1], b.as_str());
    }

    #[test]
    fn hard_split_on_long_line() {
        let text = "x".repeat(300);
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[1].len(), 100);
        assert_eq!(chunks[2].len(), 100);
    }

    #[test]
    fn respects_utf8_char_boundaries() {
        // Each emoji is 4 bytes. 10 emojis = 40 bytes.
        let text = "😀".repeat(10);
        let chunks = split_message(&text, 13); // 3 emojis = 12 bytes fits, 4th would be 16
        // Should split at 12 bytes (3 emojis), not mid-emoji.
        for chunk in &chunks {
            assert!(chunk.len() <= 13);
            // Verify valid UTF-8 (implicit since &str)
            assert!(!chunk.is_empty());
        }
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn exact_boundary_no_split() {
        let text = "a".repeat(4096);
        let chunks = split_message(&text, 4096);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn prefers_paragraph_over_line() {
        // Both \n\n and \n are present; should split at \n\n.
        let text = "aaaa\nbbbb\n\ncccc";
        let chunks = split_message(text, 12);
        assert_eq!(chunks[0], "aaaa\nbbbb\n\n");
        assert_eq!(chunks[1], "cccc");
    }

    #[test]
    fn empty_message() {
        let chunks = split_message("", 100);
        assert_eq!(chunks, vec![""]);
    }
}
