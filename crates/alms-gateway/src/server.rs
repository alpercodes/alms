//! HTTP/WebSocket server for ALMS Gateway
//!
//! Provides REST API endpoints and WebSocket connections for external control.

use crate::gateway::Gateway;
use crate::sse::{stream_run, SseEventData, SseEventType, RunId, ToolInvocationId};
use alms_core::{AgentId, AlmsResult};
use alms_session::SessionManager;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tracing::info;

// Re-export SSE types for consumers
pub use crate::sse::{EventSender, EventReceiver, event_channel};

/// Shared application state for HTTP server
#[derive(Debug, Clone)]
pub struct AppState {
    pub session_manager: Arc<SessionManager>,
    pub gateway: Arc<tokio::sync::Mutex<Gateway>>,
}

impl AppState {
    pub fn new(gateway: Gateway) -> Self {
        Self {
            session_manager: gateway.session_manager().clone(),
            gateway: Arc::new(tokio::sync::Mutex::new(gateway)),
        }
    }
}

/// Create the gateway router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .route("/sessions", post(create_session))
        .route("/sessions/:agent_id/:context_id", get(get_session))
        .route("/ws", get(websocket_handler))
        .route("/agent/run", post(run_agent))
        .route("/agent/run/stream", post(stream_run))
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "alms-gateway",
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

/// Run agent on a message (HTTP API)
async fn run_agent(
    State(state): State<AppState>,
    Json(req): Json<RunAgentRequest>,
) -> impl IntoResponse {
    let gateway = state.gateway.lock().await;
    
    // Create agent runtime
    let runtime = alms_runtime::AgentRuntime::new(
        gateway.agent_id().clone(),
        gateway.agent_config().clone(),
        gateway.llm().clone(),
    );
    
    // Run agent
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

/// WebSocket handler for real-time communication
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
) {
    info!("WebSocket connection established");
    // TODO: Implement WebSocket protocol for streaming responses
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
