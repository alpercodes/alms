//! HTTP server for ALMS Gateway
//!
//! Provides REST API endpoints per docs/api.md specification.

use crate::agents;
use crate::api_error;
use crate::approvals::{ApprovalStore, list_approvals, resolve_approval};
use crate::auth::{AuthToken, require_auth};
use crate::auth_keys;
use crate::cron_utils;
use crate::event_log::{EventLogManager, LoggedEvent};
use crate::gateway::Gateway;
use crate::jobs::{cancel_job, create_job, get_job, list_jobs};
use crate::runs::{
    cancel_run, completion_notification_loop, create_run, get_run_status, list_runs,
    run_trigger_loop, scheduler_fire_loop, stream_run_events,
};
use crate::session_queue::SessionQueue;
use crate::settings::get_settings;
use crate::sse::SseEventData;
use crate::tasks::{get_task, list_tasks};
use crate::workspace::{get_workspace, update_workspace_file};
use alms_coordinator::Coordinator;
use alms_core::{AgentId, AlmsResult, JobStatus, Run, RunId, SessionId};
use alms_runtime::Scheduler;
use alms_session::{Content, Role};
use alms_session::{JobStore, SessionManager};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use dashmap::DashMap;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

/// Run manager for tracking runs and their event streams
#[derive(Debug, Clone)]
pub struct RunManager {
    pub event_senders: Arc<DashMap<RunId, Vec<mpsc::UnboundedSender<SseEventData>>>>,
    pub runs: Arc<DashMap<RunId, Run>>,
    /// Persistent event log for reconnect-after-restart support
    pub event_log: EventLogManager,
    /// Session-level event senders for persistent SSE streams.
    pub session_senders: Arc<DashMap<SessionId, Vec<mpsc::UnboundedSender<SseEventData>>>>,
    /// Session-level event log for reconnect support.
    pub session_event_log: crate::event_log::SessionEventLogManager,
    /// Counter of in-flight (spawned but not yet finished) run tasks.
    in_flight: Arc<AtomicUsize>,
    /// Notified when an in-flight run completes (counter reaches zero).
    drain_notify: Arc<tokio::sync::Notify>,
    /// Per-run cancellation tokens for cooperative cancellation.
    cancel_tokens: Arc<DashMap<RunId, CancellationToken>>,
    /// Optional SQLite store for run persistence.
    sqlite_store: Option<Arc<alms_session::SqliteStore>>,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            event_senders: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            event_log: EventLogManager::new(),
            session_senders: Arc::new(DashMap::new()),
            session_event_log: crate::event_log::SessionEventLogManager::new(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            drain_notify: Arc::new(tokio::sync::Notify::new()),
            cancel_tokens: Arc::new(DashMap::new()),
            sqlite_store: None,
        }
    }

    /// Set the SQLite store for run persistence.
    pub fn with_store(mut self, store: Arc<alms_session::SqliteStore>) -> Self {
        self.sqlite_store = Some(store);
        self
    }

    /// Load persisted runs from SQLite into the in-memory DashMap.
    ///
    /// First marks any stale `queued`/`running` rows as `failed` (leftovers
    /// from a previous process that crashed or was killed), then loads recent
    /// terminal runs (completed/failed/cancelled) from the last 7 days.
    pub fn hydrate_from_store(&self) {
        let Some(store) = &self.sqlite_store else {
            return;
        };

        // Mark stale queued/running runs as failed before loading.
        match store.mark_stale_runs_failed() {
            Ok(count) if count > 0 => {
                info!(
                    "Marked {} stale queued/running runs as failed (gateway restarted)",
                    count
                );
            }
            Err(e) => {
                tracing::warn!("Failed to mark stale runs as failed: {}", e);
            }
            _ => {}
        }

        match store.load_all_runs() {
            Ok(runs) => {
                let loaded = runs.len();
                for run in runs {
                    self.runs.insert(run.run_id, run);
                }
                if loaded > 0 {
                    info!("Loaded {} persisted runs from SQLite", loaded);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load runs from SQLite: {}", e);
            }
        }
    }

    /// Increment the in-flight counter. Call when spawning a run task.
    pub fn track_in_flight(&self) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement the in-flight counter and wake drain waiters.
    pub fn untrack_in_flight(&self) {
        let prev = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            self.drain_notify.notify_waiters();
        }
    }

    /// Wait until all in-flight runs complete, or timeout expires.
    /// Returns `true` if drained, `false` on timeout.
    pub async fn wait_drain(&self, timeout: std::time::Duration) -> bool {
        loop {
            // Register the notification future BEFORE checking the counter
            // to avoid lost wakeups.
            let notified = self.drain_notify.notified();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                return true;
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(timeout) => return false,
            }
        }
    }

    pub fn register_sender(&self, run_id: RunId, sender: mpsc::UnboundedSender<SseEventData>) {
        self.event_senders.entry(run_id).or_default().push(sender);
    }

    pub fn remove_senders(&self, run_id: RunId) {
        self.event_senders.remove(&run_id);
    }

    /// Remove sender entries for runs that have already reached a terminal
    /// state. This is a defense-in-depth measure against the TOCTOU race in
    /// SSE subscription (see #149): if a sender is registered between the
    /// status check and `remove_senders` in `execute_run`, the entry becomes
    /// orphaned. Calling this periodically (or on demand) cleans up any
    /// leaked entries.
    pub fn purge_terminal_senders(&self) {
        self.event_senders.retain(|run_id, _| {
            let is_terminal = self
                .runs
                .get(run_id)
                .map(|r| {
                    matches!(
                        r.status,
                        alms_core::RunStatus::Completed
                            | alms_core::RunStatus::Failed
                            | alms_core::RunStatus::Cancelled
                    )
                })
                .unwrap_or(true); // run not found ⇒ definitely stale
            if is_terminal {
                tracing::debug!(run_id = %run_id.0, "Purged orphaned sender entry for terminal run");
            }
            !is_terminal
        });
    }

    pub fn insert_run(&self, run: Run) {
        if let Some(store) = &self.sqlite_store
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run.run_id.0, "Failed to persist new run to SQLite: {e}");
        }
        self.runs.insert(run.run_id, run);
    }

    pub fn get_run(&self, run_id: RunId) -> Option<Run> {
        self.runs.get(&run_id).map(|r| r.value().clone())
    }

    pub fn update_run(&self, run: Run) {
        if let Some(store) = &self.sqlite_store
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run.run_id.0, "Failed to persist run update to SQLite: {e}");
        }
        self.runs.insert(run.run_id, run);
    }

    /// Atomically transition a run to Running state and persist the snapshot.
    ///
    /// The run data is cloned while still holding the DashMap lock, so the
    /// persisted state cannot reflect a concurrent mutation.
    pub fn mark_run_as_running(&self, run_id: RunId) {
        let snapshot = self.modify_and_snapshot(run_id, |r| r.mark_running());
        self.persist_snapshot(run_id, snapshot);
    }

    /// Atomically transition a run to Completed state and persist the snapshot.
    pub fn mark_run_as_completed(
        &self,
        run_id: RunId,
        output: String,
        usage: alms_core::TokenUsage,
    ) {
        let snapshot = self.modify_and_snapshot(run_id, |r| r.mark_completed(output, usage));
        self.persist_snapshot(run_id, snapshot);
    }

    /// Atomically transition a run to Failed state and persist the snapshot.
    pub fn mark_run_as_failed(&self, run_id: RunId, error: String) {
        let snapshot = self.modify_and_snapshot(run_id, |r| r.mark_failed(error));
        self.persist_snapshot(run_id, snapshot);
    }

    /// Atomically transition a run to Cancelled state and persist the snapshot.
    pub fn mark_run_as_cancelled(&self, run_id: RunId) {
        let snapshot = self.modify_and_snapshot(run_id, |r| r.mark_cancelled());
        self.persist_snapshot(run_id, snapshot);
    }

    /// Modify a run in the DashMap and return a clone while still under lock.
    ///
    /// Returns `None` if the run does not exist (callers always `insert_run`
    /// first, so this should not happen in practice).
    fn modify_and_snapshot(&self, run_id: RunId, f: impl FnOnce(&mut Run)) -> Option<Run> {
        let mut entry = self.runs.get_mut(&run_id)?;
        f(entry.value_mut());
        Some(entry.clone())
    }

    /// Persist a previously-snapshotted run to SQLite (if store is configured).
    fn persist_snapshot(&self, run_id: RunId, snapshot: Option<Run>) {
        if let Some(store) = &self.sqlite_store
            && let Some(run) = snapshot
            && let Err(e) = store.save_run(&run)
        {
            tracing::warn!(run_id = %run_id.0, "Failed to persist run to SQLite: {e}");
        }
    }

    /// Store a per-run cancellation token.
    pub fn register_cancel_token(&self, run_id: RunId, token: CancellationToken) {
        self.cancel_tokens.insert(run_id, token);
    }

    /// Trigger cancellation for a run. Returns true if the token was found.
    pub fn cancel_run(&self, run_id: RunId) -> bool {
        if let Some(entry) = self.cancel_tokens.get(&run_id) {
            entry.value().cancel();
            true
        } else {
            false
        }
    }

    /// Remove a per-run cancellation token (cleanup after run ends).
    pub fn remove_cancel_token(&self, run_id: RunId) {
        self.cancel_tokens.remove(&run_id);
    }

    /// List runs for a session, newest first, up to `limit`.
    pub fn list_by_session(&self, session_id: SessionId, limit: usize) -> Vec<Run> {
        let mut runs: Vec<Run> = self
            .runs
            .iter()
            .filter(|e| e.value().session_id == session_id)
            .map(|e| e.value().clone())
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        runs.truncate(limit);
        runs
    }

    /// Send event to all active subscribers AND persist to event log.
    /// Dead subscribers (closed channels) are pruned automatically.
    /// Fans out to both per-run and per-session subscribers.
    pub async fn send_event(&self, run_id: RunId, session_id: SessionId, mut event: SseEventData) {
        let is_delta = event.event_type == "token_delta";

        // Per-run event log — skip token_delta to reduce lock contention.
        // Live per-run subscribers still receive deltas via fan-out below.
        if !is_delta {
            let event_id = self
                .event_log
                .log_event(run_id, session_id, &event.event_type, event.data.clone())
                .await;
            event.event_id = Some(event_id);
        }

        if let Some(mut senders) = self.event_senders.get_mut(&run_id) {
            let before = senders.len();
            senders.retain(|sender| sender.send(event.clone()).is_ok());
            let pruned = before - senders.len();
            if pruned > 0 {
                tracing::debug!(run_id = %run_id.0, pruned, "Pruned dead SSE subscriber(s)");
            }
        }

        // Per-session fan-out: forward to session subscribers.
        // Skip session event LOG for token_delta — these are high-frequency
        // (50-100 per response) and the extra lock acquisitions + clones
        // cause noticeable latency. Session reconnect doesn't need individual
        // deltas — the chat history is loaded via getSessionMessages instead.
        let session_event = if is_delta {
            // Fast path: no logging, just fan out. Leave event_id as None
            // so the dedup filter in stream_with_replay passes it through
            // (dedup only drops Some(id) where id <= max_replay_id).
            event
        } else {
            let session_event_id = self
                .session_event_log
                .log_event(session_id, run_id, &event.event_type, event.data.clone())
                .await;
            let mut e = event;
            e.event_id = Some(session_event_id);
            e
        };

        if let Some(mut senders) = self.session_senders.get_mut(&session_id) {
            senders.retain(|sender| sender.send(session_event.clone()).is_ok());
            if senders.is_empty() {
                drop(senders);
                self.session_senders.remove(&session_id);
            }
        }
    }

    /// Send a session-only event (not associated with a specific run).
    pub async fn send_session_event(
        &self,
        session_id: SessionId,
        run_id: RunId,
        event: SseEventData,
    ) {
        let session_event_id = self
            .session_event_log
            .log_event(session_id, run_id, &event.event_type, event.data.clone())
            .await;
        let mut tagged = event;
        tagged.event_id = Some(session_event_id);

        if let Some(mut senders) = self.session_senders.get_mut(&session_id) {
            senders.retain(|sender| sender.send(tagged.clone()).is_ok());
            if senders.is_empty() {
                drop(senders);
                self.session_senders.remove(&session_id);
            }
        }
    }

    pub fn register_session_sender(
        &self,
        session_id: SessionId,
        sender: mpsc::UnboundedSender<SseEventData>,
    ) {
        self.session_senders
            .entry(session_id)
            .or_default()
            .push(sender);
    }

    /// Close all active SSE sender channels (both per-run and per-session).
    ///
    /// Dropping the senders causes the corresponding `UnboundedReceiverStream`
    /// in each SSE response to terminate, which allows Axum's graceful
    /// shutdown to complete instead of waiting indefinitely for long-lived
    /// SSE connections.
    pub fn close_all_senders(&self) {
        let run_count = self.event_senders.len();
        let session_count = self.session_senders.len();
        self.event_senders.clear();
        self.session_senders.clear();
        if run_count + session_count > 0 {
            info!(
                "Closed {} per-run and {} per-session SSE sender(s) for shutdown",
                run_count, session_count
            );
        }
    }

    /// Get per-run events from a specific ID for reconnect
    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        self.event_log.events_from(run_id, from_id).await
    }

    /// Get per-session events from a specific ID for reconnect
    pub async fn session_events_from(
        &self,
        session_id: SessionId,
        from_id: u64,
    ) -> Vec<LoggedEvent> {
        self.session_event_log
            .events_from(session_id, from_id)
            .await
    }
}

impl Default for RunManager {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// Snapshot of agent config — read once at startup so handlers avoid locking the gateway.
    pub agent_config: alms_runtime::AgentConfig,
    /// Default agent ID — shared with Gateway, updated live on set-default.
    pub default_agent_id: Arc<std::sync::RwLock<AgentId>>,
    /// LLM client clone — read once at startup so run execution avoids locking the gateway.
    pub llm: alms_runtime::LlmClient,
    /// Auth token — read once at startup.
    pub auth_token_value: Option<String>,
    /// Shared secrets store for API key management.
    pub secrets: Arc<std::sync::RwLock<alms_core::secrets::SecretsStore>>,
    /// Agent-to-agent message bus for peer messaging (Layer 2).
    pub message_bus: Arc<alms_coordinator::message_bus::MessageBus>,
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
        let agent_config = gateway.agent_config().clone();
        let auth_token_value = gateway.auth_token().map(String::from);
        let db_path_str = gateway.db_path().map(String::from);
        let job_store = match db_path_str.as_deref() {
            Some(path) => {
                tracing::info!("Opening SQLite job store at {}", path);
                Arc::new(JobStore::with_sqlite(path)?)
            }
            None => Arc::new(JobStore::new()),
        };
        let mut coord = Coordinator::with_agent_config(
            agent_id,
            session_manager.clone(),
            llm.clone(),
            agent_config.clone(),
        )
        .with_completion_channel(completion_tx);
        if let Some(ref ws_dir) = workspace_dir {
            coord = coord.with_workspace_dir(ws_dir.clone());
        }
        coord = coord.with_data_dir(data_dir.clone());

        // Construct secrets store early so both coordinator and AppState can use it.
        let secrets_path = alms_core::secrets::secrets_path_from_db(db_path_str.as_deref());
        let secrets = Arc::new(std::sync::RwLock::new(
            alms_core::secrets::SecretsStore::load(secrets_path).unwrap_or_else(|e| {
                tracing::warn!("Failed to load secrets: {e}");
                alms_core::secrets::SecretsStore::empty()
            }),
        ));

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

        // Build RunManager with optional SQLite persistence, then hydrate
        // completed runs from the database so GET /runs returns history.
        let run_manager = if let Some(store) = session_manager.store() {
            let rm = RunManager::new().with_store(Arc::clone(store));
            rm.hydrate_from_store();
            rm
        } else {
            RunManager::new()
        };

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
        })
    }
}

// Re-export SSE types
pub use crate::sse::{RunEventStream, event_channel};

/// Routes that do NOT require authentication
fn public_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .nest_service(
            "/ui",
            ServeDir::new("crates/alms-gateway/static/ui")
                .fallback(ServeFile::new("crates/alms-gateway/static/ui/index.html")),
        )
}

/// Routes that require authentication (all except /health)
fn protected_router() -> Router<AppState> {
    Router::new()
        // Web UI
        .route("/", get(serve_ui))
        // Sessions
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route(
            "/sessions/{session_id}/events",
            get(crate::runs::stream_session_events),
        )
        .route("/sessions/{agent_id}/{context_id}", get(get_session))
        // Runs (canonical API per spec)
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{run_id}", get(get_run_status))
        .route("/runs/{run_id}/events", get(stream_run_events))
        .route("/runs/{run_id}/cancel", post(cancel_run))
        // Approvals
        .route("/approvals", get(list_approvals))
        .route("/approvals/{approval_id}", post(resolve_approval))
        // Audit
        .route("/audit", get(get_audit))
        // Agent registry CRUD
        .route(
            "/agents",
            get(agents::list_agents).post(agents::create_agent),
        )
        .route(
            "/agents/{id_or_name}",
            get(agents::get_agent)
                .put(agents::update_agent)
                .delete(agents::delete_agent),
        )
        .route("/agents/{id_or_name}/default", post(agents::set_default))
        // Workspace (agent identity files)
        .route("/agents/{id_or_name}/workspace", get(get_workspace))
        .route(
            "/agents/{id_or_name}/workspace/{file}",
            axum::routing::put(update_workspace_file),
        )
        // Settings (server defaults for UI pre-population)
        .route("/settings", get(get_settings))
        // Jobs (scheduled agent runs)
        .route("/jobs", post(create_job).get(list_jobs))
        .route("/jobs/{job_id}", get(get_job).delete(cancel_job))
        // Coordinator tasks (active subagents)
        .route("/tasks", get(list_tasks))
        .route("/tasks/{task_id}", get(get_task))
        // API key management
        .route(
            "/auth/keys",
            get(auth_keys::list_keys).put(auth_keys::set_key),
        )
        .route(
            "/auth/keys/{provider}",
            axum::routing::delete(auth_keys::remove_key),
        )
        .route("/ws", get(websocket_handler))
}

/// Serve the embedded web UI
async fn serve_ui() -> impl IntoResponse {
    (
        [
            ("Cache-Control", "no-store"),
            ("Content-Type", "text/html; charset=utf-8"),
        ],
        include_str!("../static/ui/index.html"),
    )
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "alms",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// GET /sessions?agent_id=<uuid> — list sessions, optionally filtered by agent.
///
/// Excludes internal sessions created by the coordinator (subagent_*) and
/// scheduler (job_*) — these are implementation details not shown in the UI.
async fn list_sessions(
    State(state): State<AppState>,
    Query(params): Query<ListSessionsQuery>,
) -> impl IntoResponse {
    let mut sessions = state.session_manager.list_all();
    sessions
        .retain(|s| !s.context_id.starts_with("subagent_") && !s.context_id.starts_with("job_"));
    if let Some(agent_id) = params.agent_id {
        sessions.retain(|s| s.agent_id == agent_id);
    }
    Json(serde_json::json!({ "sessions": sessions }))
}

#[derive(Debug, Deserialize)]
struct ListSessionsQuery {
    agent_id: Option<AgentId>,
}

/// Create a new session
async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let key = (req.agent_id, req.context_id.clone());
    let existed = state.session_manager.has_session(&key);
    let session = state.session_manager.get_or_create(key.0, key.1);

    Json(CreateSessionResponse {
        session_id: session.id,
        created: !existed,
    })
}

/// Get session info
async fn get_session(
    State(state): State<AppState>,
    Path((agent_id, context_id)): Path<(AgentId, String)>,
) -> impl IntoResponse {
    let session = state.session_manager.get_or_create(agent_id, context_id);

    Json(session)
}

/// GET /sessions/{session_id}/messages — return chat history including tool calls
async fn get_session_messages(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> impl IntoResponse {
    tracing::debug!("GET /sessions/{}/messages", session_id.0);
    match state.session_manager.get_history(session_id) {
        Ok(messages) => {
            tracing::debug!(
                "Session {} has {} total messages",
                session_id.0,
                messages.len()
            );
            let visible: Vec<serde_json::Value> = messages
                .into_iter()
                .filter_map(|m| match (&m.role, &m.content) {
                    (Role::User, Content::Text(t)) => Some(serde_json::json!({
                        "role": "user",
                        "type": "text",
                        "content": t,
                        "timestamp": m.timestamp,
                    })),
                    (Role::Assistant, Content::Text(t)) => Some(serde_json::json!({
                        "role": "assistant",
                        "type": "text",
                        "content": t,
                        "timestamp": m.timestamp,
                    })),
                    (Role::Assistant, Content::ToolCall { name, params }) => {
                        Some(serde_json::json!({
                            "role": "assistant",
                            "type": "tool_call",
                            "tool": name,
                            "params": params,
                            "timestamp": m.timestamp,
                            "metadata": m.metadata,
                        }))
                    }
                    (Role::Tool, Content::ToolResult { tool_id, result }) => {
                        let ok = m
                            .metadata
                            .as_ref()
                            .and_then(|md| md.get("ok"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Some(serde_json::json!({
                            "role": "tool",
                            "type": "tool_result",
                            "tool_id": tool_id,
                            "result": result,
                            "ok": ok,
                            "timestamp": m.timestamp,
                        }))
                    }
                    _ => None, // skip system messages
                })
                .collect();
            Json(serde_json::json!({ "messages": visible })).into_response()
        }
        Err(_) => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found").into_response()
        }
    }
}

/// GET /audit?session_id=<uuid>&limit=<n>
async fn get_audit(
    State(state): State<AppState>,
    Query(params): Query<AuditQuery>,
) -> impl IntoResponse {
    match state.session_manager.get_audit(params.session_id) {
        Ok(mut events) => {
            let limit = params.limit.unwrap_or(100);
            events.truncate(limit);
            Json(serde_json::json!({ "events": events })).into_response()
        }
        Err(_) => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found").into_response()
        }
    }
}

/// WebSocket handler (optional, SSE preferred)
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|_socket| async {
        info!("WebSocket connection established (consider using SSE instead)");
    })
}

/// TCP listener wrapper that sets TCP_NODELAY on every accepted connection.
///
/// Nagle's algorithm buffers small writes, which delays SSE events from
/// reaching the browser.  Disabling it ensures token_delta frames are sent
/// immediately.
struct NoDelayListener(tokio::net::TcpListener);

impl axum::serve::Listener for NoDelayListener {
    type Io = tokio::net::TcpStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    return (stream, addr);
                }
                Err(e) => {
                    tracing::error!("TCP accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// Start the gateway HTTP server from a pre-loaded config.
///
/// This is the preferred entry-point: the caller loads `AlmsConfig` once
/// and passes it here, avoiding a second parse inside the gateway.
pub async fn serve_with_config(bind_addr: &str, config: &alms_core::AlmsConfig) -> AlmsResult<()> {
    let gateway_config = crate::gateway::GatewayConfig::from_alms_config_with_env(config);
    let gateway = Gateway::new(gateway_config)?;
    serve_with_gateway(bind_addr, gateway).await
}

/// Start the gateway HTTP server (loads config from env internally).
///
/// Prefer `serve_with_config` when the caller already has an `AlmsConfig`
/// to avoid parsing config twice.
pub async fn serve(bind_addr: &str) -> AlmsResult<()> {
    let gateway = Gateway::from_env()?;
    serve_with_gateway(bind_addr, gateway).await
}

pub async fn serve_with_gateway(bind_addr: &str, gateway: Gateway) -> AlmsResult<()> {
    let shutdown_token = CancellationToken::new();

    // Create the scheduler with a fire channel so job IDs are forwarded to
    // the gateway for actual agent-run dispatch.
    let (fire_tx, fire_rx) = tokio::sync::mpsc::unbounded_channel::<alms_core::JobId>();
    let scheduler = Arc::new(Scheduler::new().with_fire_channel(fire_tx));

    let (completion_tx, completion_rx) =
        tokio::sync::mpsc::unbounded_channel::<alms_coordinator::SubagentCompletion>();

    let (run_trigger_tx, run_trigger_rx) =
        tokio::sync::mpsc::unbounded_channel::<alms_coordinator::message_bus::RunTrigger>();

    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        run_trigger_tx,
    )?;

    {
        let mut gateway = state.gateway.lock().await;
        gateway.initialize_channels().await?;
        gateway.start().await?;
    }

    // Re-register persisted jobs before starting the runner so the heap is
    // populated before the first sleep.
    bootstrap_scheduler(&state).await?;

    // Start the background scheduler runner (shutdown-aware).
    let scheduler_handle = state.scheduler.start_with_shutdown(shutdown_token.clone());

    // Spawn the fire-receiver: turns fired JobIds into real agent runs.
    let fire_state = state.clone();
    let fire_handle = tokio::spawn(scheduler_fire_loop(fire_rx, fire_state));

    // Spawn the completion-notification loop: turns background subagent
    // completions into follow-up runs on the parent session.
    let completion_state = state.clone();
    let completion_handle = tokio::spawn(completion_notification_loop(
        completion_rx,
        completion_state,
    ));

    // Spawn the run-trigger loop: processes peer message triggers from the
    // MessageBus and creates runs on the target agent's session.
    let trigger_state = state.clone();
    let trigger_handle = tokio::spawn(run_trigger_loop(run_trigger_rx, trigger_state));

    // Use the auth token snapshot from AppState — no mutex lock needed.
    let auth_token = AuthToken(state.auth_token_value.clone());

    // Spawn the channel message loop (Telegram polling, etc.).
    // The loop selects on the shutdown token so it exits cooperatively
    // without requiring us to lock the gateway mutex from outside.
    let background_gateway = state.gateway.clone();
    let gateway_token = shutdown_token.clone();
    let gateway_agent_queue = state.agent_queue.clone();
    let gateway_handle = tokio::spawn(async move {
        let mut gateway = background_gateway.lock().await;
        if let Err(e) = gateway
            .run_until_shutdown(gateway_token, gateway_agent_queue)
            .await
        {
            tracing::error!("Gateway message loop exited: {}", e);
        }
    });
    if auth_token.0.is_none() {
        tracing::warn!(
            "ALMS_AUTH_TOKEN is not set — API authentication is DISABLED. \
             Set it before exposing to the network."
        );
    } else {
        info!("API authentication enabled");
    }

    let app = public_router()
        .merge(
            protected_router()
                .layer(middleware::from_fn(require_auth))
                .layer(Extension(auth_token)),
        )
        .with_state(state.clone());

    info!("Starting ALMS Gateway HTTP server on {}", bind_addr);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    // === Graceful shutdown sequence ===
    // Wrap in NoDelayListener so every accepted connection has TCP_NODELAY.
    // This disables Nagle buffering, which is critical for SSE: without it,
    // small token_delta events sit in the TCP send buffer and never reach
    // the browser until the connection closes (observed on Windows).
    axum::serve(NoDelayListener(listener), app)
        .with_graceful_shutdown(shutdown_signal(shutdown_token, state.run_manager.clone()))
        .await?;

    // Phase 1: Signal received. Axum stopped accepting new connections.
    info!("HTTP server stopped accepting connections, draining...");

    // Phase 2: Scheduler loop already exiting (token cancelled).
    scheduler_handle.await.ok();
    info!("Scheduler stopped");

    // Phase 3: Abort the fire loop. The scheduler is stopped so no new
    // job IDs will arrive. The fire_tx is kept alive by Arc inside the
    // fire loop's AppState clone, so rx.recv() would hang — abort instead.
    // Any in-flight runs spawned by fire_job_run are tracked by the
    // in-flight counter and will be drained in phase 5.
    fire_handle.abort();
    fire_handle.await.ok();
    info!("Scheduler fire loop stopped");

    completion_handle.abort();
    completion_handle.await.ok();
    info!("Completion notification loop stopped");

    trigger_handle.abort();
    trigger_handle.await.ok();
    info!("RunTrigger loop stopped");

    // Phase 4: Gateway message loop already exiting (token cancelled).
    gateway_handle.await.ok();
    info!("Channel adapters stopped");

    // Phase 5: Wait for in-flight runs to complete (with timeout).
    let drain_timeout = std::time::Duration::from_secs(30);
    let drained = state.run_manager.wait_drain(drain_timeout).await;
    if drained {
        info!("All in-flight runs completed");
    } else {
        tracing::warn!("Shutdown timeout: some runs did not finish within 30s");
    }

    // Phase 6: Flush SQLite WAL.
    if let Err(e) = state.session_manager.flush_wal() {
        tracing::error!("Failed to flush session WAL: {}", e);
    }
    if let Err(e) = state.job_store.flush_wal() {
        tracing::error!("Failed to flush job WAL: {}", e);
    }
    info!("SQLite WAL flushed");

    info!("ALMS Gateway shut down cleanly");
    Ok(())
}

/// Returns a future that completes when a shutdown signal is received.
///
/// After the signal fires, this function:
/// 1. Cancels the shutdown token (stops scheduler, gateway message loop, etc.)
/// 2. Closes all SSE sender channels so persistent SSE connections terminate
///    and Axum's graceful shutdown can actually complete.
async fn shutdown_signal(token: CancellationToken, run_manager: RunManager) {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => { info!("Received Ctrl+C, initiating graceful shutdown"); }
            _ = sigterm.recv() => { info!("Received SIGTERM, initiating graceful shutdown"); }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to install Ctrl+C handler");
        info!("Received Ctrl+C, initiating graceful shutdown");
    }

    token.cancel();

    // Close all SSE sender channels so persistent SSE connections (especially
    // session-level streams) terminate. Without this, Axum's graceful shutdown
    // waits indefinitely for long-lived SSE connections to close.
    run_manager.close_all_senders();
}

/// Re-register all non-cancelled persisted jobs with the scheduler on startup.
async fn bootstrap_scheduler(state: &AppState) -> AlmsResult<()> {
    let now = chrono::Utc::now();
    let jobs = state.job_store.list();
    let mut registered = 0usize;

    for job in jobs {
        if job.status == JobStatus::Cancelled {
            continue;
        }
        let Some(fire_at) = cron_utils::compute_next_fire(&job, now) else {
            tracing::warn!("Job {} has no future fire time, skipping bootstrap", job.id);
            continue;
        };
        let delay = (fire_at - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job.id, instant).await;
        state.job_store.update_next_run_at(job.id, Some(fire_at))?;
        registered += 1;
    }

    if registered > 0 {
        info!("Bootstrapped {} job(s) into scheduler", registered);
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct CreateSessionRequest {
    agent_id: AgentId,
    context_id: String,
}

#[derive(Debug, serde::Serialize)]
struct CreateSessionResponse {
    session_id: alms_core::SessionId,
    created: bool,
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    session_id: alms_core::SessionId,
    limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_drain_immediate_when_no_in_flight() {
        let rm = RunManager::new();
        assert!(rm.wait_drain(std::time::Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn test_drain_waits_for_in_flight() {
        let rm = RunManager::new();
        rm.track_in_flight();

        let rm2 = rm.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            rm2.untrack_in_flight();
        });

        assert!(rm.wait_drain(std::time::Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn test_drain_times_out() {
        let rm = RunManager::new();
        rm.track_in_flight();
        // Never untrack — should time out.
        assert!(!rm.wait_drain(std::time::Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn test_multi_subscriber_broadcast() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx1);
        rm.register_sender(run_id, tx2);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        let e1 = rx1.recv().await.expect("subscriber 1 should receive");
        let e2 = rx2.recv().await.expect("subscriber 2 should receive");
        assert_eq!(e1.event_type, "connected");
        assert_eq!(e2.event_type, "connected");
    }

    #[tokio::test]
    async fn test_dead_subscriber_pruned() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let (tx_alive, mut rx_alive) = mpsc::unbounded_channel();
        let (tx_dead, rx_dead) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx_alive);
        rm.register_sender(run_id, tx_dead);

        // Drop the dead receiver so its sender becomes closed
        drop(rx_dead);

        rm.send_event(run_id, session_id, SseEventData::connected(run_id))
            .await;

        // Alive subscriber still gets the event
        let e = rx_alive
            .recv()
            .await
            .expect("alive subscriber should receive");
        assert_eq!(e.event_type, "connected");

        // Dead sender should have been pruned — only 1 sender left
        let senders = rm.event_senders.get(&run_id).unwrap();
        assert_eq!(senders.len(), 1);
    }

    #[tokio::test]
    async fn test_remove_senders_cleans_all() {
        let rm = RunManager::new();
        let run_id = RunId::new();

        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        rm.register_sender(run_id, tx1);
        rm.register_sender(run_id, tx2);

        assert!(rm.event_senders.contains_key(&run_id));
        rm.remove_senders(run_id);
        assert!(!rm.event_senders.contains_key(&run_id));
    }

    #[tokio::test]
    async fn test_cancel_run_triggers_token() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token.clone());

        assert!(!token.is_cancelled());
        assert!(rm.cancel_run(run_id));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancel_unknown_run_returns_false() {
        let rm = RunManager::new();
        assert!(!rm.cancel_run(RunId::new()));
    }

    #[tokio::test]
    async fn test_remove_cancel_token_cleanup() {
        let rm = RunManager::new();
        let run_id = RunId::new();
        let token = CancellationToken::new();
        rm.register_cancel_token(run_id, token);

        rm.remove_cancel_token(run_id);
        // After removal, cancel_run should return false
        assert!(!rm.cancel_run(run_id));
    }

    #[tokio::test]
    async fn test_mark_run_as_cancelled() {
        let rm = RunManager::new();
        let run = Run::new(SessionId::new(), AgentId::new(), "test".to_string());
        let run_id = run.run_id;
        rm.insert_run(run);
        rm.mark_run_as_running(run_id);
        rm.mark_run_as_cancelled(run_id);

        let r = rm.get_run(run_id).unwrap();
        assert_eq!(r.status, alms_core::RunStatus::Cancelled);
        assert!(r.ended_at.is_some());
    }

    #[tokio::test]
    async fn test_purge_terminal_senders_removes_completed() {
        let rm = RunManager::new();
        let active_id = RunId::new();
        let done_id = RunId::new();

        // Insert two runs: one running, one completed.
        let mut active_run = Run::new(SessionId::new(), AgentId::new(), "active".to_string());
        let mut done_run = Run::new(SessionId::new(), AgentId::new(), "done".to_string());
        // Override IDs for deterministic test.
        active_run.run_id = active_id;
        done_run.run_id = done_id;

        rm.insert_run(active_run);
        rm.insert_run(done_run);
        rm.mark_run_as_running(active_id);
        rm.mark_run_as_running(done_id);
        rm.mark_run_as_completed(done_id, "output".into(), Default::default());

        // Register senders for both.
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        rm.register_sender(active_id, tx1);
        rm.register_sender(done_id, tx2);

        assert!(rm.event_senders.contains_key(&active_id));
        assert!(rm.event_senders.contains_key(&done_id));

        rm.purge_terminal_senders();

        // Active run's sender should be kept, completed run's sender removed.
        assert!(rm.event_senders.contains_key(&active_id));
        assert!(!rm.event_senders.contains_key(&done_id));
    }

    #[tokio::test]
    async fn test_purge_terminal_senders_removes_missing_runs() {
        let rm = RunManager::new();
        let orphan_id = RunId::new();

        // Register a sender for a run that doesn't exist in the runs map.
        let (tx, _rx) = mpsc::unbounded_channel();
        rm.register_sender(orphan_id, tx);
        assert!(rm.event_senders.contains_key(&orphan_id));

        rm.purge_terminal_senders();

        // Sender for nonexistent run should be purged.
        assert!(!rm.event_senders.contains_key(&orphan_id));
    }
}
