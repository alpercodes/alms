//! ALMS Gateway - Integrated message router
//!
//! Connects channels (Telegram, etc.) to agent runtimes with session management.

use alms_channel::telegram::TelegramChannel;
use alms_channel::{Channel, ChannelConfig};
use alms_core::{AgentId, AlmsConfig, AlmsResult};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient};
use alms_session::{SessionConfig, SessionManager};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Gateway configuration
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Telegram bot token
    pub telegram_token: Option<String>,
    /// LLM configuration
    pub llm_config: alms_runtime::LlmConfig,
    /// Agent configuration
    pub agent_config: AgentConfig,
    /// Session configuration
    pub session_config: SessionConfig,
    /// Path to SQLite database file (None = in-memory only, not persisted)
    pub db_path: Option<String>,
    /// Base directory for agent workspace files (None = workspace API disabled)
    pub workspace_dir: Option<std::path::PathBuf>,
}

#[allow(clippy::derivable_impls)]
impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            telegram_token: None,
            llm_config: alms_runtime::LlmConfig::default(),
            agent_config: AgentConfig::default(),
            session_config: SessionConfig::default(),
            db_path: None,
            workspace_dir: None,
        }
    }
}

impl GatewayConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_telegram_token(mut self, token: impl Into<String>) -> Self {
        self.telegram_token = Some(token.into());
        self
    }

    /// Build GatewayConfig from the unified AlmsConfig.
    /// This is the preferred way to construct a GatewayConfig.
    pub fn from_alms_config(config: &AlmsConfig) -> Self {
        Self {
            telegram_token: config.channels.telegram_token.clone(),
            llm_config: config.llm.clone().into(),
            agent_config: AgentConfig {
                context_config: config.context.clone(),
                ..AgentConfig::default()
            },
            session_config: SessionConfig::default(),
            db_path: None,
            workspace_dir: None,
        }
    }

    /// Load from environment using the unified config system.
    ///
    /// Defaults to `./data/alms.db` for SQLite persistence and
    /// `./data/workspace` for agent workspace files. Override with
    /// `ALMS_DB_PATH` and `ALMS_WORKSPACE_DIR` env vars.
    pub fn from_env() -> AlmsResult<Self> {
        let config = AlmsConfig::load()?;
        let mut gateway_config = Self::from_alms_config(&config);

        gateway_config.db_path =
            Some(std::env::var("ALMS_DB_PATH").unwrap_or_else(|_| "./data/alms.db".to_string()));
        gateway_config.workspace_dir = Some(
            std::env::var("ALMS_WORKSPACE_DIR")
                .map(Into::into)
                .unwrap_or_else(|_| std::path::PathBuf::from("./data/workspace")),
        );

        // Ensure ./data/ exists before SQLite tries to open files there.
        if let Err(e) = std::fs::create_dir_all("./data") {
            tracing::warn!("Could not create ./data directory: {}", e);
        }

        Ok(gateway_config)
    }
}

/// Active channel connections
#[derive(Debug)]
struct Channels {
    telegram: Option<TelegramChannel>,
}

/// The ALMS Gateway - orchestrates channels, sessions, and agents
#[derive(Debug)]
pub struct Gateway {
    config: GatewayConfig,
    session_manager: Arc<SessionManager>,
    channels: Channels,
    llm: LlmClient,
    agent_id: AgentId,
}

impl Gateway {
    /// Create a new gateway
    pub fn new(config: GatewayConfig) -> AlmsResult<Self> {
        let session_manager = match &config.db_path {
            Some(path) => {
                info!("Opening SQLite session store at {}", path);
                Arc::new(SessionManager::with_sqlite(
                    config.session_config.clone(),
                    path,
                )?)
            }
            None => Arc::new(SessionManager::new(config.session_config.clone())),
        };
        let llm = LlmClient::new(config.llm_config.clone())?;

        Ok(Self {
            config,
            session_manager,
            channels: Channels { telegram: None },
            llm,
            agent_id: AgentId::new(),
        })
    }

    /// Create from environment
    pub fn from_env() -> AlmsResult<Self> {
        Self::new(GatewayConfig::from_env()?)
    }

    /// Initialize channels
    pub async fn initialize_channels(&mut self) -> AlmsResult<()> {
        // Initialize Telegram if token is configured
        if let Some(ref token) = self.config.telegram_token {
            info!("Initializing Telegram channel");
            let mut telegram = TelegramChannel::new();

            let channel_config = ChannelConfig {
                token: token.clone(),
                use_webhook: false,
                webhook_url: None,
                poll_interval_secs: 5,
                extra: Default::default(),
            };

            telegram.initialize(channel_config).await?;
            self.channels.telegram = Some(telegram);
            info!("Telegram channel initialized");
        } else {
            warn!("No Telegram token configured, skipping Telegram channel");
        }

        Ok(())
    }

    /// Start the gateway
    pub async fn start(&mut self) -> AlmsResult<()> {
        info!("Starting ALMS Gateway");

        // Start Telegram channel
        if let Some(ref telegram) = self.channels.telegram {
            telegram.start().await?;
            info!("Telegram channel started");
        }

        Ok(())
    }

    /// Run the main message processing loop
    pub async fn run(&mut self) -> AlmsResult<()> {
        info!("Starting message processing loop");

        // Start receiving messages from Telegram
        let mut telegram_rx: Option<mpsc::Receiver<alms_channel::IncomingMessage>> = None;
        if let Some(ref telegram) = self.channels.telegram {
            telegram_rx = Some(telegram.receive_updates().await?);
        }

        // Create agent runtime
        let runtime = AgentRuntime::new(
            self.agent_id,
            self.config.agent_config.clone(),
            self.llm.clone(),
        );

        loop {
            tokio::select! {
                // Handle Telegram messages
                Some(msg) = async {
                    if let Some(ref mut rx) = telegram_rx {
                        rx.recv().await
                    } else {
                        None
                    }
                } => {
                    if let Err(e) = self.handle_message(&runtime, msg).await {
                        error!("Error handling message: {}", e);
                    }
                }

                // Add other channel handlers here as needed

                else => {
                    // All channels closed
                    break;
                }
            }
        }

        info!("Message processing loop ended");
        Ok(())
    }

    /// Handle an incoming message
    async fn handle_message(
        &self,
        runtime: &AgentRuntime,
        msg: alms_channel::IncomingMessage,
    ) -> AlmsResult<()> {
        info!("Received message from chat {}: {}", msg.chat_id.0, msg.text);

        // Use chat_id as context_id for session management
        let context_id = format!("telegram_{}", msg.chat_id.0);

        // Run the agent
        match runtime
            .run(&self.session_manager, &context_id, &msg.text)
            .await
        {
            Ok(output) => {
                // Send response back via Telegram
                if let Some(ref telegram) = self.channels.telegram {
                    let outgoing = alms_channel::OutgoingMessage {
                        chat_id: msg.chat_id,
                        text: output.response,
                        reply_to: Some(msg.message_id),
                        options: Default::default(),
                    };

                    if let Err(e) = telegram.send_message(outgoing).await {
                        error!("Failed to send response: {}", e);
                    } else {
                        info!("Response sent successfully");
                    }
                }
            }
            Err(e) => {
                error!("Agent error: {}", e);

                // Send error message back to user
                if let Some(ref telegram) = self.channels.telegram {
                    let outgoing = alms_channel::OutgoingMessage {
                        chat_id: msg.chat_id,
                        text: "Sorry, I encountered an error processing your message.".to_string(),
                        reply_to: Some(msg.message_id),
                        options: Default::default(),
                    };

                    let _ = telegram.send_message(outgoing).await;
                }
            }
        }

        Ok(())
    }

    /// Stop the gateway
    pub async fn stop(&mut self) -> AlmsResult<()> {
        info!("Stopping ALMS Gateway");

        if let Some(ref telegram) = self.channels.telegram {
            telegram.stop().await?;
        }

        Ok(())
    }

    /// Get session manager reference
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get agent ID
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// Get LLM client reference
    pub fn llm(&self) -> &LlmClient {
        &self.llm
    }

    /// Get agent config reference
    pub fn agent_config(&self) -> &AgentConfig {
        &self.config.agent_config
    }

    /// Get workspace base directory (None = workspace API disabled)
    pub fn workspace_dir(&self) -> Option<&std::path::Path> {
        self.config.workspace_dir.as_deref()
    }

    /// Get LLM config reference (for exposing server defaults)
    pub fn llm_config(&self) -> &alms_runtime::LlmConfig {
        &self.config.llm_config
    }

    /// Get SQLite database path (None = in-memory only)
    pub fn db_path(&self) -> Option<&str> {
        self.config.db_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_config_default() {
        let config = GatewayConfig::default();
        assert!(config.telegram_token.is_none());
    }

    #[test]
    fn test_gateway_config_with_token() {
        let config = GatewayConfig::new().with_telegram_token("test_token");

        assert_eq!(config.telegram_token, Some("test_token".to_string()));
    }
}
