//! Route registration and HTTP handler functions.
//!
//! Contains the Axum router setup (`public_router`, `protected_router`) and
//! all handler functions that are defined directly in this module (sessions,
//! audit, health check, WebSocket, web UI).  Handlers defined in other gateway
//! modules (agents, runs, approvals, etc.) are wired in via imports.

use super::AppState;
use crate::agents;
use crate::api_error;
use crate::approvals::{list_approvals, resolve_approval};
use crate::auth_keys;
use crate::jobs::{cancel_job, create_job, get_job, list_jobs};
use crate::runs::{
    cancel_run, create_run, get_run_status, get_run_tool_calls, is_internal_context_id, list_runs,
    stream_run_events,
};
use crate::settings::{get_settings, patch_settings};
use crate::workspace::{get_workspace, update_workspace_file};
use alms_core::{AgentId, SessionId};
use alms_session::{Content, Role};
use axum::{
    Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::info;

/// Routes that do NOT require authentication
pub(crate) fn public_router() -> Router<AppState> {
    // Static files get Cache-Control: no-store so browsers always fetch fresh
    // JS/CSS after a deployment, matching the HTML endpoint at `/`.
    let static_files = ServeDir::new("crates/alms-gateway/static/ui")
        .fallback(ServeFile::new("crates/alms-gateway/static/ui/index.html"));
    let static_with_cache = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        ))
        .service(static_files);

    Router::new()
        .route("/health", get(health_check))
        .nest_service("/ui", static_with_cache)
}

/// Routes that require authentication (all except /health)
pub(crate) fn protected_router() -> Router<AppState> {
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
        .route("/runs/{run_id}/tool-calls", get(get_run_tool_calls))
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
        // Settings (server defaults for UI pre-population + partial update)
        .route("/settings", get(get_settings).patch(patch_settings))
        // Jobs (scheduled agent runs)
        .route("/jobs", post(create_job).get(list_jobs))
        .route("/jobs/{job_id}", get(get_job).delete(cancel_job))
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
        include_str!("../../static/ui/index.html"),
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
/// Excludes internal sessions (DM, notifications, episodic, subagent, job)
/// — these are implementation details not shown in the UI.  Uses the same
/// `INTERNAL_SESSION_PREFIXES` list as `find_user_facing_session` to keep
/// the filter consistent.
async fn list_sessions(
    State(state): State<AppState>,
    Query(params): Query<ListSessionsQuery>,
) -> impl IntoResponse {
    let mut sessions = state.session_manager.list_all();
    sessions.retain(|s| !is_internal_context_id(&s.context_id));
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
///
/// Response includes `last_event_id` — the current high-water mark of the
/// session's SSE event log. Clients should pass this value as
/// `?last_event_id=<n>` when opening the SSE stream to skip replay of
/// events that are already reflected in the returned messages.
async fn get_session_messages(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> impl IntoResponse {
    tracing::debug!("GET /sessions/{}/messages", session_id.0);

    // Read the SSE high-water mark FIRST, before loading messages.
    // If an event arrives between these two reads, worst case is the
    // client replays a few events it already has (harmless duplicates)
    // rather than missing events entirely.
    let last_event_id = state.run_manager.latest_session_event_id(session_id).await;

    match state.session_manager.get_history(session_id) {
        Ok(messages) => {
            let total = messages.len();
            tracing::debug!("Session {} has {} total messages", session_id.0, total);
            let mut skipped: usize = 0;
            let visible: Vec<serde_json::Value> = messages
                .into_iter()
                .filter_map(|m| {
                    let role_str = match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "tool",
                        Role::System => {
                            // Pass through synthetic markers (job notifications,
                            // DM-ended markers, etc.) so they survive page reloads.
                            // Non-synthetic system messages (e.g. context-builder
                            // injections) are internal and should not be exposed.
                            let is_synthetic = m
                                .metadata
                                .as_ref()
                                .and_then(|md| md.get("synthetic"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if !is_synthetic {
                                skipped += 1;
                                return None;
                            }
                            "system"
                        }
                    };
                    let json = match &m.content {
                        Content::Text(t) => {
                            let mut obj = serde_json::json!({
                                "role": role_str,
                                "type": "text",
                                "content": t,
                                "timestamp": m.timestamp,
                            });
                            if let Some(ref md) = m.metadata {
                                obj["metadata"] = md.clone();
                            }
                            obj
                        }
                        Content::ToolCall { name, params } => serde_json::json!({
                            "role": role_str,
                            "type": "tool_call",
                            "tool": name,
                            "params": params,
                            "timestamp": m.timestamp,
                            "metadata": m.metadata,
                        }),
                        Content::ToolResult { tool_id, result } => {
                            let ok = m
                                .metadata
                                .as_ref()
                                .and_then(|md| md.get("ok"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let mut obj = serde_json::json!({
                                "role": role_str,
                                "type": "tool_result",
                                "tool_id": tool_id,
                                "result": result,
                                "ok": ok,
                                "timestamp": m.timestamp,
                            });
                            // Expose metadata (including tool_invocation_id)
                            // so the frontend can correlate tool results with
                            // their invocation across history reconstruction.
                            if let Some(ref md) = m.metadata {
                                obj["metadata"] = md.clone();
                            }
                            obj
                        }
                        Content::Image { url, alt } => serde_json::json!({
                            "role": role_str,
                            "type": "image",
                            "url": url,
                            "alt": alt,
                            "timestamp": m.timestamp,
                        }),
                    };
                    Some(json)
                })
                .collect();
            if skipped > 0 {
                tracing::debug!(
                    "Session {}: returned {} of {} messages ({} system messages excluded)",
                    session_id.0,
                    visible.len(),
                    total,
                    skipped,
                );
            }

            Json(serde_json::json!({
                "messages": visible,
                "last_event_id": last_event_id,
            }))
            .into_response()
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

#[derive(Debug, serde::Deserialize)]
pub(crate) struct CreateSessionRequest {
    agent_id: AgentId,
    context_id: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CreateSessionResponse {
    session_id: alms_core::SessionId,
    created: bool,
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    session_id: alms_core::SessionId,
    limit: Option<usize>,
}
