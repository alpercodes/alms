//! Shared application state for the HTTP server.
//!
//! [`AppState`] is constructed once at startup (inside [`AppState::new`]) and
//! then shared across all Axum handlers via `State<AppState>`.

use super::RunManager;
use crate::approvals::ApprovalStore;
use crate::gateway::Gateway;
use crate::session_queue::SessionQueue;
use alms_coordinator::Coordinator;
use alms_core::{AgentId, AlmsResult};
use alms_runtime::Scheduler;
use alms_session::{JobStore, SessionManager};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Shared application state for HTTP server
#[derive(Debug, Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub gateway: Arc<tokio::sync::Mutex<Gateway>>,
    pub run_manager: RunManager,
    pub approval_store: ApprovalStore,
    /// Base directory for agent workspace files (None = workspace API disabled)
    pub workspace_dir: Option<std::path::PathBuf>,
    /// Absolute path to the gateway's data directory. Propagated to shell_exec
    /// as `ALMS_DATA_DIR` so CLI commands invoked by agents find the right DB.
    pub data_dir: std::path::PathBuf,
    /// Job store for scheduled jobs
    pub job_store: Arc<JobStore>,
    /// Scheduler for firing jobs at the right time
    pub scheduler: Arc<Scheduler>,
    /// Coordinator for subagent lifecycle management
    pub coordinator: Arc<Coordinator>,
    /// Token cancelled during graceful shutdown.
    pub shutdown_token: CancellationToken,
    /// Per-agent work queue -- serializes all runs for a given agent (Section 7
    /// of the Layer 2 design: no parallel agent instances).
    pub agent_queue: Arc<SessionQueue<AgentId>>,
    /// Snapshot of LLM config — read once at startup so handlers avoid locking the gateway.
    pub llm_config: alms_runtime::LlmConfig,
    /// Agent config — mutable via PATCH /settings (context section).
    pub agent_config: Arc<parking_lot::RwLock<alms_runtime::AgentConfig>>,
    /// Default agent ID — shared with Gateway, updated live on set-default.
    pub default_agent_id: Arc<parking_lot::RwLock<AgentId>>,
    /// LLM client clone — read once at startup so run execution avoids locking the gateway.
    pub llm: alms_runtime::LlmClient,
    /// Auth token — read once at startup.
    pub auth_token_value: Option<String>,
    /// Shared secrets store for API key management.
    pub secrets: Arc<parking_lot::RwLock<alms_core::secrets::SecretsStore>>,
    /// Agent-to-agent message bus for peer messaging (Layer 2).
    pub message_bus: Arc<alms_coordinator::message_bus::MessageBus>,
    /// Session config — mutable via PATCH /settings.
    pub session_config: Arc<parking_lot::RwLock<alms_session::SessionConfig>>,
    /// Logging config snapshot — exposed via GET /settings for UI display (read-only, requires restart).
    pub logging_config: alms_core::config::LoggingConfig,
    /// Tools config — mutable via PATCH /settings.
    pub tools_config: Arc<parking_lot::RwLock<alms_core::config::ToolsConfig>>,
}

impl AppState {
    pub fn new(
        gateway: Gateway,
        scheduler: Arc<Scheduler>,
        shutdown_token: CancellationToken,
        completion_tx: tokio::sync::mpsc::UnboundedSender<alms_coordinator::SubagentCompletion>,
        run_trigger_tx: tokio::sync::mpsc::UnboundedSender<
            alms_coordinator::message_bus::RunTrigger,
        >,
    ) -> AlmsResult<Self> {
        let workspace_dir = gateway.workspace_dir().map(|p| p.to_path_buf());
        let data_dir = gateway
            .data_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|cwd| cwd.join("data"))
                    .unwrap_or_else(|_| std::path::PathBuf::from("./data"))
            });
        let session_manager = gateway.session_manager().clone();
        let llm = gateway.llm().clone();
        let agent_id = gateway.agent_id();
        let default_agent_id = gateway.agent_id_handle();
        let llm_config = gateway.llm_config().clone();
        let mut agent_config_val = gateway.agent_config().clone();
        let auth_token_value = gateway.auth_token().map(String::from);
        let mut session_config = gateway.session_config().clone();
        let logging_config = gateway.logging_config().clone();
        let mut tools_config = gateway.tools_config().clone();

        // Apply persisted settings from a previous PATCH /settings so that
        // configuration changes survive gateway restarts.
        //
        // Persisted settings use `Option<T>` fields — only values the user
        // explicitly set via PATCH /settings are `Some`. This ensures that:
        // - Code-default changes (e.g. `run_summary_mode` changing from Off
        //   to Llm) are picked up for non-overridden fields
        // - Env-var overrides (e.g. `ALMS_RUN_SUMMARY_MODE`) remain effective
        if let Some(persisted) = crate::settings::load_persisted_settings(&data_dir) {
            if let Some(ctx_overrides) = persisted.context {
                ctx_overrides.apply_to(&mut agent_config_val.context_config);
            }
            if let Some(sess_overrides) = persisted.session {
                sess_overrides.apply_to(&mut session_config);
            }
            if let Some(tools_overrides) = persisted.tools {
                // Also sync the two copies kept in agent_config.
                if let Some(ref root) = tools_overrides.sandbox_root {
                    agent_config_val.sandbox_root = root.clone();
                }
                if let Some(ref policy) = tools_overrides.shell_policy {
                    agent_config_val.shell_policy = policy.clone();
                }
                tools_overrides.apply_to(&mut tools_config);
            }
        }

        // Create the shared agent config Arc *once* — both the Coordinator and
        // AppState reference the same lock so PATCH /settings updates propagate.
        let agent_config = Arc::new(parking_lot::RwLock::new(agent_config_val));
        let db_path_str = gateway.db_path().map(String::from);
        let job_store = match db_path_str.as_deref() {
            Some(path) => {
                tracing::info!("Opening SQLite job store at {}", path);
                Arc::new(JobStore::with_sqlite(path)?)
            }
            None => Arc::new(JobStore::new()),
        };
        // Build RunManager with optional SQLite persistence, then hydrate
        // completed runs from the database so GET /runs returns history.
        // Created before the Coordinator so we can share it as a RunRegistrar.
        let run_manager = if let Some(store) = session_manager.store() {
            let rm = RunManager::new().with_store(Arc::clone(store));
            rm.hydrate_from_store();
            rm
        } else {
            RunManager::new()
        };

        let mut coord = Coordinator::with_agent_config(
            agent_id,
            session_manager.clone(),
            llm.clone(),
            Arc::clone(&agent_config),
        )
        .with_completion_channel(completion_tx)
        .with_run_registrar(Arc::new(run_manager.clone()));
        if let Some(ref ws_dir) = workspace_dir {
            coord = coord.with_workspace_dir(ws_dir.clone());
        }
        coord = coord.with_data_dir(data_dir.clone());

        // Share the Gateway's secrets store so runtime key changes are visible
        // to both HTTP handlers and the Telegram message loop.
        let secrets = gateway.secrets_handle();

        coord = coord.with_secrets(secrets.clone());
        let coordinator = Arc::new(coord);

        // Create the peer-messaging MessageBus (Layer 2).
        let message_bus = Arc::new(alms_coordinator::message_bus::MessageBus::new(
            session_manager.clone(),
            run_trigger_tx,
        ));

        // Migrate any legacy UUID-based workspace directories to name-based paths.
        if let Some(ws_dir) = &workspace_dir
            && let Some(store) = session_manager.store()
        {
            match store.list_agents() {
                Ok(agents) => {
                    let pairs: Vec<_> = agents.iter().map(|a| (a.id.0, a.name.clone())).collect();
                    match alms_core::migrate_workspace_dirs(ws_dir, &pairs) {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(
                            count = n,
                            "Migrated workspace directories from UUID to name-based paths"
                        ),
                        Err(e) => tracing::warn!(error = %e, "Workspace migration error"),
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to list agents for workspace migration — migration skipped"
                    );
                }
            }
        }

        Ok(Self {
            session_manager,
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
            run_manager,
            approval_store: ApprovalStore::new(),
            workspace_dir,
            data_dir,
            job_store,
            scheduler,
            coordinator,
            shutdown_token: shutdown_token.clone(),
            agent_queue: Arc::new(SessionQueue::new(shutdown_token)),
            llm_config,
            agent_config,
            default_agent_id,
            llm,
            auth_token_value,
            secrets,
            message_bus,
            session_config: Arc::new(parking_lot::RwLock::new(session_config)),
            logging_config,
            tools_config: Arc::new(parking_lot::RwLock::new(tools_config)),
        })
    }
}
