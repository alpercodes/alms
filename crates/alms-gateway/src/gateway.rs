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
    /// Absolute path to the project root — the agent's filesystem sandbox
    /// boundary (#945). Resolved at gateway construction time from
    /// [`ServerConfig::resolved_project_root`]; the CLI `--project` flag
    /// writes through to that field before this struct is built.
    pub project_root: Option<std::path::PathBuf>,
    /// Base directory for agent workspace files (None = workspace API
    /// disabled). After #945 this is `<project_root>/.alms/agents/` —
    /// flat, sibling to `alms.db` rather than nested under
    /// `<data_dir>/workspace/`.
    pub workspace_dir: Option<std::path::PathBuf>,
    /// Explicit agent ID (None = resolve from sidecar file or generate new)
    pub agent_id: Option<AgentId>,
    /// Bearer token for API authentication (None = auth disabled)
    pub auth_token: Option<String>,
    /// Logging configuration snapshot — exposed via GET /settings for UI display.
    pub logging_config: alms_core::config::LoggingConfig,
    /// Tools configuration snapshot — timeout and max_output_bytes for UI display.
    pub tools_config: alms_core::config::ToolsConfig,
    /// Security configuration snapshot (#947) — config-file-only,
    /// loaded once at boot, never mutated by `PATCH /settings`. Used to
    /// resolve the `allow_full_os_access` list at run-start to decide
    /// whether to attach the project-root sandbox to a new
    /// [`AgentRuntime`].
    pub security_config: alms_core::config::SecurityConfig,
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
            project_root: None,
            workspace_dir: None,
            agent_id: None,
            auth_token: None,
            logging_config: alms_core::config::LoggingConfig::default(),
            tools_config: alms_core::config::ToolsConfig::default(),
            security_config: alms_core::config::SecurityConfig::default(),
        }
    }
}

impl GatewayConfig {
    pub fn new() -> Self {
        Self::default()
    }

    // TODO(dead-code): test-only builder — consider gating behind #[cfg(test)]
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
                // Agent-loop hard caps (#987 / B3 / #1150): bound iteration
                // count, the absolute wall-clock backstop, and the phase-aware
                // inactivity budgets (between-iterations + tool-phase ceiling)
                // so a run that stops making progress terminates instead of
                // hanging forever.
                max_iterations: config.llm.max_iterations,
                max_run_duration_secs: config.llm.max_run_duration_secs,
                between_iterations_secs: config.llm.between_iterations_secs,
                tool_phase_ceiling_secs: config.llm.tool_phase_ceiling_secs,
                sandbox_root: config.tools.sandbox_root.clone(),
                shell_policy: config.tools.shell_policy.clone(),
                shell_permissions: config.tools.shell_permissions.clone(),
                shell_classification_mode: config.tools.shell_classification_mode,
                shell_spill: config.tools.shell_spill.clone(),
                tool_output_truncate: config.tools.tool_output_truncate.clone(),
                enabled_tools: config.tools.enabled.clone(),
                fs_edit_fuzzy_match: config.tools.fs_edit.fuzzy_match,
                // Server-default extended-thinking budget — can be
                // overridden per-agent in the registry.
                anthropic_thinking_budget: config.llm.anthropic.thinking_budget_tokens,
                // Anthropic prompt caching (#766) — server-level only,
                // no per-agent / per-run override per issue #766.
                anthropic_prompt_cache_enabled: config.llm.anthropic.prompt_cache_enabled,
                // Server-default OpenAI-compat reasoning effort (#768) —
                // two-layer precedence (per-agent > server).
                openai_reasoning_effort: config.llm.openai.reasoning_effort,
                // Server-default Gemini thinking budget (#769) — two-layer
                // precedence mirrors Anthropic/OpenAI. Silently ignored
                // when the effective provider is not Gemini.
                gemini_thinking_budget: config.llm.gemini.thinking_budget,
                // Gemini context caching (#769) — server-level only,
                // no per-agent / per-run override per issue #769.
                gemini_cache_enabled: config.llm.gemini.cache_enabled,
                gemini_cache_ttl_seconds: config.llm.gemini.cache_ttl_seconds,
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
            project_root: None,
            workspace_dir: None,
            agent_id: None,
            auth_token: config.server.auth_token.clone(),
            logging_config: config.logging.clone(),
            tools_config: config.tools.clone(),
            security_config: config.security.clone(),
        }
    }

    /// Build GatewayConfig from a pre-loaded AlmsConfig, applying
    /// environment-variable overrides for db_path, workspace_dir,
    /// project_root, and agent_id.
    ///
    /// The data directory is resolved from `config.server.data_dir` (which
    /// itself respects the `ALMS_DATA_DIR` env var). Individual paths can
    /// still be overridden with `ALMS_DB_PATH` and `ALMS_WORKSPACE_DIR`.
    /// The project root (#945) follows its own precedence chain via
    /// [`ServerConfig::resolved_project_root`].
    pub fn from_alms_config_with_env(config: &AlmsConfig) -> Self {
        let mut gateway_config = Self::from_alms_config(config);

        let data_dir = &config.server.data_dir;

        gateway_config.db_path = Some(config.server.db_path());

        // Ensure data dir exists before SQLite tries to open files there.
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            tracing::warn!("Could not create data directory {}: {}", data_dir, e);
        }

        // data_dir is already resolved to an absolute path by
        // AlmsConfig::load(). Store it for shell_exec env injection.
        gateway_config.data_dir = Some(std::path::PathBuf::from(data_dir));

        // Resolve the project root (#945) and ensure it exists. This is the
        // single source of truth for the agent's filesystem-sandbox
        // boundary in v2 — fs_*, shell, fs_grep, fs_glob all share it.
        let project_root = config.server.resolved_project_root();
        if let Err(e) = std::fs::create_dir_all(&project_root) {
            tracing::warn!(
                "Could not create project root {}: {}",
                project_root.display(),
                e
            );
        }
        gateway_config.project_root = Some(project_root);

        // Agent metadata directory (#945): `<project_root>/.alms/agents/`.
        // Workspace files for each agent live in `<agents_dir>/<name>/`,
        // flat and sibling to `alms.db` instead of nested under the data
        // directory.
        gateway_config.workspace_dir = Some(config.server.agents_dir());

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

/// Emit a structured WARN for every agent named in
/// [`SecurityConfig::allow_full_os_access`][alms_core::config::SecurityConfig::allow_full_os_access]
/// (#947).
///
/// Called once from [`Gateway::start`] so operators see the loosened
/// sandbox at boot. A matching per-run WARN fires from
/// `runs/lifecycle.rs::execute_run` so log scanners can correlate runs
/// against the listed agents on long-lived daemons too.
///
/// Extracted as a free function (not a method) so unit tests can call
/// it directly under a custom tracing subscriber without spinning up a
/// full `Gateway` instance — see `boot_warn_emits_for_each_listed_agent`
/// in this file's tests.
pub(crate) fn warn_full_os_access_at_boot(security_config: &alms_core::config::SecurityConfig) {
    for agent_name in &security_config.allow_full_os_access {
        warn!(
            target: "alms.security",
            agent_name = %agent_name,
            allow_full_os_access = true,
            "Agent '{}' is in [security].allow_full_os_access — runs will execute \
             WITHOUT the project-root filesystem sandbox. shell_permissions and \
             the destructive-command classifier still apply. Worktree-mode (when \
             configured) is silently ignored for this agent.",
            agent_name,
        );
    }
}

/// Emit a boot-time `WARN` for every agent that has both
/// `worktree_mode = "git"` AND is on `[security].allow_full_os_access`
/// (#946 + #947 precedence overlap).
///
/// At runtime the security list wins — the agent runs without any
/// filesystem sandbox even though it has a worktree provisioned. The
/// worktree itself stays on disk so the operator can flip the
/// security knob off later without re-running `git worktree add`. We
/// surface the conflict at boot so operators see it on every restart,
/// not just on the per-run WARN that fires from the runs lifecycle.
///
/// Best-effort: when the SQLite store is unavailable (in-memory test
/// builds, missing data dir) the function is a no-op.
pub(crate) fn warn_worktree_and_full_os_access_overlap_at_boot(
    store: Option<&std::sync::Arc<alms_session::SqliteStore>>,
    security_config: &alms_core::config::SecurityConfig,
) {
    if security_config.allow_full_os_access.is_empty() {
        return;
    }
    let Some(store) = store else {
        return;
    };
    let agents = match store.list_agents() {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "Could not list agents for worktree/full-os-access overlap WARN");
            return;
        }
    };
    for agent in agents {
        if agent.worktree_mode == alms_core::WorktreeMode::Git
            && security_config.is_full_os_access_agent(&agent.name)
        {
            warn!(
                target: "alms.security",
                agent_name = %agent.name,
                allow_full_os_access = true,
                worktree_mode = "git",
                "Agent '{}' has worktree_mode=git AND is on [security].allow_full_os_access. \
                 At runtime the security list wins — the agent will run WITHOUT any \
                 filesystem sandbox. The worktree at <project>/.alms/worktrees/{}/ \
                 stays on disk so the operator can flip the security knob off later \
                 without re-creating it.",
                agent.name,
                agent.name,
            );
        }
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
                let recovered = store.mark_stale_runs_failed()?;
                if recovered > 0 {
                    info!(
                        recovered,
                        "Recovered stale runs and released their pending inputs before session hydration"
                    );
                }
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

        // Resolve API key. Precedence:
        //   1. SecretsStore (`alms auth set <provider> <key>`) — highest,
        //      because runtime operator changes should always win.
        //   2. The provider entry's `api_key_env` / `api_key` fields, for
        //      configs that wire a generic OpenAI-compatible provider
        //      declaratively in `alms.toml`.
        let mut llm_config = config.llm_config.clone();
        if let Some(key) = secrets_store.resolve_key(&llm_config.provider) {
            llm_config.api_key = key;
        } else if let Some(entry) = llm_config.providers.get(&llm_config.provider).cloned()
            && let Some(key) = entry.resolve_api_key()
        {
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
            // e.g. .alms/telegram_offset_{agent_name}
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

        // Boot-time WARN for every agent named in
        // `[security].allow_full_os_access` (#947). Operators see each
        // listed agent on every boot — log scanners can correlate these
        // structured fields against runs. The matching run-start WARN
        // fires per-run from the runs lifecycle so a long-lived daemon
        // continues to surface the policy at run granularity.
        warn_full_os_access_at_boot(&self.config.security_config);

        // Boot-time WARN for every agent that has BOTH
        // `worktree_mode = "git"` (#946) AND is on
        // `[security].allow_full_os_access`. The security list always
        // wins at runtime — the worktree exists on disk (so flipping
        // the security knob off restores the worktree sandbox without
        // a re-create) but the run-time sandbox attachment is
        // skipped. Surface the precedence at boot so operators can
        // catch the conflict in the daemon's first 200 lines.
        warn_worktree_and_full_os_access_overlap_at_boot(
            self.session_manager.store(),
            &self.config.security_config,
        );

        // Resolve the shell tool's interpreter once at boot and log it at
        // `target = "alms.security"` (#1121). Installs the config-file-only
        // `[tools].shell_path` override when set, plus the `[tools].shell_engine`
        // selection (#1143 — `builtin` re-execs `alms shell-host` instead of
        // resolving an external bash). On Windows this also surfaces
        // "Git Bash not found" in the daemon's first log lines instead of
        // letting the first agent run discover it from a failed tool call.
        alms_runtime::init_shell_resolution(
            self.config.tools_config.shell_path.clone(),
            self.config.tools_config.shell_engine,
        );

        // Shell output spill retention sweep (issue #756). Runs once at
        // startup — no background ticker — so expired `.alms/shell_output/*`
        // files are cleaned up the next time the gateway restarts rather
        // than growing unbounded. Failures are non-fatal and logged.
        if let Some(ref data_dir) = self.config.data_dir {
            let retention_days = self.config.tools_config.shell_spill.retention_days;
            match alms_runtime::spill::sweep_expired(data_dir, retention_days) {
                Ok(deleted) => {
                    if deleted > 0 {
                        info!(
                            deleted,
                            retention_days,
                            "Cleaned up expired shell output spill files at startup"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        data_dir = %data_dir.display(),
                        "Shell output spill retention sweep failed at startup"
                    );
                }
            }

            // In-loop tool-output spill retention sweep (issue #851).
            // Same lifecycle as the shell-output sweep above — startup-only,
            // filesystem-mtime check, non-fatal on error. Removes expired
            // `.alms/tool-output/*` files.
            let trunc_retention = self.config.tools_config.tool_output_truncate.retention_days;
            match alms_runtime::tool_output_truncate::sweep_expired(data_dir, trunc_retention) {
                Ok(deleted) => {
                    if deleted > 0 {
                        info!(
                            deleted,
                            retention_days = trunc_retention,
                            "Cleaned up expired tool-output spill files at startup"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        data_dir = %data_dir.display(),
                        "Tool-output spill retention sweep failed at startup"
                    );
                }
            }
        }

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
                    //
                    // NOTE: `self.config.agent_config` is a boot-time snapshot
                    // — `Gateway` holds `GatewayConfig` by value and never sees
                    // PATCH /settings mutations. HTTP-triggered runs (and the
                    // Coordinator) share the live `Arc<RwLock<AgentConfig>>` on
                    // `AppState`, so this is a known asymmetry: PATCH /settings
                    // updates to context / session / tools / llm provider
                    // defaults take effect for HTTP runs immediately and for
                    // Telegram runs only after a daemon restart. This is
                    // pre-existing behaviour for the context / session / tools
                    // sections and is documented in `docs/api.md` § 10.2.
                    let resolved = {
                        let secrets_guard = self.secrets.read();
                        match crate::runs::resolve_agent_config(
                            agent_id,
                            &self.session_manager,
                            &self.config.agent_config,
                            &self.llm,
                            Some(&secrets_guard),
                        ) {
                        Ok(r) => r,
                        Err(e) => {
                            // #863: per-agent provider override with no model
                            // on any layer. Telegram has no HTTP response
                            // surface, so log + drop the message — operator
                            // must fix the agent config (PATCH /agents/{id})
                            // before further messages are routable.
                            error!(
                                agent_id = %agent_id,
                                "Telegram: dropping message — {}",
                                e
                            );
                            continue;
                        }
                        }
                    };
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
                    // Pin the agent's sandbox at the project root (#945)
                    // — or, when `worktree_mode = "git"` (#946), at the
                    // agent's dedicated worktree under
                    // `<project>/.alms/worktrees/<name>/`. Mirror the HTTP
                    // path so Telegram-triggered runs see the same model.
                    // Must precede `with_workspace` so the re-registration
                    // of fs_* / shell happens before workspace attachment.
                    //
                    // Precedence: `[security].allow_full_os_access` wins
                    // over both worktree mode and project-root mode. The
                    // worktree (if provisioned) stays on disk — only the
                    // run-time sandbox attachment is bypassed.
                    let full_os_access = self
                        .config
                        .security_config
                        .is_full_os_access_agent(&effective_name);
                    let worktree_mode = resolved.worktree_mode;
                    if full_os_access {
                        warn!(
                            target: "alms.security",
                            agent_name = %effective_name,
                            allow_full_os_access = true,
                            channel = "telegram",
                            worktree_mode = %worktree_mode.as_wire_str(),
                            "Telegram run starting for agent '{}' WITHOUT project-root \
                             filesystem sandbox (allow_full_os_access). shell_permissions \
                             and the destructive-command classifier still apply. \
                             Worktree-mode is silently ignored at runtime.",
                            effective_name,
                        );
                        runtime = runtime.with_unrestricted_filesystem();
                    } else if worktree_mode == alms_core::WorktreeMode::Git
                        && let Some(project_root) = self.config.project_root.clone()
                    {
                        let worktree_dir =
                            alms_core::worktree::worktree_path(&project_root, &effective_name);
                        let sibling_read_root =
                            project_root.join(".alms").join("agents");
                        runtime = runtime
                            .with_extra_fs_read_root(sibling_read_root)
                            .with_project_root(worktree_dir.clone());
                        info!(
                            target: "alms.worktree",
                            agent_name = %effective_name,
                            channel = "telegram",
                            worktree_dir = %worktree_dir.display(),
                            "Telegram run starting under per-agent git worktree (#946)"
                        );
                    } else if let Some(project_root) = self.config.project_root.clone() {
                        runtime = runtime.with_project_root(project_root);
                    }
                    // Attach workspace so agent personality/goals/memories
                    // are appended to the system prompt (same as HTTP path).
                    if let Some(ws_dir) = &self.config.workspace_dir {
                        let workspace =
                            alms_runtime::AgentWorkspace::new(ws_dir, &effective_name);
                        runtime = runtime.with_workspace(workspace);
                    }
                    let runtime = Arc::new(runtime);
                    let sm = Arc::clone(&self.session_manager);
                    let name_for_ctx = effective_name;
                    let Ok(reservation) = agent_queue.reserve(agent_id).await else {
                        break;
                    };
                    if let Err(error) = reservation.submit(Box::pin(async move {
                            process_telegram_message(
                                runtime,
                                sm,
                                telegram,
                                msg,
                                &name_for_ctx,
                            )
                            .await;
                        })) {
                        warn!(
                            ?error,
                            agent_id = %agent_id.0,
                            "Telegram run queue closed before dispatch"
                        );
                        break;
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

    /// Get the absolute path to the project root (#945) — the agent's
    /// filesystem sandbox boundary.
    pub fn project_root(&self) -> Option<&std::path::Path> {
        self.config.project_root.as_deref()
    }

    /// Get workspace base directory (None = workspace API disabled).
    /// After #945 this is `<project_root>/.alms/agents/`.
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

    /// Get the security config reference (#947). Snapshotted at gateway
    /// construction; the field is config-file-only and never mutated at
    /// runtime, so no `Arc<RwLock<_>>` is needed.
    pub fn security_config(&self) -> &alms_core::config::SecurityConfig {
        &self.config.security_config
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
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
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
    fn gateway_restart_releases_queued_input_before_hydrating_history() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("restart-recovery.db");
        let db_path = db_path.to_str().unwrap();
        let agent_id = AgentId::new();
        let session = alms_session::Session::new(agent_id, "restart-recovery");
        let run = alms_core::Run::new(session.id, agent_id, "recover me".to_string());
        let run_id = run.run_id;
        let message = alms_session::Message {
            id: "restart-recovery-input".to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(run.input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": run_id.0.to_string(),
            })),
        };

        {
            let store = SqliteStore::open(db_path).unwrap();
            store.save_session(&session).unwrap();
            store.save_run_with_initial_message(&run, &message).unwrap();
        }

        let config = GatewayConfig {
            db_path: Some(db_path.to_string()),
            ..GatewayConfig::default()
        };
        let gateway = Gateway::new(config).unwrap();

        let history = gateway.session_manager.get_history(session.id).unwrap();
        let input = history
            .iter()
            .find(|candidate| candidate.id == message.id)
            .expect("admitted input must survive restart");
        let metadata = input.metadata.as_ref().unwrap();
        assert_eq!(metadata["pending_input"], false);
        assert!(metadata["input_claimed_at"].is_string());

        let context = gateway
            .session_manager
            .get_context_history(session.id)
            .unwrap();
        let context_text = context
            .iter()
            .find_map(|message| match &message.content {
                alms_session::Content::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("recovered prompt must be context-visible");
        assert_eq!(context_text, "recover me");

        let persisted = gateway
            .session_manager
            .store()
            .unwrap()
            .load_run(run_id)
            .unwrap()
            .unwrap();
        assert!(matches!(persisted.status(), alms_core::RunStatus::Failed));
        assert_eq!(persisted.terminal_reason(), Some("gateway_restarted"));
    }

    #[test]
    fn gateway_restart_releases_cancelled_queued_input_without_changing_terminal_state() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("cancelled-restart-recovery.db");
        let db_path = db_path.to_str().unwrap();
        let agent_id = AgentId::new();
        let session = alms_session::Session::new(agent_id, "cancelled-restart-recovery");
        let mut run = alms_core::Run::new(session.id, agent_id, "keep me visible".to_string());
        let run_id = run.run_id;
        let message = alms_session::Message {
            id: "cancelled-restart-recovery-input".to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(run.input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": run_id.0.to_string(),
            })),
        };

        {
            let store = SqliteStore::open(db_path).unwrap();
            store.save_session(&session).unwrap();
            store.save_run_with_initial_message(&run, &message).unwrap();
            assert!(run.mark_cancelled(), "queued cancellation must be accepted");
            store.save_run(&run).unwrap();
        }

        let config = GatewayConfig {
            db_path: Some(db_path.to_string()),
            ..GatewayConfig::default()
        };
        let gateway = Gateway::new(config).unwrap();

        let context = gateway
            .session_manager
            .get_context_history(session.id)
            .unwrap();
        let recovered_input = context
            .iter()
            .find(|candidate| candidate.id == message.id)
            .expect("cancelled run input must become context-visible after restart");
        let metadata = recovered_input.metadata.as_ref().unwrap();
        assert_eq!(metadata["pending_input"], false);
        assert!(metadata["input_claimed_at"].is_string());

        let persisted = gateway
            .session_manager
            .store()
            .unwrap()
            .load_run(run_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            persisted.status(),
            alms_core::RunStatus::Cancelled
        ));
        assert_eq!(persisted.lifecycle_revision(), run.lifecycle_revision());
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
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: alms_core::WorktreeMode::Off,
            debug_mode: false,
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

    // ── #947: WARN-log assertions for [security].allow_full_os_access ──

    /// In-memory `MakeWriter` that captures every line emitted by the
    /// fmt subscriber so tests can assert on log content.
    ///
    /// Implemented as a `Mutex<Vec<u8>>` shared via `Arc` so the writer
    /// closure (which the subscriber calls per event) and the test body
    /// (which reads the captured lines) point at the same buffer.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = LogWriter;
        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(self.0.clone())
        }
    }

    struct LogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedLogs {
        fn captured(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// `warn_full_os_access_at_boot` emits one structured WARN per
    /// listed agent. Acceptance check from #947: "WARN at agent-create
    /// / first-observation-at-boot".
    #[test]
    fn boot_warn_emits_for_each_listed_agent() {
        use tracing_subscriber::fmt::format::FmtSpan;

        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_target(true)
            .with_span_events(FmtSpan::NONE)
            .without_time()
            // No ANSI colour codes — keeps assertions on the captured
            // text simple.
            .with_ansi(false)
            .finish();

        let security_config = alms_core::config::SecurityConfig {
            allow_full_os_access: vec!["alice".into(), "bob".into()],
        };

        // Drive the helper under our subscriber.
        tracing::subscriber::with_default(subscriber, || {
            warn_full_os_access_at_boot(&security_config);
        });

        let captured = logs.captured();

        // The structured fields each appear in the output (the fmt
        // subscriber renders them as `field_name=value`).
        assert!(
            captured.contains("agent_name=alice"),
            "WARN must carry structured agent_name=alice: {captured}"
        );
        assert!(
            captured.contains("agent_name=bob"),
            "WARN must carry structured agent_name=bob: {captured}"
        );
        assert!(
            captured.contains("allow_full_os_access=true"),
            "WARN must carry structured allow_full_os_access=true: {captured}"
        );
        // Target is the configured "alms.security" string.
        assert!(
            captured.contains("alms.security"),
            "WARN must use the alms.security tracing target: {captured}"
        );
        // Two listed agents → at least two `WARN` markers.
        let warn_lines = captured.matches("WARN").count();
        assert!(
            warn_lines >= 2,
            "Expected ≥2 WARN lines (one per listed agent), got {warn_lines}: {captured}"
        );
    }

    /// Empty `allow_full_os_access` is a complete no-op: no WARN at boot.
    #[test]
    fn boot_warn_silent_when_list_is_empty() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_target(true)
            .without_time()
            .with_ansi(false)
            .finish();

        let security_config = alms_core::config::SecurityConfig::default();
        tracing::subscriber::with_default(subscriber, || {
            warn_full_os_access_at_boot(&security_config);
        });

        let captured = logs.captured();
        assert!(
            !captured.contains("alms.security"),
            "no WARN must fire when the list is empty: {captured}"
        );
    }
}
