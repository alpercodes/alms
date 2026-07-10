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

/// Live server-default LLM `(model, provider)` pair surfaced via
/// `GET /settings` and mutated via `PATCH /settings`'s top-level
/// `model` / `provider` keys. See
/// [`AppState::server_llm_default`] for lifetime / propagation rules.
#[derive(Debug, Clone)]
pub struct ServerLlmDefault {
    /// Server-default model id (e.g. `"z-ai/glm-5.2"`).
    pub model: String,
    /// Server-default provider name (must be a key in `llm_config.providers`).
    pub provider: String,
}

/// Shared application state for HTTP server
#[derive(Debug, Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub gateway: Arc<tokio::sync::Mutex<Gateway>>,
    pub run_manager: RunManager,
    pub approval_store: ApprovalStore,
    /// Base directory for agent workspace files (None = workspace API
    /// disabled). After #945 this is `<project_root>/.alms/agents/`.
    pub workspace_dir: Option<std::path::PathBuf>,
    /// Absolute path to the gateway's data directory. Propagated to shell_exec
    /// as `ALMS_DATA_DIR` so CLI commands invoked by agents find the right DB.
    pub data_dir: std::path::PathBuf,
    /// Absolute path to the project root (#945) — the agent's filesystem
    /// sandbox boundary. Resolved once at gateway start with precedence
    /// CLI `--project` > `ALMS_PROJECT_ROOT` > `current_dir()`. Both
    /// `fs_*` and `shell` tools enforce against this single root.
    pub project_root: std::path::PathBuf,
    /// Job store for scheduled jobs
    pub job_store: Arc<JobStore>,
    /// Open job episodes (#1198) — deferred-completion tracking for
    /// scheduled jobs (pending DMs / background subagents, quiescence,
    /// the 4-hour deadline). In-memory by design for phase 1; see
    /// `docs/jobs-await-completion-design.md`.
    pub(crate) job_episodes: Arc<crate::runs::job_episode::JobEpisodeTracker>,
    /// Jobs cancelled by the operator via `DELETE /jobs/{id}` (#1206
    /// follow-up, Codex P2 on PR #1210). The #1206 orphan-run suppression
    /// keys on membership here — the operator's *intent* — NOT on
    /// `JobStatus::Cancelled`, because `JobStatus` has no `Completed`
    /// variant (#763) and a normally-spent one-shot is also recorded as
    /// `Cancelled`. Keying on status would silently drop the late results
    /// of a one-shot's deadline-detached work (D5's "leave running"
    /// contract delivers those via an orphan run on the job session).
    ///
    /// In-memory lifetime is correct, not a compromise: the suppressed
    /// phenomena are in-flight triggers/completions, which don't survive a
    /// daemon restart either (#1159 B12) — a restart clearing the set
    /// loses nothing. Entries are never removed; job ids are tiny and
    /// operator cancels are rare.
    pub(crate) operator_cancelled_jobs: Arc<dashmap::DashSet<alms_core::JobId>>,
    /// Serializes a job cancellation's intent/cancellation sweep with a
    /// triggered run's pre-enqueue cancellation check (#1210 follow-up).
    ///
    /// The critical section contains no `await`: a trigger either sees the
    /// operator-cancel intent before it creates a run, or cancellation sees
    /// the run and cancels its already-registered token before it can spend a
    /// turn. See `jobs::cancel_job` and `runs::notifications::enqueue_triggered_run`.
    pub(crate) job_trigger_cancellation_gate: Arc<parking_lot::Mutex<()>>,
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
    ///
    /// `default_model` and `provider` here are seeded from the boot config
    /// (TOML / env / compiled defaults) merged with any persisted
    /// `model` / `provider` PATCHes from a previous run. The handler
    /// snapshot itself is by-value clone — PATCHing those two fields
    /// updates the persisted state and the live `Arc<RwLock<String>>`
    /// next to this field, but **does not** propagate to in-flight runs
    /// until the daemon restarts (the run path reads `state.llm` and
    /// this snapshot, both of which are clones taken at handler entry).
    /// Same restart-required contract as the Logging block.
    pub llm_config: alms_runtime::LlmConfig,
    /// Live source of truth for the server-default LLM `(model, provider)` —
    /// PATCH /settings writes here, GET /settings reads here, and
    /// `persist_settings` snapshots from here into `settings.json`.
    ///
    /// Wrapped in `Arc<RwLock>` so the PATCH handler can see its own
    /// write reflected on the next GET without round-tripping through the
    /// `state.llm_config` clone (which is by-value and not mutable from
    /// inside a handler). Restart is still required for the value to
    /// reach `state.llm` and downstream run paths, but the operator-facing
    /// settings surface stays consistent in the meantime.
    pub server_llm_default: Arc<parking_lot::RwLock<ServerLlmDefault>>,
    /// Agent config — mutable via PATCH /settings (context, llm provider
    /// defaults, and the `sandbox_root` / `shell_policy` mirrors of the
    /// tools section). Shared with the Coordinator (`Arc::clone`) so all
    /// HTTP-triggered run paths observe the same live config; the
    /// Telegram path inherits a boot-time snapshot — see
    /// `gateway.rs::Gateway::run_telegram` and `docs/api.md` § 10.2.
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
    /// Security config (#947) — config-file-only snapshot. The
    /// `[security].allow_full_os_access` list is consulted at run-start
    /// to decide whether to attach the project-root sandbox; it is NOT
    /// mutable via `PATCH /settings`, NOT persisted in
    /// `PersistedSettings`, and the `/settings` PATCH handler rejects
    /// any payload referencing this section with `400
    /// SECURITY_KNOB_NOT_PATCHABLE`.
    pub security_config: alms_core::config::SecurityConfig,
}

impl AppState {
    pub fn new(
        gateway: Gateway,
        scheduler: Arc<Scheduler>,
        shutdown_token: CancellationToken,
        completion_tx: tokio::sync::mpsc::UnboundedSender<alms_coordinator::SubagentCompletion>,
        // Bounded (#842 / B11) — see `MessageBus` and `server::mod`.
        run_trigger_tx: tokio::sync::mpsc::Sender<alms_coordinator::message_bus::RunTrigger>,
        dm_event_tx: tokio::sync::mpsc::Sender<alms_coordinator::message_bus::DmEvent>,
    ) -> AlmsResult<Self> {
        let workspace_dir = gateway.workspace_dir().map(|p| p.to_path_buf());
        let data_dir = gateway
            .data_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|cwd| cwd.join(".alms"))
                    .unwrap_or_else(|_| std::path::PathBuf::from("./.alms"))
            });
        // Project root (#945): the agent's filesystem-sandbox boundary.
        // The gateway always populates this in production
        // (`from_alms_config_with_env`); fall back to cwd for in-process
        // tests that build a `GatewayConfig` directly without the env
        // resolver. Mirrors `data_dir`'s defensive fallback above.
        let project_root = gateway
            .project_root()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });
        let session_manager = gateway.session_manager().clone();
        let mut llm = gateway.llm().clone();
        let agent_id = gateway.agent_id();
        let default_agent_id = gateway.agent_id_handle();
        let mut llm_config = gateway.llm_config().clone();
        let mut agent_config_val = gateway.agent_config().clone();
        let auth_token_value = gateway.auth_token().map(String::from);
        let mut session_config = gateway.session_config().clone();
        let logging_config = gateway.logging_config().clone();
        let mut tools_config = gateway.tools_config().clone();
        let security_config = gateway.security_config().clone();

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
            // LLM provider-family overrides (#809). Applied directly to
            // the live `AgentConfig` which is the source of truth for
            // every `POST /runs`.
            if let Some(llm_overrides) = persisted.llm {
                llm_overrides.apply_to(&mut agent_config_val);
            }
            // Server-default LLM `(model, provider)` from a previous
            // PATCH /settings. Persisted top-level rather than nested
            // because these fields shadow `llm.model` / `llm.provider`
            // in `alms.toml` (the same wire shape `GET /settings`
            // exposes), not any per-provider reasoning knob. Apply to
            // the `LlmConfig` snapshot first so `GET /settings` shows
            // the patched value immediately on next boot, then push to
            // the `LlmClient` so the next run actually uses the new
            // wire identity.
            //
            // Codex follow-up on #1081 (P1): switch to
            // `with_provider_and_secrets` so the API key for the new
            // provider is resolved from `SecretsStore` and
            // `[llm.providers.<name>].api_key_env / api_key` (the same
            // resolution path `resolve_agent_config` uses for per-agent
            // provider switches). With the old `with_provider`-only
            // call, `apply_provider` would clear the previous provider's
            // key and never repopulate from the env-based provider entry
            // — and later `resolve_agent_config` only re-checks
            // `SecretsStore` via `with_secrets`, never the provider
            // entry's env/static fields. Deployments configuring keys
            // exclusively via `[llm.providers.<name>].api_key_env` (no
            // `alms auth set`) would silently boot with an empty key
            // after a persisted provider switch.
            //
            // Codex follow-up on #1081 (P1, Finding 6): an operator who
            // PATCH-switched the server-default provider to `anthropic`
            // and later removed `[llm.providers.anthropic]` from
            // `alms.toml` (or renamed it) would otherwise still force the
            // stale provider on every restart. `with_provider_and_secrets`
            // would then run `apply_provider` against an unconfigured
            // name — clearing the previously active provider's API key
            // without populating a new one, leaving the LlmClient with
            // empty credentials. Default runs after boot would then fail
            // with an opaque 401 / "no API key" error that the operator
            // has no way to correlate back to the persisted PATCH.
            //
            // Guard the apply with a `providers.contains_key` check.
            // On miss, log a structured WARN under `alms.config` and
            // skip the override — the boot path falls through to the
            // `alms.toml` default provider, which is the operator's
            // most recent in-file configuration and the documented
            // recovery surface.
            //
            // Codex follow-up on #1081 (#1088): the model override must be
            // gated on the same decision. Model names are namespace-specific
            // (e.g. `claude-haiku-4-5` only makes sense on `anthropic`); if
            // the persisted provider is invalid we fall back to the
            // alms.toml default provider, and applying a stale namespace-
            // specific model on top would force the new provider to receive
            // a slug it can't resolve, surfacing as opaque 4xx wire errors
            // on the next default-agent run. When the provider is dropped
            // we drop the model too and let the alms.toml default model
            // through. Operators can re-pin via PATCH /settings once the
            // provider is restored.
            let persisted_provider_valid = match persisted.provider {
                Some(ref provider) if !provider.is_empty() => {
                    llm_config.providers.contains_key(provider)
                }
                // No persisted provider override (or empty string) means
                // we keep the in-file default and the persisted model
                // override — if any — is applied verbatim. The operator
                // explicitly pinned only the model via PATCH /settings,
                // which targets the active provider.
                _ => true,
            };
            if let Some(ref provider) = persisted.provider
                && !provider.is_empty()
            {
                if persisted_provider_valid {
                    llm_config.provider = provider.clone();
                    let secrets_handle = gateway.secrets_handle();
                    let secrets_guard = secrets_handle.read();
                    llm = llm.with_provider_and_secrets(provider, &secrets_guard);
                } else {
                    // Branch the prose on whether a non-empty persisted
                    // model is on the wire — when only a provider was
                    // persisted, the "AND model" phrasing would mislead
                    // operators scanning the log line. The structured
                    // `persisted_model` field is identical either way so
                    // log-shape consumers (and the regression tests) are
                    // unaffected by the prose split.
                    let has_persisted_model =
                        persisted.model.as_deref().is_some_and(|m| !m.is_empty());
                    if has_persisted_model {
                        tracing::warn!(
                            target: "alms.config",
                            persisted_provider = %provider,
                            persisted_model = persisted.model.as_deref().unwrap_or(""),
                            active_provider = %llm_config.provider,
                            active_model = %llm_config.default_model,
                            "ignoring persisted server-default provider AND model — \
                             `[llm.providers.{provider}]` is no longer configured \
                             in alms.toml; falling back to the in-file default \
                             provider and model. The persisted model is namespace- \
                             specific and would be invalid on the fallback provider. \
                             PATCH /settings with a configured (provider, model) \
                             pair to clear the persisted override, or restore the \
                             entry in alms.toml."
                        );
                    } else {
                        tracing::warn!(
                            target: "alms.config",
                            persisted_provider = %provider,
                            persisted_model = persisted.model.as_deref().unwrap_or(""),
                            active_provider = %llm_config.provider,
                            active_model = %llm_config.default_model,
                            "ignoring persisted server-default provider — \
                             `[llm.providers.{provider}]` is no longer configured \
                             in alms.toml; falling back to the in-file default \
                             provider and model. PATCH /settings with a \
                             configured provider to clear the persisted \
                             override, or restore the entry in alms.toml."
                        );
                    }
                }
            }
            // Only apply the persisted model when the persisted provider is
            // either absent or still valid. If the provider was skipped
            // above, the model is part of the same stale override and is
            // dropped together (the WARN above already named it).
            if persisted_provider_valid
                && let Some(ref model) = persisted.model
                && !model.is_empty()
            {
                llm_config.default_model = model.clone();
                llm = llm.with_model(model);
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
        .with_run_registrar(Arc::new(run_manager.clone()))
        // #1180: stream each subagent's OWN run events to its own session's SSE
        // log so the fullscreen subagent-session view streams live and is
        // replayable. Foreground and background alike.
        .with_subagent_self_sink(Arc::new(crate::runs::GatewaySubagentSelfSink::new(
            run_manager.clone(),
        )));
        if let Some(ref ws_dir) = workspace_dir {
            coord = coord.with_workspace_dir(ws_dir.clone());
        }
        coord = coord.with_data_dir(data_dir.clone());
        // Plumb the project root (#945) into the coordinator so subagents
        // inherit the parent's filesystem-sandbox boundary. Without this,
        // subagents fall through to whatever sandbox their `AgentConfig`
        // resolves at construction time, which after #945 would break the
        // single-root invariant.
        coord = coord.with_project_root(project_root.clone());
        // Plumb the security config (#947) so named subagents on
        // `[security].allow_full_os_access` opt out of the project-root
        // sandbox the same way HTTP-triggered runs do. Empty list (the
        // default) means no subagent is listed and the project-root pin
        // above wins as before.
        coord = coord.with_security_config(security_config.clone());

        // Share the Gateway's secrets store so runtime key changes are visible
        // to both HTTP handlers and the Telegram message loop.
        let secrets = gateway.secrets_handle();

        coord = coord.with_secrets(secrets.clone());
        let coordinator = Arc::new(coord);

        // Create the peer-messaging MessageBus (Layer 2).
        let message_bus = Arc::new(
            alms_coordinator::message_bus::MessageBus::new(session_manager.clone(), run_trigger_tx)
                .with_dm_event_channel(dm_event_tx),
        );

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

        // Seed the live `server_llm_default` from whatever ended up on the
        // post-persisted-apply `llm_config` snapshot. This is the
        // single read-write surface the PATCH /settings handler mutates;
        // GET /settings and persist_settings both consult it.
        let server_llm_default = Arc::new(parking_lot::RwLock::new(ServerLlmDefault {
            model: llm_config.default_model.clone(),
            provider: llm_config.provider.clone(),
        }));

        Ok(Self {
            session_manager,
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
            run_manager,
            approval_store: ApprovalStore::new(),
            workspace_dir,
            data_dir,
            project_root,
            job_store,
            job_episodes: Arc::new(crate::runs::job_episode::JobEpisodeTracker::new(
                std::time::Duration::from_secs(crate::runs::job_episode::EPISODE_DEADLINE_SECS),
            )),
            operator_cancelled_jobs: Arc::new(dashmap::DashSet::new()),
            job_trigger_cancellation_gate: Arc::new(parking_lot::Mutex::new(())),
            scheduler,
            coordinator,
            shutdown_token: shutdown_token.clone(),
            agent_queue: Arc::new(SessionQueue::new(shutdown_token)),
            llm_config,
            server_llm_default,
            agent_config,
            default_agent_id,
            llm,
            auth_token_value,
            secrets,
            message_bus,
            session_config: Arc::new(parking_lot::RwLock::new(session_config)),
            logging_config,
            tools_config: Arc::new(parking_lot::RwLock::new(tools_config)),
            security_config,
        })
    }
}
