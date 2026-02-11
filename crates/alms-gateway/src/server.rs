//! HTTP server for ALMS Gateway
//!
//! Provides REST API endpoints per docs/api.md specification.

use crate::gateway::Gateway;
use crate::runs::{create_run, get_run_status, stream_run_events};
use crate::sse::{RunEventStream, SseEventData};
use alms_core::{AgentId, AlmsResult, EventLogManager, Run, RunId, SessionId};
use alms_session::SessionManager;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dashmap::DashMap;
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
        self.event_senders.get(&run_id).map(|s| s.clone())
    }

    pub fn remove_sender(&self, run_id: RunId) {
        self.event_senders.remove(&run_id);
    }

    pub fn insert_run(&self, run: Run) {
        self.runs.insert(run.run_id, run);
    }

    pub fn get_run(&self, run_id: RunId) -> Option<Run> {
        self.runs.get(&run_id).map(|r| r.clone())
    }

    pub fn update_run(&self, run: Run) {
        self.runs.insert(run.run_id, run);
    }

    /// Send event to active subscribers AND persist to event log
    pub async fn send_event(
        &self,
        run_id: RunId,
        session_id: SessionId,
        mut event: SseEventData,
    ) {
        // Log event and get persistent ID
        let event_id = self
            .event_log
            .log_event(run_id, session_id, &event.event_type, event.data.clone())
            .await;
        
        event.event_id = Some(event_id);
        
        // Send to active subscribers
        if let Some(sender) = self.get_sender(run_id) {
            let _ = sender.send(event);
        }
    }

    /// Get events from a specific ID for reconnect
    pub async fn events_from(&self, run_id: RunId, from_id: u64) -> Vec<alms_core::LoggedEvent> {
        self.event_log
            .get_or_create(run_id)
            .await
            .events_from(from_id)
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
}

impl AppState {
    pub fn new(gateway: Gateway) -> Self {
        Self {
            session_manager: gateway.session_manager().clone(),
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
            run_manager: RunManager::new(),
        }
    }
}

// Re-export SSE types
pub use crate::sse::{SseEventData, event_channel, RunEventStream};

/// Create the gateway router (per docs/api.md)
pub fn router() -> Router<AppState> {
    Router::new()
        // Health
        .route("/health", get(health_check))
        // Sessions
        .route("/sessions", post(create_session))
        .route("/sessions/:agent_id/:context_id", get(get_session))
        // Runs (per API spec)
        .route("/runs", post(create_run))
        .route("/runs/:run_id", get(get_run_status))
        .route("/runs/:run_id/events", get(stream_run_events))
        // Legacy (deprecated)
        .route("/agent/run", post(run_agent))
        .route("/ws", get(websocket_handler))
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "alms",
        "version": env!("CARGO_PKG_VERSION"),
    }))
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
    let session = state
        .session_manager
        .get_or_create(agent_id, context_id);
    
    Json(session)
}

/// Legacy: Run agent on a message (HTTP API) -- deprecated, use POST /runs
async fn run_agent(
    State(state): State<AppState>,
    Json(req): Json<RunAgentRequest>,
) -> impl IntoResponse {
    let gateway = state.gateway.lock().await;
    
    let runtime = alms_runtime::AgentRuntime::new(
        gateway.agent_id().clone(),
        gateway.agent_config().clone(),
        gateway.llm().clone(),
    );
    
    match runtime.run(
        &gateway.session_manager().clone(),
        &req.context_id,
        &req.message,
    ).await {
        Ok(response) => Json(serde_json::json!({
            "success": true,
            "response": response,
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
    let state = AppState::new(gateway);

    {
        let mut gateway = state.gateway.lock().await;
        gateway.initialize_channels().await?;
        gateway.start().await?;
    }

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