//! ALMS Gateway - Integrated message router
//!
//! Connects channels (Telegram, etc.) to agent runtimes with session management.

use crate::session_queue::SessionQueue;
use alms_channel::telegram::TelegramChannel;
use alms_channel::{Channel, ChannelConfig};
use alms_core::{AgentId, AgentRecord, AlmsConfig, AlmsResult, validate_agent_name};
use alms_runtime::{AgentConfig, AgentRuntime, LlmClient};
use alms_session::{SessionConfig, SessionManager, SqliteStore};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, instrument, warn};
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
    /// Absolute path to the data directory (SQLite DB, secrets, etc.).
    /// Propagated as `ALMS_DATA_DIR` to shell_exec so agent CLI commands
    /// find the right database regardless of sandbox cwd.
    pub data_dir: Option<std::path::PathBuf>,
    /// Base directory for agent workspace files (None = workspace API disabled)
    pub workspace_dir: Option<std::path::PathBuf>,
    /// Explicit agent ID (None = resolve from sidecar file or generate new)
    pub agent_id: Option<AgentId>,
    /// Bearer token for API authentication (None = auth disabled)
    pub auth_token: Option<String>,
    /// Logging configuration snapshot — exposed via GET /settings for UI display.
    pub logging_config: alms_core::config::LoggingConfig,
    /// Tools configuration snapshot — timeout and max_output_bytes for UI display.
    pub tools_config: alms_core::config::ToolsConfig,
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
            data_dir: None,
            workspace_dir: None,
            agent_id: None,
            auth_token: None,
            logging_config: alms_core::config::LoggingConfig::default(),
            tools_config: alms_core::config::ToolsConfig::default(),
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
                enabled_tools: config.tools.enabled.clone(),
                ..AgentConfig::default()
            },
            session_config: SessionConfig {
                idle_timeout_secs: config.session.idle_timeout_secs,
                auto_archive: config.session.auto_archive,
                archive_ttl_secs: config.session.archive_ttl_secs,
                max_messages: config.session.max_messages,
                max_context_tokens: config.session.max_context_tokens,
            },
            db_path: None,
            data_dir: None,
            workspace_dir: None,
            agent_id: None,
            auth_token: config.server.auth_token.clone(),
            logging_config: config.logging.clone(),
            tools_config: config.tools.clone(),
        }
    }

    /// Build GatewayConfig from a pre-loaded AlmsConfig, applying
    /// environment-variable overrides for db_path, workspace_dir, and
    /// agent_id.
    ///
    /// The data directory is resolved from `config.server.data_dir` (which
    /// itself respects the `ALMS_DATA_DIR` env var). Individual paths can
    /// still be overridden with `ALMS_DB_PATH` and `ALMS_WORKSPACE_DIR`.
    pub fn from_alms_config_with_env(config: &AlmsConfig) -> Self {
        let mut gateway_config = Self::from_alms_config(config);

        let data_dir = &config.server.data_dir;

        gateway_config.db_path = Some(config.server.db_path());
        gateway_config.workspace_dir = Some(config.server.workspace_dir());

        // Ensure data dir exists before SQLite tries to open files there.
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            tracing::warn!("Could not create data directory {}: {}", data_dir, e);
        }

        // data_dir is already resolved to an absolute path by
        // AlmsConfig::load(). Store it for shell_exec env injection.
        gateway_config.data_dir = Some(std::path::PathBuf::from(data_dir));

        gateway_config.agent_id = Some(resolve_default_agent_id(Path::new(data_dir)));

        gateway_config
    }

    /// Load from environment using the unified config system.
    ///
    /// This calls `AlmsConfig::load()` internally. If you already have a
    /// loaded config, prefer `from_alms_config_with_env()` to avoid
    /// parsing config twice.
    pub fn from_env() -> AlmsResult<Self> {
        let config = AlmsConfig::load()?;
        Ok(Self::from_alms_config_with_env(&config))
    }
}

/// Resolve the default agent ID: env var > sidecar file > generate new.
///
/// The sidecar file is `<data_dir>/agent_id` — a plain-text UUID.
/// If the file is missing or contains garbage, a new ID is generated and
/// persisted (self-healing). Write failures are non-fatal warnings.
/// Whether the agent ID was loaded from an existing sidecar file (true)
/// or freshly generated (false). Used to skip auto-migration on first run.
static SIDECAR_EXISTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[instrument]
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
            SIDECAR_EXISTED.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// A Telegram bot bound to a specific agent.
#[derive(Debug)]
struct AgentTelegramBot {
    agent_id: AgentId,
    agent_name: String,
    channel: Arc<TelegramChannel>,
}

/// The ALMS Gateway - orchestrates channels, sessions, and agents
#[derive(Debug)]
pub struct Gateway {
    config: GatewayConfig,
    session_manager: Arc<SessionManager>,
    /// Per-agent Telegram bots. Each entry is a dedicated bot polling loop
    /// bound to one agent. Messages from each bot route to its owning agent.
    telegram_bots: Vec<AgentTelegramBot>,
    llm: LlmClient,
    /// Shared default agent ID — updated live when the default agent changes.
    agent_id: Arc<RwLock<AgentId>>,
    /// Live secrets store — shared with AppState so runtime key changes are
    /// visible to the Telegram handler (and any other non-HTTP paths).
    secrets: Arc<RwLock<alms_core::secrets::SecretsStore>>,
}

impl Gateway {
    /// Create a new gateway
    pub fn new(config: GatewayConfig) -> AlmsResult<Self> {
        let agent_id = Arc::new(RwLock::new(config.agent_id.unwrap_or_default()));

        let session_manager = match &config.db_path {
            Some(path) => {
                info!("Opening SQLite session store at {}", path);
                let store = SqliteStore::open(path)?;
                // Auto-migrate sidecar agent into the agents registry — only
                // if a sidecar file existed (actual migration, not first run).
                if SIDECAR_EXISTED.load(std::sync::atomic::Ordering::Relaxed) {
                    migrate_sidecar_agent(&store, *agent_id.read());
                }
                Arc::new(SessionManager::with_store(
                    config.session_config.clone(),
                    store,
                )?)
            }
            None => Arc::new(SessionManager::new(config.session_config.clone())),
        };
        // Load secrets store — used both for initial key resolution and shared
        // with AppState so runtime key changes are visible everywhere.
        let secrets_path = alms_core::secrets::secrets_path_from_db(config.db_path.as_deref());
        let secrets_store =
            alms_core::secrets::SecretsStore::load(&secrets_path).unwrap_or_else(|e| {
                warn!("Failed to load secrets: {e}");
                alms_core::secrets::SecretsStore::empty()
            });

        // Resolve API key from secrets store (the only source — no env var fallback).
        let mut llm_config = config.llm_config.clone();
        if let Some(key) = secrets_store.resolve_key(&llm_config.provider) {
            llm_config.api_key = key;
        }
        let llm = LlmClient::new(llm_config)?;

        let secrets = Arc::new(RwLock::new(secrets_store));

        Ok(Self {
            config,
            session_manager,
            telegram_bots: Vec::new(),
            llm,
            agent_id,
            secrets,
        })
    }

    /// Create from environment
    pub fn from_env() -> AlmsResult<Self> {
        Self::new(GatewayConfig::from_env()?)
    }

    /// Initialize Telegram channels.
    ///
    /// Spawns a dedicated polling loop for each agent that has a `telegram_token`
    /// configured in the registry. Falls back to the global telegram token from
    /// the secrets store (`alms auth set telegram <token>`) for the default
    /// agent if no per-agent tokens are found.
    pub async fn initialize_channels(&mut self) -> AlmsResult<()> {
        // Phase 1: Collect per-agent Telegram tokens from the agent registry.
        let mut agent_tokens: Vec<(AgentId, String, String)> = Vec::new(); // (id, name, token)

        if let Some(store) = self.session_manager.store() {
            match store.agents_with_telegram() {
                Ok(agents) => {
                    for agent in agents {
                        if let Some(ref token) = agent.telegram_token {
                            agent_tokens.push((agent.id, agent.name.clone(), token.clone()));
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to load agents with Telegram tokens: {e}");
                }
            }
        }

        // Phase 2: If no per-agent tokens found, fall back to the global
        // telegram token from secrets store (set via `alms auth set telegram <token>`).
        if agent_tokens.is_empty() {
            let global_token = self
                .config
                .telegram_token
                .clone()
                .or_else(|| self.secrets.read().resolve_key("telegram"));
            if let Some(token) = global_token {
                let default_id = self.agent_id();
                let default_name = self
                    .session_manager
                    .store()
                    .and_then(|store| store.load_agent_by_id(default_id).ok().flatten())
                    .map(|r| r.name)
                    .unwrap_or_else(|| "default".to_string());
                agent_tokens.push((default_id, default_name, token));
                info!(
                    "No per-agent Telegram tokens found, using global telegram token from secrets store for default agent"
                );
            }
        }

        if agent_tokens.is_empty() {
            info!("No Telegram tokens configured, skipping Telegram channels");
            return Ok(());
        }

        // Phase 2b: Migrate legacy session context IDs.
        //
        // Old format: `telegram_{chat_id}` -> new: `telegram_{agent_name}_{chat_id}`.
        // Migration is idempotent and non-fatal.
        if let Some(store) = self.session_manager.store() {
            for (_agent_id, agent_name, _token) in &agent_tokens {
                match store.migrate_telegram_context_ids(agent_name) {
                    Ok(0) => {}
                    Ok(n) => info!(
                        "Migrated {n} legacy Telegram session(s) to new context ID format \
                         for agent '{agent_name}'"
                    ),
                    Err(e) => warn!(
                        "Failed to migrate Telegram session context IDs for agent \
                         '{agent_name}': {e}"
                    ),
                }
            }
        }

        // Phase 3: Initialize a TelegramChannel for each token.
        for (agent_id, agent_name, token) in agent_tokens {
            info!(
                "Initializing Telegram channel for agent '{}' ({})",
                agent_name, agent_id
            );
            let mut telegram = TelegramChannel::new();

            // Persist update offset per agent alongside the DB file
            // e.g. ./data/telegram_offset_{agent_name}
            if let Some(ref db_path) = self.config.db_path {
                let data_dir = std::path::Path::new(db_path)
                    .parent()
                    .unwrap_or(std::path::Path::new("."));
                telegram = telegram
                    .with_offset_file(data_dir.join(format!("telegram_offset_{agent_name}")));
            }

            let channel_config = ChannelConfig {
                token: token.clone(),
                use_webhook: false,
                webhook_url: None,
                poll_interval_secs: 5,
                extra: Default::default(),
            };

            match telegram.initialize(channel_config).await {
                Ok(()) => {
                    self.telegram_bots.push(AgentTelegramBot {
                        agent_id,
                        agent_name: agent_name.clone(),
                        channel: Arc::new(telegram),
                    });
                    info!("Telegram channel initialized for agent '{}'", agent_name);
                }
                Err(e) => {
                    // Non-fatal: skip this bot but continue with others.
                    error!(
                        "Failed to initialize Telegram channel for agent '{}': {}",
                        agent_name, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Start the gateway
    pub async fn start(&mut self) -> AlmsResult<()> {
        info!("Starting ALMS Gateway");

        // Start all Telegram channels (non-fatal per bot — log and continue)
        for bot in &self.telegram_bots {
            if let Err(e) = bot.channel.start().await {
                error!(
                    "Failed to start Telegram channel for agent '{}': {}",
                    bot.agent_name, e
                );
                continue;
            }
            info!("Telegram channel started for agent '{}'", bot.agent_name);
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
    /// All runs for the same agent are serialized via `agent_queue` (FIFO).
    /// Different agents process concurrently.
    ///
    /// Per-agent Telegram bots each have their own polling loop. Messages from
    /// all bots are merged into a single channel tagged with the owning agent,
    /// so routing is deterministic: bot X's messages always go to agent X.
    pub async fn run_until_shutdown(
        &mut self,
        token: CancellationToken,
        agent_queue: Arc<SessionQueue<AgentId>>,
    ) -> AlmsResult<()> {
        info!("Starting message processing loop (shutdown-aware)");

        // Merge all per-agent Telegram receivers into a single tagged channel.
        // Each item is (agent_id, agent_name, telegram_channel, message).
        let (merged_tx, mut merged_rx) = mpsc::channel::<(
            AgentId,
            String,
            Arc<TelegramChannel>,
            alms_channel::IncomingMessage,
        )>(256);

        for bot in &self.telegram_bots {
            let mut rx = match bot.channel.receive_updates().await {
                Ok(rx) => rx,
                Err(e) => {
                    error!(
                        "Failed to receive updates for agent '{}': {} — skipping bot",
                        bot.agent_name, e
                    );
                    continue;
                }
            };
            let tx = merged_tx.clone();
            let agent_id = bot.agent_id;
            let agent_name = bot.agent_name.clone();
            let channel = Arc::clone(&bot.channel);

            // Forward messages from this bot's receiver into the merged channel,
            // tagging each with the owning agent.
            let span = tracing::info_span!("telegram_forwarder", agent = %agent_name);
            tokio::spawn(
                async move {
                    while let Some(msg) = rx.recv().await {
                        if tx
                            .send((agent_id, agent_name.clone(), Arc::clone(&channel), msg))
                            .await
                            .is_err()
                        {
                            break; // merged receiver dropped
                        }
                    }
                }
                .instrument(span),
            );
        }
        // Drop our copy so merged_rx closes when all forwarders are done.
        drop(merged_tx);

        loop {
            tokio::select! {
                Some((agent_id, agent_name, telegram, msg)) = merged_rx.recv() => {
                    // Route the message to the owning agent (not the default).
                    let secrets_guard = self.secrets.read();
                    let resolved = crate::runs::resolve_agent_config(
                        agent_id,
                        &self.session_manager,
                        &self.config.agent_config,
                        &self.llm,
                        Some(&secrets_guard),
                    );
                    drop(secrets_guard);
                    // Bootstrap detection: first-time agents get the
                    // bootstrap interview prompt instead of their default.
                    let mut agent_config = resolved.agent_config;
                    if let Some(ws_dir) = &self.config.workspace_dir {
                        let effective_name = resolved
                            .agent_name
                            .as_deref()
                            .unwrap_or(&agent_name);
                        let workspace =
                            alms_runtime::AgentWorkspace::new(ws_dir, effective_name);
                        if workspace.needs_bootstrap() {
                            info!(
                                "Telegram: agent '{}' needs bootstrap — using interview prompt",
                                effective_name
                            );
                            agent_config.system_prompt =
                                alms_runtime::AgentWorkspace::bootstrap_prompt().to_string();
                        }
                    }
                    let mut runtime = match AgentRuntime::new(
                        agent_id,
                        agent_config,
                        resolved.llm,
                    ) {
                        Ok(rt) => rt,
                        Err(e) => {
                            error!("Failed to create agent runtime: {}", e);
                            continue;
                        }
                    };
                    // Set agent name for perspective mapping in DM sessions.
                    let effective_name = resolved
                        .agent_name
                        .clone()
                        .unwrap_or_else(|| agent_name.clone());
                    runtime = runtime.with_agent_name(effective_name.clone());
                    // Inject ALMS_DATA_DIR so CLI commands invoked via
                    // shell_exec find the correct database.
                    {
                        let shell_env = alms_core::build_shell_default_env(
                            self.config.data_dir.as_deref(),
                            self.config.workspace_dir.as_deref(),
                        );
                        if !shell_env.is_empty() {
                            runtime = runtime.with_shell_default_env(shell_env);
                        }
                    }
                    // Attach workspace so agent personality/goals/memories
                    // are prepended to the system prompt (same as HTTP path).
                    if let Some(ws_dir) = &self.config.workspace_dir {
                        let workspace =
                            alms_runtime::AgentWorkspace::new(ws_dir, &effective_name);
                        runtime = runtime.with_workspace(workspace);
                    }
                    let runtime = Arc::new(runtime);
                    let sm = Arc::clone(&self.session_manager);
                    let name_for_ctx = effective_name;
                    agent_queue.enqueue(
                        agent_id,
                        Box::pin(async move {
                            process_telegram_message(
                                runtime,
                                sm,
                                telegram,
                                msg,
                                &name_for_ctx,
                            )
                            .await;
                        }),
                    );
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

        for bot in &self.telegram_bots {
            info!("Stopping Telegram channel for agent '{}'", bot.agent_name);
            if let Err(e) = bot.channel.stop().await {
                error!(
                    "Failed to stop Telegram channel for agent '{}': {}",
                    bot.agent_name, e
                );
            }
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
        *self.agent_id.read()
    }

    /// Get LLM client reference
    pub fn llm(&self) -> &LlmClient {
        &self.llm
    }

    /// Get agent config reference
    pub fn agent_config(&self) -> &AgentConfig {
        &self.config.agent_config
    }

    /// Get the absolute path to the data directory.
    pub fn data_dir(&self) -> Option<&std::path::Path> {
        self.config.data_dir.as_deref()
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

    /// Get session config reference (for exposing server defaults)
    pub fn session_config(&self) -> &SessionConfig {
        &self.config.session_config
    }

    /// Get logging config reference (for exposing server defaults)
    pub fn logging_config(&self) -> &alms_core::config::LoggingConfig {
        &self.config.logging_config
    }

    /// Get tools config reference (for exposing server defaults)
    pub fn tools_config(&self) -> &alms_core::config::ToolsConfig {
        &self.config.tools_config
    }

    /// Get a clone of the shared secrets store handle.
    ///
    /// The returned `Arc` is the same instance used by the Gateway's Telegram
    /// handler, so updates made through AppState's copy are visible here too.
    pub fn secrets_handle(&self) -> Arc<RwLock<alms_core::secrets::SecretsStore>> {
        Arc::clone(&self.secrets)
    }
}

/// Handle a single Telegram message in its own spawned task.
///
/// Takes owned `Arc`s so each message is processed concurrently without
/// blocking the select loop (fixes head-of-line blocking).
///
/// The `agent_name` parameter is used to namespace the session context ID
/// so each agent gets its own conversation history per chat:
/// `telegram_{agent_name}_{chat_id}`.
async fn process_telegram_message(
    runtime: Arc<AgentRuntime>,
    session_manager: Arc<SessionManager>,
    telegram: Arc<TelegramChannel>,
    msg: alms_channel::IncomingMessage,
    agent_name: &str,
) {
    info!(
        "Received message for agent '{}' from chat {}: {}",
        agent_name, msg.chat_id.0, msg.text
    );

    let context_id = format!("telegram_{}_{}", agent_name, msg.chat_id.0);

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
/// Uses `create_agent_if_none_exist` to atomically check-and-insert within a
/// single SQLite transaction, avoiding the TOCTOU race between checking
/// `list_agents().is_empty()` and `create_agent()`.
///
/// All errors are non-fatal (`warn!` only) — migration must never block startup.
#[instrument(skip(store))]
fn migrate_sidecar_agent(store: &SqliteStore, agent_id: AgentId) {
    let migration_name = "main";
    if let Err(e) = validate_agent_name(migration_name) {
        warn!(
            "Migration agent name '{}' fails validation: {}",
            migration_name, e
        );
        return;
    }

    let now = chrono::Utc::now();
    let record = AgentRecord {
        id: agent_id,
        name: migration_name.to_string(),
        description: "Auto-migrated default agent".to_string(),
        model: None,
        posture: None,
        provider: None,
        telegram_token: None,
        is_default: false,
        created_at: now,
        last_active: now,
    };

    // Atomic check-and-insert: only inserts if the agents table is empty.
    // Returns Ok(false) if agents already exist (no-op), avoiding the TOCTOU
    // race that existed when list_agents() and create_agent() were separate calls.
    // The agent is marked as default within the same transaction.
    match store.create_agent_if_none_exist(&record) {
        Ok(true) => info!("Migrated sidecar agent to registry: {}", agent_id.0),
        Ok(false) => {} // agents already exist, nothing to migrate
        Err(e) => warn!("Failed to migrate sidecar agent to registry: {}", e),
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
    fn test_migrate_name_passes_validation() {
        // The hardcoded migration name "main" must pass validate_agent_name.
        // If "main" is ever added to the reserved-names list, this test will
        // catch the breakage before it reaches production.
        assert!(validate_agent_name("main").is_ok());
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
            posture: None,
            provider: None,
            telegram_token: None,
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
