//! HTTP server for ALMS Gateway
//!
//! Provides REST API endpoints per docs/api.md specification.

use crate::approvals::{ApprovalStore, list_approvals, resolve_approval};
use crate::cron_utils;
use crate::event_log::{EventLogManager, LoggedEvent};
use crate::gateway::Gateway;
use crate::jobs::{cancel_job, create_job, get_job, list_jobs};
use crate::runs::{
    create_run, get_run_status, list_runs, scheduler_fire_loop, stream_run_events,
    stream_run_legacy,
};
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
    Extension, Json, Router, middleware,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use crate::auth::{AuthToken, require_auth};
use dashmap::DashMap;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Run manager for tracking runs and their event streams
#[derive(Debug, Clone)]
pub struct RunManager {
    pub event_senders: Arc<DashMap<RunId, mpsc::UnboundedSender<SseEventData>>>,
    pub runs: Arc<DashMap<RunId, Run>>,
    /// Persistent event log for reconnect-after-restart support
    pub event_log: EventLogManager,
    /// Counter of in-flight (spawned but not yet finished) run tasks.
    in_flight: Arc<AtomicUsize>,
    /// Notified when an in-flight run completes (counter reaches zero).
    drain_notify: Arc<tokio::sync::Notify>,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            event_senders: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            event_log: EventLogManager::new(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            drain_notify: Arc::new(tokio::sync::Notify::new()),
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
        self.event_senders.insert(run_id, sender);
    }

    pub fn get_sender(&self, run_id: RunId) -> Option<mpsc::UnboundedSender<SseEventData>> {
        self.event_senders.get(&run_id).map(|s| s.value().clone())
    }

    pub fn remove_sender(&self, run_id: RunId) {
        self.event_senders.remove(&run_id);
    }

    pub fn insert_run(&self, run: Run) {
        self.runs.insert(run.run_id, run);
    }

    pub fn get_run(&self, run_id: RunId) -> Option<Run> {
        self.runs.get(&run_id).map(|r| r.value().clone())
    }

    pub fn update_run(&self, run: Run) {
        self.runs.insert(run.run_id, run);
    }

    /// Atomically transition a run to Running state.
    pub fn mark_run_as_running(&self, run_id: RunId) {
        self.runs.entry(run_id).and_modify(|r| r.mark_running());
    }

    /// Atomically transition a run to Completed state.
    pub fn mark_run_as_completed(
        &self,
        run_id: RunId,
        output: String,
        usage: alms_core::TokenUsage,
    ) {
        self.runs
            .entry(run_id)
            .and_modify(|r| r.mark_completed(output.clone(), usage));
    }

    /// Atomically transition a run to Failed state.
    pub fn mark_run_as_failed(&self, run_id: RunId, error: String) {
        self.runs
            .entry(run_id)
            .and_modify(|r| r.mark_failed(error.clone()));
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

    /// Send event to active subscribers AND persist to event log
    pub async fn send_event(&self, run_id: RunId, session_id: SessionId, mut event: SseEventData) {
        let event_id = self
            .event_log
            .log_event(run_id, session_id, &event.event_type, event.data.clone())
            .await;

        event.event_id = Some(event_id);

        if let Some(sender) = self.get_sender(run_id) {
            let _ = sender.send(event);
        }
    }

    /// Get events from a specific ID for reconnect
    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<LoggedEvent> {
        self.event_log.events_from(run_id, from_id).await
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
    /// Job store for scheduled jobs
    pub job_store: Arc<JobStore>,
    /// Scheduler for firing jobs at the right time
    pub scheduler: Arc<Scheduler>,
    /// Coordinator for subagent lifecycle management
    pub coordinator: Arc<Coordinator>,
    /// Token cancelled during graceful shutdown.
    pub shutdown_token: CancellationToken,
}

impl AppState {
    pub fn new(
        gateway: Gateway,
        scheduler: Arc<Scheduler>,
        shutdown_token: CancellationToken,
    ) -> AlmsResult<Self> {
        let workspace_dir = gateway.workspace_dir().map(|p| p.to_path_buf());
        let session_manager = gateway.session_manager().clone();
        let llm = gateway.llm().clone();
        let agent_id = *gateway.agent_id();
        let job_store = match gateway.db_path() {
            Some(path) => {
                tracing::info!("Opening SQLite job store at {}", path);
                Arc::new(JobStore::with_sqlite(path)?)
            }
            None => Arc::new(JobStore::new()),
        };
        let coordinator = Arc::new(Coordinator::new(agent_id, session_manager.clone(), llm));
        Ok(Self {
            session_manager,
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
            run_manager: RunManager::new(),
            approval_store: ApprovalStore::new(),
            workspace_dir,
            job_store,
            scheduler,
            coordinator,
            shutdown_token,
        })
    }
}

// Re-export SSE types
pub use crate::sse::{RunEventStream, event_channel};

/// Routes that do NOT require authentication
fn public_router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}

/// Routes that require authentication (all except /health)
fn protected_router() -> Router<AppState> {
    Router::new()
        // Web UI
        .route("/", get(serve_ui))
        // Sessions
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route("/sessions/{agent_id}/{context_id}", get(get_session))
        // Runs (canonical API per spec)
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{run_id}", get(get_run_status))
        .route("/runs/{run_id}/events", get(stream_run_events))
        // Approvals
        .route("/approvals", get(list_approvals))
        .route("/approvals/{approval_id}", post(resolve_approval))
        // Audit
        .route("/audit", get(get_audit))
        // Workspace (agent identity files)
        .route("/agents/{agent_id}/workspace", get(get_workspace))
        .route(
            "/agents/{agent_id}/workspace/{file}",
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
        // Legacy (deprecated) - kept for MVP compatibility
        .route("/agent/run", post(run_agent))
        .route("/agent/run/stream", post(stream_run_legacy))
        .route("/ws", get(websocket_handler))
}

/// Serve the embedded web UI
async fn serve_ui() -> impl IntoResponse {
    (
        [
            ("Cache-Control", "no-store"),
            ("Content-Type", "text/html; charset=utf-8"),
        ],
        include_str!("../static/index.html"),
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

/// GET /sessions — list all sessions across all agents
///
/// Excludes internal sessions created by the coordinator (subagent_*) and
/// scheduler (job_*) — these are implementation details not shown in the UI.
async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let mut sessions = state.session_manager.list_all();
    sessions.retain(|s| {
        !s.context_id.starts_with("subagent_") && !s.context_id.starts_with("job_")
    });
    Json(serde_json::json!({ "sessions": sessions }))
}

/// Create a new session
async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let key = (req.agent_id, req.context_id.clone());
    let existed = state.session_manager.has_session(&key);
    let session = state
        .session_manager
        .get_or_create(key.0, key.1);

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

/// GET /sessions/{session_id}/messages — return user/assistant chat history
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
                .filter_map(|m| {
                    let role = match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        _ => return None, // skip system / tool messages
                    };
                    let text = match m.content {
                        Content::Text(t) => t,
                        _ => return None, // skip non-text content
                    };
                    Some(serde_json::json!({
                        "role": role,
                        "content": text,
                        "timestamp": m.timestamp,
                    }))
                })
                .collect();
            Json(serde_json::json!({ "messages": visible })).into_response()
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "Session not found" }
            })),
        )
            .into_response(),
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
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "Session not found" }
            })),
        )
            .into_response(),
    }
}

/// Legacy: Run agent on a message (HTTP API) -- deprecated, use POST /runs
async fn run_agent(
    State(state): State<AppState>,
    Json(req): Json<RunAgentRequest>,
) -> impl IntoResponse {
    // Reject during shutdown.
    if state.shutdown_token.is_cancelled() {
        return Json(serde_json::json!({
            "success": false,
            "error": "Server is shutting down",
        }));
    }

    // Extract what we need, then drop the lock before the LLM call
    let (agent_id, agent_config, llm) = {
        let gateway = state.gateway.lock().await;
        (
            *gateway.agent_id(),
            gateway.agent_config().clone(),
            gateway.llm().clone(),
        )
    };

    let runtime = alms_runtime::AgentRuntime::new(agent_id, agent_config, llm);

    match runtime
        .run(&state.session_manager, &req.context_id, &req.message)
        .await
    {
        Ok(output) => Json(serde_json::json!({
            "success": true,
            "response": output.response,
        })),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e.to_string(),
        })),
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

/// Start the gateway HTTP server
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

    let state = AppState::new(gateway, scheduler, shutdown_token.clone())?;

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

    // Spawn the channel message loop (Telegram polling, etc.).
    // The loop selects on the shutdown token so it exits cooperatively
    // without requiring us to lock the gateway mutex from outside.
    let background_gateway = state.gateway.clone();
    let gateway_token = shutdown_token.clone();
    let gateway_handle = tokio::spawn(async move {
        let mut gateway = background_gateway.lock().await;
        if let Err(e) = gateway.run_until_shutdown(gateway_token).await {
            tracing::error!("Gateway message loop exited: {}", e);
        }
    });

    let auth_token = {
        let gateway = state.gateway.lock().await;
        AuthToken(gateway.auth_token().map(String::from))
    };
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
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_token))
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
async fn shutdown_signal(token: CancellationToken) {
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
        ctrl_c
            .await
            .expect("failed to install Ctrl+C handler");
        info!("Received Ctrl+C, initiating graceful shutdown");
    }

    token.cancel();
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

#[derive(Debug, serde::Deserialize)]
struct RunAgentRequest {
    context_id: String,
    message: String,
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
}
