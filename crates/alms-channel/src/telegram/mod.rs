//! Telegram Bot API client implementation

mod types;
use alms_core::channel::{ChatId, MessageId, UserId};
use alms_core::{
    AlmsResult, Channel, ChannelConfig, IncomingMessage, OutgoingMessage, truncate_to_char_boundary,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::Client;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use types::*;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";

/// A newtype that wraps a secret string and redacts it in `Debug` output
/// to prevent accidental exposure in logs or error messages.
#[derive(Clone)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

impl Secret {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Telegram's hard limit for sendMessage text.
// NOTE: Telegram counts UTF-16 code units, but we split on UTF-8 bytes.
// This is conservative — we may over-split (send shorter chunks than needed)
// but will never exceed the limit.
const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

/// Telegram Bot API client
#[derive(Debug, Clone)]
pub struct TelegramChannel {
    token: Secret,
    client: Client,
    last_update_id: Arc<AtomicI64>,
    running: Arc<AtomicBool>,
    use_webhook: bool,
    webhook_url: Option<String>,
    /// Token used to cancel the polling loop immediately (including in-flight
    /// long-poll HTTP requests) without waiting for the 30-second timeout.
    cancel_token: CancellationToken,
    /// Handle to the spawned polling task. Guarded by a mutex so `stop()` can
    /// abort the task and `receive_updates()` can detect double-calls.
    poll_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Currently unused after removing the interval ticker (long-poll timeout
    /// is the wait mechanism). Retained for config API compatibility; may be
    /// repurposed as error backoff duration.
    #[allow(dead_code)]
    poll_interval_secs: u64,
    /// Optional file path for persisting last_update_id across restarts.
    /// When set, the offset is read on startup and written after each batch.
    offset_file: Option<std::path::PathBuf>,
}

impl TelegramChannel {
    /// Create a new Telegram channel (not initialized)
    pub fn new() -> Self {
        Self {
            token: Secret(String::new()),
            client: Client::new(),
            last_update_id: Arc::new(AtomicI64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            use_webhook: false,
            webhook_url: None,
            cancel_token: CancellationToken::new(),
            poll_handle: Arc::new(Mutex::new(None)),
            poll_interval_secs: 5,
            offset_file: None,
        }
    }

    /// Set the file path for persisting the Telegram update offset.
    pub fn with_offset_file(mut self, path: std::path::PathBuf) -> Self {
        self.offset_file = Some(path);
        self
    }

    /// Load persisted offset from file. Returns 0 if file doesn't exist, is invalid,
    /// or contains a negative value.
    fn load_offset(&self) -> i64 {
        let Some(ref path) = self.offset_file else {
            return 0;
        };
        match std::fs::read_to_string(path) {
            Ok(s) => s.trim().parse::<i64>().unwrap_or(0).max(0),
            Err(_) => 0,
        }
    }

    /// Persist current offset to file.
    ///
    /// On Unix, uses atomic temp+rename. On Windows, writes directly since
    /// `fs::rename` fails when the target exists.
    fn save_offset(&self, offset: i64) {
        let Some(ref path) = self.offset_file else {
            return;
        };
        if cfg!(windows) {
            // Windows: fs::rename fails if target exists. Write directly —
            // small file, corruption risk is negligible for an offset value.
            if let Err(e) = std::fs::write(path, offset.to_string()) {
                warn!(
                    "Failed to write Telegram offset to {}: {}",
                    path.display(),
                    e
                );
            }
        } else {
            // Unix: atomic temp+rename
            let tmp = path.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, offset.to_string()) {
                warn!(
                    "Failed to write Telegram offset to {}: {}",
                    tmp.display(),
                    e
                );
                return;
            }
            if let Err(e) = std::fs::rename(&tmp, path) {
                warn!("Failed to rename Telegram offset file: {}", e);
            }
        }
    }

    /// Build the API URL for a method
    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", TELEGRAM_API_BASE, self.token.as_str(), method)
    }

    /// Make a GET request to the Telegram API
    async fn get<T: serde::de::DeserializeOwned>(&self, method: &str) -> AlmsResult<T> {
        let url = self.api_url(method);
        let response = self.client.get(&url).send().await.map_err(|e| {
            alms_core::AlmsError::Channel(format!("HTTP error: {}", e.without_url()))
        })?;

        let api_response: TelegramResponse<T> = response.json().await.map_err(|e| {
            alms_core::AlmsError::Channel(format!("JSON parse error: {}", e.without_url()))
        })?;

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
            .map_err(|e| {
                alms_core::AlmsError::Channel(format!("HTTP error: {}", e.without_url()))
            })?;

        let api_response: TelegramResponse<T> = response.json().await.map_err(|e| {
            alms_core::AlmsError::Channel(format!("JSON parse error: {}", e.without_url()))
        })?;

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

    /// Run the polling loop.
    ///
    /// Uses `tokio::select!` with the cancellation token so in-flight
    /// long-poll HTTP requests are dropped immediately on shutdown instead
    /// of blocking for up to 30 seconds.
    async fn run_polling(&self, tx: mpsc::Sender<IncomingMessage>) -> AlmsResult<()> {
        info!("Starting Telegram polling loop");
        let mut consecutive_errors: u64 = 0;

        loop {
            if self.cancel_token.is_cancelled() || !self.running.load(Ordering::Relaxed) {
                break;
            }

            let offset = self.last_update_id.load(Ordering::Relaxed);
            let offset_param = if offset > 0 { Some(offset + 1) } else { None };

            // Race the long-poll against the cancellation token so we can
            // exit without waiting for the full HTTP timeout.
            let updates_result = tokio::select! {
                result = self.get_updates(offset_param) => result,
                _ = self.cancel_token.cancelled() => {
                    info!("Polling cancelled during get_updates");
                    break;
                }
            };

            match updates_result {
                Ok(updates) => {
                    consecutive_errors = 0;
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
                    // Persist offset after processing each batch
                    let current = self.last_update_id.load(Ordering::Relaxed);
                    if current > 0 {
                        self.save_offset(current);
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    let base = std::cmp::min(5 * (1u64 << consecutive_errors.min(6)), 300);
                    // Add jitter: ±25% using subsecond nanos as cheap entropy
                    let jitter_range = (base / 4).max(1);
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .subsec_nanos() as u64;
                    let backoff = base - jitter_range + (nanos % (2 * jitter_range + 1));
                    if consecutive_errors <= 3 {
                        error!(
                            "Error getting updates (attempt {}): {}",
                            consecutive_errors, e
                        );
                    } else {
                        warn!(
                            "Error getting updates (attempt {}, backoff {}s): {}",
                            consecutive_errors, backoff, e
                        );
                    }
                    // Back off with cancellation support via token.
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                        _ = self.cancel_token.cancelled() => {
                            info!("Polling cancelled during error backoff");
                            break;
                        }
                    }
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
        let window = truncate_to_char_boundary(remaining, max_len);

        // Try paragraph break.
        let split_at = window
            .rfind("\n\n")
            .map(|pos| pos + 2) // include the delimiter in the first chunk
            // Try line break.
            .or_else(|| window.rfind('\n').map(|pos| pos + 1))
            // Hard split at char boundary.
            .unwrap_or_else(|| truncate_to_char_boundary(remaining, max_len).len());

        let (chunk, rest) = remaining.split_at(split_at);
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        remaining = rest;
    }

    chunks
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

        self.token = Secret(config.token);
        self.use_webhook = config.use_webhook;
        self.webhook_url = config.webhook_url;
        self.poll_interval_secs = config.poll_interval_secs;

        // Restore persisted offset from previous run (avoids duplicate replies)
        let saved_offset = self.load_offset();
        if saved_offset > 0 {
            self.last_update_id.store(saved_offset, Ordering::Relaxed);
            info!("Restored Telegram update offset: {}", saved_offset);
        }

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
        let handle = tokio::spawn(async move {
            if let Err(e) = channel.run_polling(tx).await {
                error!("Polling error: {}", e);
            }
        });

        // Guard against multiple concurrent polling tasks + store handle
        // under a single lock acquisition to avoid TOCTOU.
        {
            let mut guard = self.poll_handle.lock();
            if guard.is_some() {
                handle.abort();
                return Err(alms_core::AlmsError::Channel(
                    "receive_updates() called while a polling task is already running".to_string(),
                ));
            }
            *guard = Some(handle);
        }

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
        // Cancel the token so the polling loop (including in-flight HTTP
        // requests) exits immediately rather than waiting up to 30 seconds.
        //
        // Note: CancellationToken is one-shot — after stop(), a subsequent
        // receive_updates() call would exit immediately. If restart support
        // is needed in the future, create a fresh token in receive_updates().
        self.cancel_token.cancel();

        // Take the handle out of the mutex (drop the guard before awaiting
        // because parking_lot MutexGuard is !Send).
        let handle = self.poll_handle.lock().take();
        if let Some(handle) = handle {
            // Token cancellation causes run_polling to exit cleanly via
            // tokio::select!, so we just await — no abort() needed.
            let _ = handle.await;
        }

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
        channel.token = Secret("tok123".to_string());
        assert_eq!(
            channel.api_url("sendMessage"),
            "https://api.telegram.org/bottok123/sendMessage"
        );
    }

    // -- Debug redaction tests --

    #[test]
    fn debug_output_redacts_token() {
        let mut channel = TelegramChannel::new();
        channel.token = Secret("SUPER_SECRET_TOKEN_12345".to_string());
        let debug = format!("{:?}", channel);
        assert!(
            !debug.contains("SUPER_SECRET_TOKEN_12345"),
            "Token was leaked in Debug output: {}",
            debug
        );
        assert!(debug.contains("***"), "Debug should contain redacted '***'");
    }

    #[test]
    fn secret_debug_redacts() {
        let s = Secret("my-api-key".to_string());
        assert_eq!(format!("{:?}", s), "***");
    }

    #[test]
    fn secret_as_str_returns_value() {
        let s = Secret("my-api-key".to_string());
        assert_eq!(s.as_str(), "my-api-key");
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

    // -- cancellation / double-call guard tests --

    #[test]
    fn cancel_token_cancelled_after_stop() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let channel = TelegramChannel::new();
            assert!(!channel.cancel_token.is_cancelled());
            // stop() cancels the token even without a running poll task
            channel.stop().await.unwrap();
            assert!(channel.cancel_token.is_cancelled());
        });
    }

    #[test]
    fn poll_handle_none_initially() {
        let channel = TelegramChannel::new();
        assert!(channel.poll_handle.lock().is_none());
    }

    // -- offset persistence tests --

    /// Create a unique temp directory for tests using PID + nanosecond timestamp
    /// to avoid collisions in parallel CI runs.
    fn test_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "alms_test_{}_{}_{nanos}",
            label,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn offset_persist_and_restore() {
        let dir = test_temp_dir("persist");
        let offset_file = dir.join("telegram_offset");

        // Save offset
        let ch = TelegramChannel::new().with_offset_file(offset_file.clone());
        ch.save_offset(42);

        // Restore into a new channel
        let ch2 = TelegramChannel::new().with_offset_file(offset_file.clone());
        assert_eq!(ch2.load_offset(), 42);

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offset_load_missing_file_returns_zero() {
        let ch = TelegramChannel::new()
            .with_offset_file(std::path::PathBuf::from("/nonexistent/path/offset"));
        assert_eq!(ch.load_offset(), 0);
    }

    #[test]
    fn offset_load_no_file_configured_returns_zero() {
        let ch = TelegramChannel::new();
        assert_eq!(ch.load_offset(), 0);
    }

    #[test]
    fn offset_load_corrupted_file_returns_zero() {
        let dir = test_temp_dir("corrupt");
        let offset_file = dir.join("telegram_offset");

        // Write garbage content
        std::fs::write(&offset_file, "not_a_number\n").unwrap();

        let ch = TelegramChannel::new().with_offset_file(offset_file);
        assert_eq!(ch.load_offset(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offset_load_negative_value_clamped_to_zero() {
        let dir = test_temp_dir("negative");
        let offset_file = dir.join("telegram_offset");

        // Write a negative offset (corrupted/malicious)
        std::fs::write(&offset_file, "-5\n").unwrap();

        let ch = TelegramChannel::new().with_offset_file(offset_file);
        assert_eq!(ch.load_offset(), 0);

        let _ = std::fs::remove_dir_all(&dir);
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
