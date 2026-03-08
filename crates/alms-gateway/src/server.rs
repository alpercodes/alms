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
use crate::workspace::{get_workspace, update_workspace_file};
use alms_core::{AgentId, AlmsResult, JobStatus, Run, RunId, SessionId};
use alms_session::{Content, Role};
use alms_runtime::Scheduler;
use alms_session::{JobStore, SessionManager};
use axum::{
    Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use dashmap::DashMap;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

/// Run manager for tracking runs and their event streams
#[derive(Debug, Clone)]
pub struct RunManager {
    pub event_senders: Arc<DashMap<RunId, mpsc::UnboundedSender<SseEventData>>>,
    pub runs: Arc<DashMap<RunId, Run>>,
    /// Persistent event log for reconnect-after-restart support
    pub event_log: EventLogManager,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            event_senders: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            event_log: EventLogManager::new(),
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
    pub fn mark_run_as_completed(&self, run_id: RunId, output: String, usage: alms_core::TokenUsage) {
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
}

impl AppState {
    pub fn new(gateway: Gateway, scheduler: Arc<Scheduler>) -> AlmsResult<Self> {
        let workspace_dir = gateway.workspace_dir().map(|p| p.to_path_buf());
        let job_store = match gateway.db_path() {
            Some(path) => {
                tracing::info!("Opening SQLite job store at {}", path);
                Arc::new(JobStore::with_sqlite(path)?)
            }
            None => Arc::new(JobStore::new()),
        };
        Ok(Self {
            session_manager: gateway.session_manager().clone(),
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
            run_manager: RunManager::new(),
            approval_store: ApprovalStore::new(),
            workspace_dir,
            job_store,
            scheduler,
        })
    }
}

// Re-export SSE types
pub use crate::sse::{RunEventStream, event_channel};

/// Create the gateway router (per docs/api.md)
pub fn router() -> Router<AppState> {
    Router::new()
        // Web UI
        .route("/", get(serve_ui))
        // Health
        .route("/health", get(health_check))
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
        // Legacy (deprecated) - kept for MVP compatibility
        .route("/agent/run", post(run_agent))
        .route("/agent/run/stream", post(stream_run_legacy))
        .route("/ws", get(websocket_handler))
}

/// Serve the embedded web UI
async fn serve_ui() -> impl IntoResponse {
    axum::response::Html(include_str!("../static/index.html"))
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
async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.session_manager.list_all();
    Json(serde_json::json!({ "sessions": sessions }))
}

/// Create a new session
async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let session = state
        .session_manager
        .get_or_create(req.agent_id, req.context_id);

    Json(CreateSessionResponse {
        session_id: session.id,
        created: true,
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
    match state.session_manager.get_history(session_id) {
        Ok(messages) => {
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
    // Create the scheduler with a fire channel so job IDs are forwarded to
    // the gateway for actual agent-run dispatch.
    let (fire_tx, fire_rx) = tokio::sync::mpsc::unbounded_channel::<alms_core::JobId>();
    let scheduler = Arc::new(Scheduler::new().with_fire_channel(fire_tx));

    let state = AppState::new(gateway, scheduler)?;

    {
        let mut gateway = state.gateway.lock().await;
        gateway.initialize_channels().await?;
        gateway.start().await?;
    }

    // Re-register persisted jobs before starting the runner so the heap is
    // populated before the first sleep.
    bootstrap_scheduler(&state).await?;

    // Start the background scheduler runner.
    let _scheduler_handle = state.scheduler.start();

    // Spawn the fire-receiver: turns fired JobIds into real agent runs.
    let fire_state = state.clone();
    tokio::spawn(scheduler_fire_loop(fire_rx, fire_state));

    let background_gateway = state.gateway.clone();
    tokio::spawn(async move {
        let mut gateway = background_gateway.lock().await;
        if let Err(e) = gateway.run().await {
            tracing::error!("Gateway message loop exited: {}", e);
        }
    });

    let app = router().with_state(state);

    info!("Starting ALMS Gateway HTTP server on {}", bind_addr);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
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
