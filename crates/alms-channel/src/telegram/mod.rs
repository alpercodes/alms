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

    /// Send a raw message via the API
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
        let request = SendMessageRequest::new(message.chat_id.0, message.text).parse_mode("HTML");

        let request = if let Some(reply_to) = message.reply_to {
            request.reply_to(reply_to.0)
        } else {
            request
        };

        let sent: SentMessage = self.post("sendMessage", &request).await?;
        Ok(MessageId(sent.message_id))
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
