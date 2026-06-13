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
use crate::auth::SSE_ENDPOINT_SEGMENTS;
use crate::auth_keys;
use crate::jobs::{cancel_job, create_job, get_job, list_jobs};
use crate::runs::{
    cancel_dm, cancel_run, classify_session_type, create_run, get_run_reasoning, get_run_status,
    get_run_text, get_run_tool_calls, is_internal_context_id, list_runs, stream_run_events,
};
use crate::settings::{get_settings, patch_settings};
use crate::workspace::{get_workspace, open_workspace, update_workspace_file};
use alms_core::{AgentId, SessionId, dm_participants};
use alms_session::{Content, Role, Session};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{MethodRouter, delete, get, post},
};
use rust_embed::Embed;
use serde::Deserialize;
use std::borrow::Cow;
use tracing::info;

/// Static UI assets embedded into the binary at compile time.
///
/// During release builds every file under `static/ui/` is baked into the
/// binary so the server works from any working directory.
#[derive(Embed)]
#[folder = "static/ui/"]
struct UiAssets;

/// Routes that do NOT require authentication
pub(crate) fn public_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .route("/ui/{*path}", get(serve_embedded_asset))
        .route("/ui", get(serve_embedded_index))
        .route("/ui/", get(serve_embedded_index))
}

/// Serve a static asset from the embedded UI files.
///
/// Falls back to `index.html` for paths that don't match any file so that
/// client-side routing (SPA) works correctly.
async fn serve_embedded_asset(Path(path): Path<String>) -> axum::response::Response {
    serve_embedded_file(&path)
}

/// Serve the embedded `index.html` for bare `/ui` and `/ui/` requests.
async fn serve_embedded_index() -> axum::response::Response {
    serve_embedded_file("index.html")
}

/// Convert a `Cow<'static, [u8]>` into `Bytes` without copying when the
/// data is statically borrowed (the common case in release builds).
fn cow_to_bytes(data: Cow<'static, [u8]>) -> Bytes {
    match data {
        Cow::Borrowed(slice) => Bytes::from_static(slice),
        Cow::Owned(vec) => Bytes::from(vec),
    }
}

/// Look up a file in the embedded assets and return it with the correct
/// `Content-Type` and `Cache-Control: no-store`.
fn serve_embedded_file(path: &str) -> axum::response::Response {
    match UiAssets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                cow_to_bytes(file.data),
            )
                .into_response()
        }
        // SPA fallback: if the path has no extension (likely a client-side
        // route), serve index.html.  Otherwise return 404.
        None if !path.contains('.') => {
            if let Some(index) = UiAssets::get("index.html") {
                let mime = index.metadata.mimetype();
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, mime.to_string()),
                        (header::CACHE_CONTROL, "no-store".to_string()),
                    ],
                    cow_to_bytes(index.data),
                )
                    .into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Returns the canonical list of SSE route specs registered on the protected
/// router, derived from [`SSE_ENDPOINT_SEGMENTS`].
///
/// Each entry is `(axum_path, handler)` where `axum_path` is the full path
/// string with the `{id}` placeholder (e.g. `"/runs/{id}/events"`).  The
/// path is constructed from the segment, so adding a new SSE endpoint is a
/// two-line change: append a segment to [`SSE_ENDPOINT_SEGMENTS`] and add a
/// matching `match` arm here for the handler.
///
/// This is the single source of truth for:
/// - [`protected_router`] (production route registration), and
/// - the in-test `Router` in [`crate::auth::tests`] (regression guard).
///
/// If an entry is added to [`SSE_ENDPOINT_SEGMENTS`] without a matching arm
/// below, the server panics at startup — a loud failure that catches the
/// drift bug #905 / PR #904 closed.
pub(crate) fn sse_route_specs() -> Vec<(String, MethodRouter<AppState>)> {
    SSE_ENDPOINT_SEGMENTS
        .iter()
        .map(|seg| {
            let path = format!("/{seg}/{{id}}/events");
            let handler: MethodRouter<AppState> = match *seg {
                "runs" => get(stream_run_events),
                "sessions" => get(crate::runs::stream_session_events),
                "agents" => get(crate::runs::stream_agent_events),
                other => panic!(
                    "SSE_ENDPOINT_SEGMENTS contains \"{other}\" with no handler mapping in \
                     sse_route_specs(); add a match arm in crates/alms-gateway/src/server/routes.rs"
                ),
            };
            (path, handler)
        })
        .collect()
}

/// Returns just the axum path strings for the SSE routes, derived from
/// [`SSE_ENDPOINT_SEGMENTS`].
///
/// Stateless helper used by the auth test app, where the production
/// handlers (which require `AppState`) can't be wired up.  Test-only —
/// production code paths go through [`sse_route_specs`] which carries
/// the handler alongside the path.
#[cfg(test)]
pub(crate) fn sse_route_paths() -> Vec<String> {
    SSE_ENDPOINT_SEGMENTS
        .iter()
        .map(|seg| format!("/{seg}/{{id}}/events"))
        .collect()
}

/// Routes that require authentication (all except /health)
pub(crate) fn protected_router() -> Router<AppState> {
    let mut router = Router::new()
        // Web UI
        .route("/", get(serve_ui))
        // Sessions
        .route("/sessions", get(list_sessions).post(create_session))
        // Single-session metadata probe (#1065). Singular path is a
        // deliberate visual-separation choice — `matchit` (axum's path
        // matcher) distinguishes routes by segment count first, so a
        // single-segment `GET /sessions/<uuid>` would NOT collide with
        // the two-segment `GET /sessions/{agent_id}/{context_id}` route
        // below; both already coexist (see the `/sessions/{session_id}`
        // `delete` and `/messages`/`/tool-calls` routes). The singular
        // path is kept because `GET /sessions/<uuid>` returning a single
        // envelope vs `GET /sessions` returning a list-of-envelopes
        // reads as visually similar and tripped up frontend code during
        // the #1065 review; the different segment name removes that
        // ambiguity for human readers.
        .route("/session/{session_id}", get(get_session_metadata))
        .route("/sessions/{session_id}", delete(delete_session_by_id))
        .route("/sessions/{session_id}/messages", get(get_session_messages))
        .route(
            "/sessions/{session_id}/tool-calls",
            get(get_session_tool_calls),
        )
        .route("/sessions/{session_id}/cancel-dm", post(cancel_dm))
        .route("/sessions/{agent_id}/{context_id}", get(get_session))
        // Runs (canonical API per spec)
        .route("/runs", get(list_runs).post(create_run))
        .route("/runs/{run_id}", get(get_run_status))
        .route("/runs/{run_id}/cancel", post(cancel_run))
        .route("/runs/{run_id}/tool-calls", get(get_run_tool_calls))
        .route("/runs/{run_id}/reasoning", get(get_run_reasoning))
        .route("/runs/{run_id}/text", get(get_run_text))
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
        // #858: open the workspace dir in the host file explorer.
        // Registered BEFORE the `{file}` PUT route so axum's path matcher
        // resolves `/workspace/open` to this handler instead of treating
        // "open" as a file slug — a PUT on `/workspace/open` would have
        // hit the file-overwrite handler with `file = "open"` and 404'd
        // anyway, but POST is unambiguous either way.
        .route("/agents/{id_or_name}/workspace/open", post(open_workspace))
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
        // Timeline (cross-channel unified activity view)
        .route(
            "/agents/{id_or_name}/timeline",
            get(crate::timeline::get_agent_timeline),
        )
        .route("/ws", get(websocket_handler));

    // SSE streaming endpoints — registered from the canonical
    // SSE_ENDPOINT_SEGMENTS list via sse_route_specs() so the auth
    // middleware's is_sse_endpoint() matcher and the production route
    // table cannot drift (#905).  Each entry contributes one route:
    //   /runs/{id}/events     -> stream_run_events
    //   /sessions/{id}/events -> stream_session_events
    //   /agents/{id}/events   -> stream_agent_events  (#856 — emits
    //                            session_activity_started/_ended for
    //                            runs across all of the agent's sessions)
    for (path, handler) in sse_route_specs() {
        router = router.route(&path, handler);
    }
    router
}

/// Serve the embedded web UI (the `/` route behind auth).
async fn serve_ui() -> axum::response::Response {
    serve_embedded_file("index.html")
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "alms",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// GET /sessions?agent_id=<uuid>&include_dms=true — list sessions.
///
/// By default, excludes the truly internal session types (episodic,
/// subagent, job) — these are implementation details not shown in the
/// regular UI. Uses the same `INTERNAL_SESSION_PREFIXES` list as
/// `find_user_facing_session` to keep the filter consistent.
///
/// Notification sessions (`notifications:*` context IDs) are
/// **unconditionally included** in the response. They surface real
/// operator-facing activity (DM endings, subagent completions, peer
/// messages) and are always rendered in the sidebar's Notifications
/// section. They participate in the `agent_id` filter on this endpoint
/// — when no `agent_id` is supplied the cross-agent fetch picks up
/// notifications for every agent in the tenant.
///
/// Optional inclusion flag:
///
/// - `include_dms=true` — include DM sessions (`dm:*` context IDs).
///   DM sessions are stored under `AgentId::nil()` (sentinel), so the
///   `agent_id` filter is not applied to them — instead they are included
///   based on participant names parsed from the context ID.
///
/// Each session in the response is enriched with:
/// - `session_type`: one of `"chat"`, `"dm"`, `"notification"`, `"job"`,
///   `"subagent"`, `"telegram"`, `"episodic"` (derived from `context_id`)
/// - `participants`: `[name1, name2]` for DM sessions (parsed from `context_id`)
/// - `agent_name`: agent name extracted from `notifications:{agent}` context IDs
/// - `has_active_run`: `true` if any queued or running run is currently
///   tied to this session — drives the sidebar's "active" indicator on
///   the initial load and after SSE reconnect (#856). Pairs with the
///   agent-scoped SSE feed (`GET /agents/{agent_id}/events`) which emits
///   live transitions between calls to this endpoint.
async fn list_sessions(
    State(state): State<AppState>,
    Query(params): Query<ListSessionsQuery>,
) -> impl IntoResponse {
    let include_dms = params.include_dms.unwrap_or(false);
    let all_sessions = state.session_manager.list_all();

    let mut result: Vec<serde_json::Value> = Vec::new();

    for session in all_sessions {
        let session_type = classify_session_type(&session.context_id);
        let is_internal = is_internal_context_id(&session.context_id);

        match session_type {
            "dm" => {
                // DM sessions: only include when explicitly requested
                if !include_dms {
                    continue;
                }
                // agent_id filter does not apply to DM sessions (they use nil sentinel)
            }
            "notification" => {
                // Notification sessions are always shown in the sidebar
                // (no opt-in toggle). Apply the agent_id filter so a
                // per-agent fetch only returns notifications owned by
                // that agent; the cross-agent fetch (`agent_id` unset)
                // picks up every notification in the tenant.
                if let Some(agent_id) = params.agent_id
                    && session.agent_id != agent_id
                {
                    continue;
                }
            }
            _ if is_internal => {
                // Other internal sessions (job, subagent, episodic): always excluded
                continue;
            }
            _ => {
                // Regular user-facing sessions: apply agent_id filter
                if let Some(agent_id) = params.agent_id
                    && session.agent_id != agent_id
                {
                    continue;
                }
            }
        }

        // Build the enriched JSON object.
        //
        // `include_parent_session_id = false` here: the per-subagent-session
        // `parent_run_id` -> parent `session_id` walk would be an N x lookup
        // across all subagent rows in `runs`, and the sidebar does not need
        // it (subagent sessions are filtered out of `list_sessions` entirely
        // unless an inclusion flag is added later). The new
        // `GET /session/{session_id}` endpoint (Iris's spec for #1065) is
        // the cheap one-shot probe path that pays for it.
        let obj = enrich_session_json(&state, &session, session_type, false);

        result.push(obj);
    }

    Json(serde_json::json!({ "sessions": result }))
}

/// Build the enriched JSON object for a single session, used both by the
/// `GET /sessions` list endpoint and the `GET /session/{session_id}`
/// single-session lookup.
///
/// Adds the same fields list_sessions has always exposed
/// (`session_type`, `has_active_run`, `participants` for DM,
/// `agent_name` for notification sessions), plus — when
/// `include_parent_session_id` is true — a `parent_session_id` field
/// for sessions whose `session_type` is `"subagent"`.
///
/// `parent_session_id` derivation:
///
/// 1. Walk ALL runs on this session and pick the first one whose
///    `parent_run_id` is `Some(_)`. Walking every run (not just the
///    newest) matters because a later run on the same `subagent_*`
///    session may carry `parent_run_id: None` — e.g. a manual
///    `POST /runs` that goes through `Run::new` instead of
///    `Run::for_subagent`, or a system-triggered notification run.
///    The original spawning run (which by construction goes through
///    `Run::for_subagent`) is the breadcrumb we need to surface.
/// 2. Look up that parent run by id and return its `session_id`.
/// 3. Return `null` if either lookup misses — the parent run may no
///    longer be resident in memory (`run_manager` is in-process), and
///    a missing parent should never fail this read-only probe.
///
/// Memory cost: subagent sessions are typically small (a handful of
/// runs each), and `list_by_session` already clones a `Vec<Run>` of
/// the full session today regardless of `limit` (it filters, collects,
/// sorts, then truncates), so passing `usize::MAX` here is no more
/// expensive than the previous `1`. The `find` short-circuits on the
/// first parent-linked match.
///
/// `parent_session_id` is omitted from non-subagent sessions because
/// the field is meaningless there; the field is always emitted (as
/// `null`) for subagent sessions so the frontend can branch on its
/// presence without checking `session_type` first.
fn enrich_session_json(
    state: &AppState,
    session: &Session,
    session_type: &'static str,
    include_parent_session_id: bool,
) -> serde_json::Value {
    let mut obj = serde_json::to_value(session).unwrap_or_default();
    obj["session_type"] = serde_json::json!(session_type);
    // `has_active_run` powers the sidebar's "active" indicator on
    // initial load / SSE reconnect (#856).
    obj["has_active_run"] = serde_json::json!(state.run_manager.has_active_runs(session.id));

    // Type-specific enrichments.
    match session_type {
        "dm" => {
            if let Some((a, b)) = dm_participants(&session.context_id) {
                obj["participants"] = serde_json::json!([a, b]);
            }
        }
        "notification" => {
            // Extract agent name from "notifications:{agent}" context_id.
            if let Some(agent_name) = session.context_id.strip_prefix("notifications:") {
                obj["agent_name"] = serde_json::json!(agent_name);
            }
        }
        _ => {}
    }

    // Parent-session breadcrumb derivation (#1065). Only meaningful for
    // subagent sessions; emitted as `null` when the parent run is no
    // longer resident (defensive — `run_manager` is in-memory).
    //
    // Walk ALL runs on the session and pick the first one carrying
    // `parent_run_id`. Looking at only the newest run (as the initial
    // shipping of #1065 did) would miss the breadcrumb if a later
    // parent-less run was added to the same subagent session — e.g. a
    // manual `POST /runs` going through `Run::new` instead of
    // `Run::for_subagent`, or a system-triggered notification run.
    if include_parent_session_id && session_type == "subagent" {
        let parent_session_id = state
            .run_manager
            .list_by_session(session.id, usize::MAX)
            .into_iter()
            .find_map(|run| run.parent_run_id)
            .and_then(|parent_run_id| state.run_manager.get_run(parent_run_id))
            .map(|parent_run| parent_run.session_id);
        obj["parent_session_id"] = match parent_session_id {
            Some(id) => serde_json::json!(id),
            None => serde_json::Value::Null,
        };
    }

    obj
}

/// GET /session/{session_id} — return the enriched session envelope for a
/// single session (#1065).
///
/// Singular `/session/` (not plural `/sessions/`) is a deliberate
/// visual-separation choice — `matchit` (axum's path matcher)
/// distinguishes routes by segment count first, so a one-segment
/// `GET /sessions/<uuid>` would NOT collide with the two-segment
/// `GET /sessions/{agent_id}/{context_id}` handler. The singular form
/// is preferred because `GET /sessions/<uuid>` (single envelope) and
/// `GET /sessions` (list of envelopes) read as visually similar and
/// tripped up frontend code during the #1065 review; the different
/// segment name removes that ambiguity for human readers.
///
/// Returns the same fields as a single entry from `GET /sessions`
/// (`id`, `agent_id`, `context_id`, `created_at`, `last_activity`,
/// `status`, `session_type`, `has_active_run`, plus `participants` /
/// `agent_name` for the DM / notification cases) and additionally
/// surfaces `parent_session_id` for subagent sessions so the frontend
/// can render the "← Back to parent session" breadcrumb and the
/// "Subagent session — read-only" header on resolver-led boot.
async fn get_session_metadata(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> impl IntoResponse {
    match state.session_manager.get(session_id) {
        Ok(session) => {
            let session_type = classify_session_type(&session.context_id);
            let body = enrich_session_json(&state, &session, session_type, true);
            Json(body).into_response()
        }
        Err(_) => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found").into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListSessionsQuery {
    agent_id: Option<AgentId>,
    /// When `true`, DM sessions (`dm:*` context IDs) are included in the
    /// response alongside regular user-facing sessions.
    include_dms: Option<bool>,
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

/// DELETE /sessions/{session_id} — delete a session by ID.
async fn delete_session_by_id(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> impl IntoResponse {
    // Look up the session to get agent_id + context_id, then delete.
    match state.session_manager.get(session_id) {
        Ok(session) => {
            // Refuse to delete a session that has active (queued/running) runs.
            if state.run_manager.has_active_runs(session_id) {
                return api_error(
                    StatusCode::CONFLICT,
                    "ACTIVE_RUNS",
                    "Cannot delete session with active runs",
                )
                .into_response();
            }
            match state
                .session_manager
                .delete(session.agent_id, &session.context_id)
            {
                Ok(()) => {
                    Json(serde_json::json!({ "ok": true, "deleted": session_id.0.to_string() }))
                        .into_response()
                }
                Err(e) => {
                    api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", e).into_response()
                }
            }
        }
        Err(_) => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found").into_response()
        }
    }
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
                    // Filter out notification input messages (Role::User
                    // with `notification_input: true` metadata). These are
                    // internal LLM prompts persisted by execute_run for
                    // notification runs landing on user-facing sessions.
                    // They must be Role::User for LLM API compatibility
                    // (Anthropic requires a trailing user turn; OpenRouter
                    // models produce poor responses to trailing system
                    // messages) but should not appear as "user" bubbles
                    // in the chat UI.
                    let is_notification_input = m
                        .metadata
                        .as_ref()
                        .and_then(|md| md.get("notification_input"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_notification_input {
                        skipped += 1;
                        return None;
                    }

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

/// GET /sessions/{session_id}/tool-calls — return all tool call records across
/// all runs for a session, ordered by run creation time then sequence number.
///
/// This endpoint supplements the per-run `GET /runs/{run_id}/tool-calls` by
/// providing a session-level view.  It is especially important for DM sessions
/// where tool calls are stored only in `run_tool_calls` (not in
/// `session_messages`) and would otherwise be lost on page reload.
async fn get_session_tool_calls(
    State(state): State<AppState>,
    Path(session_id): Path<SessionId>,
) -> impl IntoResponse {
    tracing::debug!("GET /sessions/{}/tool-calls", session_id.0);

    // Verify the session exists.
    if state.session_manager.get(session_id).is_err() {
        return api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Session not found").into_response();
    }

    let records = state
        .session_manager
        .store()
        .map(|store| store.load_tool_calls_for_session(session_id))
        .transpose();

    match records {
        Ok(Some(tool_calls)) => Json(serde_json::json!({
            "session_id": session_id.0.to_string(),
            "tool_calls": tool_calls,
        }))
        .into_response(),
        Ok(None) => {
            // No SQLite store — return empty list.
            Json(serde_json::json!({
                "session_id": session_id.0.to_string(),
                "tool_calls": [],
            }))
            .into_response()
        }
        Err(e) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            format!("Failed to load tool calls: {e}"),
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

// ---------------------------------------------------------------------------
// Tests — `GET /session/{session_id}` (#1065)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Tests for the `GET /session/{session_id}` single-session metadata
    //! endpoint added for #1065 (subagent breadcrumb + read-only banner on
    //! resolver-led boot).
    //!
    //! The tests exercise the four cases called out in Iris's spec:
    //!
    //! - 200 for a regular chat session (`session_type: "chat"`,
    //!   `parent_session_id` omitted)
    //! - 200 for a subagent session whose parent run is resident
    //!   (`session_type: "subagent"`, `parent_session_id` = parent's
    //!   `session_id`)
    //! - 200 for a subagent session whose parent run is no longer resident
    //!   (`parent_session_id: null` — defensive case)
    //! - 404 for a non-existent session id
    //! - 200 for a DM session — exercises the `participants` enrichment
    //!   reused from `list_sessions`.
    //! - 200 for a notification session — exercises the `agent_name`
    //!   enrichment reused from `list_sessions`.
    use super::*;
    use crate::server::AppState;
    use alms_core::Run;

    /// Build a minimal `AppState` with in-memory session storage. Matches
    /// `runs::integration_tests::test_app_state` but lives here so the
    /// `routes` test module is self-contained.
    fn test_app_state() -> AppState {
        let gateway = crate::gateway::Gateway::new(crate::gateway::GatewayConfig::default())
            .expect("gateway construction");
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _trigger_rx) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = tokio::sync::mpsc::channel(8);
        AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .expect("AppState::new")
    }

    /// Drive the handler through axum's `IntoResponse` and return
    /// `(StatusCode, JSON body)`. Mirrors the `invoke_open` helper in
    /// `crate::workspace::tests`.
    async fn invoke_get_session_metadata(
        state: AppState,
        session_id: SessionId,
    ) -> (StatusCode, serde_json::Value) {
        use axum::body::to_bytes;
        let resp =
            get_session_metadata(axum::extract::State(state), axum::extract::Path(session_id))
                .await
                .into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("body to bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, body)
    }

    #[tokio::test]
    async fn get_session_metadata_returns_chat_envelope() {
        let state = test_app_state();
        let agent_id = AgentId::new();
        let session = state
            .session_manager
            .get_or_create(agent_id, "regular-chat");
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], serde_json::json!(session_id));
        assert_eq!(body["agent_id"], serde_json::json!(agent_id));
        assert_eq!(body["context_id"], "regular-chat");
        assert_eq!(body["session_type"], "chat");
        assert_eq!(body["has_active_run"], false);
        // `parent_session_id` is only added for subagent sessions — confirm
        // it is absent (not present-as-null) on the chat path so callers
        // can use field presence as a shortcut to "is this a subagent
        // session" if they want to.
        assert!(
            body.get("parent_session_id").is_none(),
            "parent_session_id should be omitted for non-subagent sessions; \
             got body = {body}"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_returns_404_for_unknown_session() {
        let state = test_app_state();
        let unknown = SessionId::new();

        let (status, body) = invoke_get_session_metadata(state, unknown).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn get_session_metadata_subagent_resolves_parent_session_id() {
        let state = test_app_state();
        let parent_agent_id = AgentId::new();
        let sub_agent_id = AgentId::new();

        // Parent session + a single Run on it — this Run is what the
        // subagent run's `parent_run_id` will point at.
        let parent_session = state
            .session_manager
            .get_or_create(parent_agent_id, "parent-chat");
        let parent_session_id = parent_session.id;
        let parent_run = Run::new(parent_session_id, parent_agent_id, "user input".into());
        let parent_run_id = parent_run.run_id;
        state.run_manager.insert_run(parent_run);

        // Subagent session (context_id must start with `subagent_` so
        // `classify_session_type` returns `"subagent"`), plus a run on
        // that session whose `parent_run_id` is set.
        let sub_context_id = format!("subagent_{}", uuid::Uuid::new_v4());
        let sub_session = state
            .session_manager
            .get_or_create(sub_agent_id, sub_context_id);
        let sub_session_id = sub_session.id;
        let sub_run = Run::for_subagent(
            sub_session_id,
            sub_agent_id,
            "subagent input".into(),
            parent_run_id,
        );
        state.run_manager.insert_run(sub_run);

        let (status, body) = invoke_get_session_metadata(state, sub_session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert_eq!(
            body["parent_session_id"],
            serde_json::json!(parent_session_id)
        );
    }

    #[tokio::test]
    async fn get_session_metadata_subagent_missing_parent_run_returns_null() {
        // Defensive case from Iris's spec: subagent session exists, but
        // the parent run is no longer resident in `run_manager`. Endpoint
        // must return 200 with `parent_session_id: null` rather than
        // surfacing the lookup miss as an error.
        let state = test_app_state();
        let sub_agent_id = AgentId::new();

        let sub_context_id = format!("subagent_{}", uuid::Uuid::new_v4());
        let sub_session = state
            .session_manager
            .get_or_create(sub_agent_id, sub_context_id);
        let sub_session_id = sub_session.id;

        // Subagent run points at a parent_run_id whose Run is NOT in
        // run_manager (simulating "parent run evicted from memory").
        let phantom_parent_run_id = alms_core::RunId::new();
        let sub_run = Run::for_subagent(
            sub_session_id,
            sub_agent_id,
            "subagent input".into(),
            phantom_parent_run_id,
        );
        state.run_manager.insert_run(sub_run);

        let (status, body) = invoke_get_session_metadata(state, sub_session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert!(
            body["parent_session_id"].is_null(),
            "expected null parent_session_id when parent run is not resident; \
             got body = {body}"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_subagent_later_parentless_run_does_not_blind_lookup() {
        // Codex follow-up on #1067: if a later run on the same subagent
        // session lands without `parent_run_id` (e.g. a manual
        // `POST /runs` going through `Run::new`, or a system-triggered
        // notification run), the endpoint must still surface the
        // original parent breadcrumb from the older parent-linked run.
        //
        // `list_by_session` returns newest-first, so before the fix the
        // initial `next()` would have picked the parent-less run and
        // returned `null`. After the fix, the walk runs `find_map` over
        // all runs on the session and short-circuits on the first
        // `parent_run_id.is_some()` match.
        let state = test_app_state();
        let parent_agent_id = AgentId::new();
        let sub_agent_id = AgentId::new();

        // Parent session + the run the subagent's `parent_run_id` points at.
        let parent_session = state
            .session_manager
            .get_or_create(parent_agent_id, "parent-chat");
        let parent_session_id = parent_session.id;
        let parent_run = Run::new(parent_session_id, parent_agent_id, "user input".into());
        let parent_run_id = parent_run.run_id;
        state.run_manager.insert_run(parent_run);

        // Subagent session — first run goes through `Run::for_subagent`
        // and carries the breadcrumb.
        let sub_context_id = format!("subagent_{}", uuid::Uuid::new_v4());
        let sub_session = state
            .session_manager
            .get_or_create(sub_agent_id, sub_context_id);
        let sub_session_id = sub_session.id;
        let original_sub_run = Run::for_subagent(
            sub_session_id,
            sub_agent_id,
            "subagent input".into(),
            parent_run_id,
        );
        state.run_manager.insert_run(original_sub_run);

        // ...then a LATER run lands on the same session without a
        // parent breadcrumb. Use a strictly-later `created_at` so the
        // newest-first sort in `list_by_session` puts this run ahead of
        // the original parent-linked one — otherwise the lookup could
        // pass for the wrong reason (the original ordering happens to
        // keep the breadcrumb first).
        let mut parentless_later_run =
            Run::new(sub_session_id, sub_agent_id, "follow-up input".into());
        parentless_later_run.created_at = chrono::Utc::now() + chrono::Duration::seconds(60);
        state.run_manager.insert_run(parentless_later_run);

        // Sanity: the newest-first ordering really does put the
        // parent-less run ahead of the parent-linked one. If this
        // assert ever fires the test below is no longer exercising
        // the path it's named after.
        let ordered = state
            .run_manager
            .list_by_session(sub_session_id, usize::MAX);
        assert!(
            ordered
                .first()
                .map(|r| r.parent_run_id.is_none())
                .unwrap_or(false),
            "test precondition: newest-first should put the parent-less run first; got: {ordered:?}"
        );

        let (status, body) = invoke_get_session_metadata(state, sub_session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert_eq!(
            body["parent_session_id"],
            serde_json::json!(parent_session_id),
            "newer parent-less run must not shadow an older parent-linked breadcrumb; \
             got body = {body}"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_subagent_no_runs_returns_null() {
        // Edge case: subagent session exists but has no runs at all in
        // memory yet (or all runs have been evicted). `list_by_session`
        // returns empty; `parent_session_id` must still be `null`, not
        // missing.
        let state = test_app_state();
        let sub_agent_id = AgentId::new();
        let sub_context_id = format!("subagent_{}", uuid::Uuid::new_v4());
        let sub_session = state
            .session_manager
            .get_or_create(sub_agent_id, sub_context_id);
        let sub_session_id = sub_session.id;

        let (status, body) = invoke_get_session_metadata(state, sub_session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert!(
            body["parent_session_id"].is_null(),
            "expected null parent_session_id for subagent session with no runs; \
             got body = {body}"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_dm_envelope_includes_participants() {
        let state = test_app_state();
        // DM sessions are created under the AgentId::nil() sentinel.
        let session = state
            .session_manager
            .get_or_create(AgentId::nil(), "dm:alice:bob");
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "dm");
        assert_eq!(body["participants"], serde_json::json!(["alice", "bob"]));
        assert!(
            body.get("parent_session_id").is_none(),
            "parent_session_id should be omitted for DM sessions"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_notification_envelope_includes_agent_name() {
        let state = test_app_state();
        let agent_id = AgentId::new();
        let session = state
            .session_manager
            .get_or_create(agent_id, "notifications:my-agent");
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "notification");
        assert_eq!(body["agent_name"], "my-agent");
        assert!(
            body.get("parent_session_id").is_none(),
            "parent_session_id should be omitted for notification sessions"
        );
    }
}
