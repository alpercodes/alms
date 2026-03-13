//! ALMS Gateway - Integrated message router
//!
//! Connects channels (Telegram, etc.) to agent runtimes with session management.

use crate::session_queue::SessionQueue;
use alms_channel::telegram::TelegramChannel;
use alms_channel::{Channel, ChannelConfig};
use alms_core::{AgentId, AgentRecord, AlmsConfig, AlmsResult, SessionId};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient};
use alms_session::{SessionConfig, SessionManager, SqliteStore};
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

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
    /// Explicit agent ID (None = resolve from sidecar file or generate new)
    pub agent_id: Option<AgentId>,
    /// Bearer token for API authentication (None = auth disabled)
    pub auth_token: Option<String>,
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
            agent_id: None,
            auth_token: None,
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
                sandbox_root: config.tools.sandbox_root.clone(),
                shell_policy: config.tools.shell_policy.clone(),
                ..AgentConfig::default()
            },
            session_config: SessionConfig::default(),
            db_path: None,
            workspace_dir: None,
            agent_id: None,
            auth_token: config.server.auth_token.clone(),
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

        gateway_config.agent_id = Some(resolve_default_agent_id(Path::new("./data")));

        Ok(gateway_config)
    }
}

/// Resolve the default agent ID: env var > sidecar file > generate new.
///
/// The sidecar file is `<data_dir>/agent_id` — a plain-text UUID.
/// If the file is missing or contains garbage, a new ID is generated and
/// persisted (self-healing). Write failures are non-fatal warnings.
fn resolve_default_agent_id(data_dir: &Path) -> AgentId {
    // 1. Env var override takes highest precedence
    if let Ok(val) = std::env::var("ALMS_AGENT_ID") {
        if let Ok(uuid) = Uuid::parse_str(val.trim()) {
            info!("Using agent ID from ALMS_AGENT_ID: {}", uuid);
            return AgentId(uuid);
        }
        warn!("Invalid ALMS_AGENT_ID '{}', ignoring", val);
    }

    let id_file = data_dir.join("agent_id");

    // 2. Try to load from sidecar file
    if let Ok(contents) = std::fs::read_to_string(&id_file) {
        if let Ok(uuid) = Uuid::parse_str(contents.trim()) {
            info!("Loaded persisted agent ID: {}", uuid);
            return AgentId(uuid);
        }
        warn!(
            "Invalid agent_id file contents '{}', generating new ID",
            contents.trim()
        );
    }

    // 3. Generate new and persist
    let agent_id = AgentId::new();
    if let Err(e) = std::fs::write(&id_file, agent_id.0.to_string()) {
        warn!("Failed to persist agent_id to {}: {}", id_file.display(), e);
    } else {
        info!("Generated and persisted new agent ID: {}", agent_id.0);
    }
    agent_id
}

/// Active channel connections
#[derive(Debug)]
struct Channels {
    telegram: Option<Arc<TelegramChannel>>,
}

/// The ALMS Gateway - orchestrates channels, sessions, and agents
#[derive(Debug)]
pub struct Gateway {
    config: GatewayConfig,
    session_manager: Arc<SessionManager>,
    channels: Channels,
    llm: LlmClient,
    /// Shared default agent ID — updated live when the default agent changes.
    agent_id: Arc<RwLock<AgentId>>,
}

impl Gateway {
    /// Create a new gateway
    pub fn new(config: GatewayConfig) -> AlmsResult<Self> {
        let agent_id = Arc::new(RwLock::new(config.agent_id.unwrap_or_default()));

        let session_manager = match &config.db_path {
            Some(path) => {
                info!("Opening SQLite session store at {}", path);
                let store = SqliteStore::open(path)?;
                // Auto-migrate sidecar agent into the agents registry
                migrate_sidecar_agent(&store, *agent_id.read().unwrap());
                Arc::new(SessionManager::with_store(
                    config.session_config.clone(),
                    store,
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
            agent_id,
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
            self.channels.telegram = Some(Arc::new(telegram));
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

    /// Run the main message processing loop (standalone, no shutdown signal).
    pub async fn run(&mut self) -> AlmsResult<()> {
        let token = CancellationToken::new();
        let queue = Arc::new(SessionQueue::new(token.clone()));
        let result = self.run_until_shutdown(token.clone(), queue).await;
        token.cancel();
        result
    }

    /// Run the message processing loop until the shutdown token is cancelled.
    ///
    /// Messages to the same session are serialized via `session_queue` (FIFO).
    /// Messages to different sessions process concurrently.
    pub async fn run_until_shutdown(
        &mut self,
        token: CancellationToken,
        session_queue: Arc<SessionQueue<SessionId>>,
    ) -> AlmsResult<()> {
        info!("Starting message processing loop (shutdown-aware)");

        let mut telegram_rx: Option<mpsc::Receiver<alms_channel::IncomingMessage>> = None;
        if let Some(ref telegram) = self.channels.telegram {
            telegram_rx = Some(telegram.receive_updates().await?);
        }

        loop {
            tokio::select! {
                Some(msg) = async {
                    if let Some(ref mut rx) = telegram_rx {
                        rx.recv().await
                    } else {
                        None
                    }
                } => {
                    if let Some(ref telegram) = self.channels.telegram {
                        // Read the live default agent ID per message so
                        // set-default changes take effect immediately.
                        let agent_id = self.agent_id();
                        let runtime = Arc::new(AgentRuntime::new(
                            agent_id,
                            self.config.agent_config.clone(),
                            self.llm.clone(),
                        ));
                        let context_id = format!("telegram_{}", msg.chat_id.0);
                        let session = self.session_manager.get_or_create(agent_id, &context_id);
                        let sm = Arc::clone(&self.session_manager);
                        let tg = Arc::clone(telegram);
                        session_queue.enqueue(
                            session.id,
                            Box::pin(async move {
                                process_telegram_message(runtime, sm, tg, msg).await;
                            }),
                        );
                    }
                }
                _ = token.cancelled() => {
                    info!("Shutdown signal received, stopping message loop");
                    break;
                }
                else => {
                    break;
                }
            }
        }

        // Stop channel adapters (Telegram polling)
        self.stop().await?;

        info!("Message processing loop ended");
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
    /// Get a clone of the shared default agent ID handle.
    pub fn agent_id_handle(&self) -> Arc<RwLock<AgentId>> {
        Arc::clone(&self.agent_id)
    }

    /// Read the current default agent ID.
    pub fn agent_id(&self) -> AgentId {
        *self.agent_id.read().unwrap()
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

    /// Get auth token (None = auth disabled)
    pub fn auth_token(&self) -> Option<&str> {
        self.config.auth_token.as_deref()
    }
}

/// Handle a single Telegram message in its own spawned task.
///
/// Takes owned `Arc`s so each message is processed concurrently without
/// blocking the select loop (fixes head-of-line blocking).
async fn process_telegram_message(
    runtime: Arc<AgentRuntime>,
    session_manager: Arc<SessionManager>,
    telegram: Arc<TelegramChannel>,
    msg: alms_channel::IncomingMessage,
) {
    info!("Received message from chat {}: {}", msg.chat_id.0, msg.text);

    let context_id = format!("telegram_{}", msg.chat_id.0);

    match runtime.run(&session_manager, &context_id, &msg.text).await {
        Ok(output) => {
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
        Err(e) => {
            error!("Agent error: {}", e);

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

/// Migrate the sidecar agent ID into the `agents` SQLite table.
///
/// This is a one-time, idempotent migration for existing deployments that used
/// `./data/agent_id` (a plain-text UUID) before multi-agent support was added.
/// If the agents table already has entries, this is a no-op.
///
/// All errors are non-fatal (`warn!` only) — migration must never block startup.
fn migrate_sidecar_agent(store: &SqliteStore, agent_id: AgentId) {
    // If agents table already has entries, skip (already migrated or manually created)
    match store.list_agents() {
        Ok(agents) if !agents.is_empty() => {
            return;
        }
        Err(e) => {
            warn!("Failed to check agents table during migration: {}", e);
            return;
        }
        _ => {}
    }

    let now = chrono::Utc::now();
    let record = AgentRecord {
        id: agent_id,
        name: "main".to_string(),
        description: "Auto-migrated default agent".to_string(),
        model: None,
        system_prompt: None,
        posture: None,
        is_default: true,
        created_at: now,
        last_active: now,
    };

    if let Err(e) = store.create_agent(&record) {
        warn!("Failed to migrate sidecar agent to registry: {}", e);
        return;
    }

    // No separate set_default_agent() needed — record already has is_default: true.
    info!("Migrated sidecar agent to registry: {}", agent_id.0);
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

    #[test]
    fn test_resolve_agent_id_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = resolve_default_agent_id(dir.path());
        let file_contents = std::fs::read_to_string(dir.path().join("agent_id")).unwrap();
        assert_eq!(file_contents, id.0.to_string());
    }

    #[test]
    fn test_resolve_agent_id_loads_existing() {
        let dir = tempfile::tempdir().unwrap();
        let known_uuid = Uuid::new_v4();
        std::fs::write(dir.path().join("agent_id"), known_uuid.to_string()).unwrap();
        let id = resolve_default_agent_id(dir.path());
        assert_eq!(id.0, known_uuid);
    }

    #[test]
    fn test_resolve_agent_id_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = resolve_default_agent_id(dir.path());
        let second = resolve_default_agent_id(dir.path());
        assert_eq!(first, second);
    }

    #[test]
    fn test_resolve_agent_id_overwrites_invalid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent_id"), "not-a-uuid").unwrap();
        let id = resolve_default_agent_id(dir.path());
        // Should have generated a valid ID and overwritten the file
        let file_contents = std::fs::read_to_string(dir.path().join("agent_id")).unwrap();
        assert_eq!(file_contents, id.0.to_string());
    }

    #[test]
    fn test_gateway_uses_config_agent_id() {
        let expected = AgentId::new();
        let config = GatewayConfig {
            agent_id: Some(expected),
            ..GatewayConfig::default()
        };
        let gateway = Gateway::new(config).unwrap();
        assert_eq!(gateway.agent_id(), expected);
    }

    #[test]
    fn test_migrate_creates_default_agent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        migrate_sidecar_agent(&store, agent_id);

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, agent_id);
        assert_eq!(agents[0].name, "main");
        assert!(agents[0].is_default);
    }

    #[test]
    fn test_migrate_idempotent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        migrate_sidecar_agent(&store, agent_id);
        migrate_sidecar_agent(&store, agent_id);

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
    }

    #[test]
    fn test_migrate_preserves_agent_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        migrate_sidecar_agent(&store, agent_id);

        let loaded = store.load_agent_by_id(agent_id).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, agent_id);
    }

    #[test]
    fn test_migrate_skips_when_agents_exist() {
        let store = SqliteStore::open_in_memory().unwrap();
        // Pre-populate with an agent
        let existing_id = AgentId::new();
        let now = chrono::Utc::now();
        let existing = AgentRecord {
            id: existing_id,
            name: "atlas".to_string(),
            description: String::new(),
            model: None,
            system_prompt: None,
            posture: None,
            is_default: true,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&existing).unwrap();

        // Migration with a different agent_id should be a no-op
        let sidecar_id = AgentId::new();
        migrate_sidecar_agent(&store, sidecar_id);

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, existing_id);
    }
}
