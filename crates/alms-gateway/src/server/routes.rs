// SPDX-License-Identifier: Apache-2.0

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
use crate::operations::get_operational_metrics;
use crate::runs::{
    cancel_dm, cancel_run, cancel_subagent, classify_session_type, create_run, get_run_reasoning,
    get_run_status, get_run_text, get_run_tool_calls, is_internal_context_id, list_runs,
    stream_run_events,
};
use crate::settings::{get_settings, patch_settings};
use crate::workspace::{get_workspace, open_workspace, update_workspace_file};
use alms_core::{
    AgentId, SessionId, SubagentOwner, dm_participants, parse_subagent_context,
    parse_subagent_parent,
};
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
/// The Vite production build under `static/ui-dist/` is checked in and baked
/// into the binary, so the server works from any working directory without a
/// runtime Node.js dependency.
#[derive(Embed)]
#[folder = "static/ui-dist/"]
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
        // Session-keyed subagent cancel: the UI knows a subagent's session
        // id (chips / drill-down view) but not its run id, and subagent runs
        // have no cancel token in the RunManager — see `cancel_subagent`.
        .route(
            "/sessions/{session_id}/subagent/cancel",
            post(cancel_subagent),
        )
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
        // Operational counters and live SSE subscriber gauges.
        .route("/operations/metrics", get(get_operational_metrics))
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
        // Global cross-agent session-activity SSE feed (#1211). Unlike the
        // per-agent `/agents/{id}/events` feed, this is not parameterised by
        // agent, so it can't be derived from SSE_ENDPOINT_SEGMENTS (which
        // builds `/{seg}/{id}/events` paths). Its query-string auth is
        // whitelisted directly in `auth::is_sse_endpoint`.
        .route(
            "/events/session-activity",
            get(crate::runs::stream_session_activity),
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
/// subagent) — these are implementation details not shown in the
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
/// Scheduled-job sessions (`job_{job_id}` context IDs) are likewise
/// **unconditionally included** (#1197). Each scheduled job runs on a
/// stable per-job session that accumulates history across firings —
/// surfacing it lets the operator inspect what the job actually did
/// instead of only seeing the completion marker posted to a user-facing
/// session. Job sessions render in the sidebar's collapsed "Jobs" group
/// and participate in the `agent_id` filter the same way notifications
/// do. Note they remain "internal" for notification-targeting purposes:
/// `find_user_facing_session` still skips them.
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
/// - `agent_name`: the session's owning agent, recovered from the
///   `context_id` — the agent for `notifications:{agent}`, the
///   subagent for `subagent_{parent}_{name}` (#1277). Absent when the
///   context carries no recoverable owner.
/// - `parent_agent_id`: for subagent sessions only, the agent that
///   invoked the subagent, recovered from the `context_id` (#1278).
///   Absent when the context carries no readable parent.
///
/// Named subagent sessions are listed (#1278) — they are filed under the
/// invoked agent's registry id, so they belong in that agent's timeline.
/// Ephemeral subagent and episodic sessions are not; see the `"subagent"`
/// arm below for why the split falls there.
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
            "job" => {
                // Scheduled-job sessions are always shown in the
                // sidebar's collapsed "Jobs" group (#1197). Mirrors the
                // notification arm exactly: no opt-in toggle, same
                // `agent_id` filter semantics (job sessions are stored
                // under the owning agent's real id).
                if let Some(agent_id) = params.agent_id
                    && session.agent_id != agent_id
                {
                    continue;
                }
            }
            "subagent" => {
                // #1278: a NAMED subagent session is filed under the invoked
                // agent's registry id, so it belongs in that agent's own
                // timeline — which is Alper's actual complaint ("the invoked
                // agent's work is not in its own timeline"). Same `agent_id`
                // filter semantics as the notification / job arms above.
                //
                // EPHEMERAL subagent sessions stay excluded, and not as an
                // arbitrary cut-off: an ephemeral subagent has no registry
                // agent, so there is no timeline for it to appear in — its
                // `agent_id` is a fresh `AgentId::new()` that matches no
                // agent filter and no sidebar group. Listing them would also
                // put one permanent row in the sidebar per one-shot
                // `invoke_agent` call. They remain reachable the way they
                // already were: by session id, via `GET /session/{id}` and
                // the parent's `invoke_agent` result / `subagent_started`.
                //
                // A context this binary cannot parse is excluded too — that
                // is the pre-#1278 behaviour, and an unreadable owner is
                // exactly the case #1277 decided must not be guessed at.
                if !matches!(
                    parse_subagent_context(&session.context_id),
                    Some(SubagentOwner::Named(_))
                ) {
                    continue;
                }
                if let Some(agent_id) = params.agent_id
                    && session.agent_id != agent_id
                {
                    continue;
                }
            }
            _ if is_internal => {
                // Other internal sessions (episodic): always excluded
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
        // it. Named subagent rows DO reach the sidebar since #1278, but the
        // attribution they render is the invoking agent, which comes from
        // `parent_agent_id` — parsed straight out of the `context_id` for
        // free. The parent SESSION breadcrumb is a different (and much more
        // expensive) question, and `GET /session/{session_id}` (Iris's spec
        // for #1065) stays the one-shot probe path that pays for it.
        let obj = enrich_session_json(&state, &session, session_type, false);

        result.push(obj);
    }

    Json(serde_json::json!({ "sessions": result }))
}

/// Display label for an ephemeral (unnamed) subagent session (#1277).
///
/// An ephemeral subagent has no name — only a task id, which must never be
/// rendered as if it were one. The parentheses are load-bearing: agent names
/// are restricted to ASCII alphanumerics and hyphens (`validate_agent_name`),
/// so this string cannot be confused for a real agent, and no agent can ever
/// be registered under it. Uppercase becoming admissible in #2 left that
/// untouched — parentheses are still outside the class.
const EPHEMERAL_SUBAGENT_LABEL: &str = "(subagent)";

/// Build the enriched JSON object for a single session, used both by the
/// `GET /sessions` list endpoint and the `GET /session/{session_id}`
/// single-session lookup.
///
/// Adds the same fields list_sessions has always exposed
/// (`session_type`, `has_active_run`, `participants` for DM,
/// `agent_name` for notification and subagent sessions), plus — when
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
        "subagent" => {
            // #1277: the frontend's agents-list lookup on `agent_id` could
            // not name a subagent session, so its label fell back to
            // whichever agent happened to be active — the PARENT. The
            // `context_id` carries the identity, so recover it here,
            // exactly as the notification arm above does.
            //
            // #1278 made `agent_id` resolve too, for the named case: those
            // sessions are now filed under the invoked agent's registry id.
            // This arm stays authoritative anyway, and the two agree by
            // construction — the registry id is looked up BY this same name
            // (`SessionManager::named_subagent_key`). It has to stay because
            // it is still the only answer for the two cases `agent_id`
            // cannot cover: an ephemeral subagent (fresh `AgentId::new()`),
            // and a named one whose agent was never registered.
            //
            // Left unset for an unparseable context: an absent `agent_name`
            // renders as no name, which is the correct answer for "unknown
            // owner". Do not substitute a placeholder here — the ephemeral
            // marker below is a statement that there IS no name, not a
            // stand-in for one we failed to read.
            match parse_subagent_context(&session.context_id) {
                Some(SubagentOwner::Named(name)) => {
                    obj["agent_name"] = serde_json::json!(name);
                }
                Some(SubagentOwner::Ephemeral) => {
                    obj["agent_name"] = serde_json::json!(EPHEMERAL_SUBAGENT_LABEL);
                }
                None => {}
            }

            // #1278: who ASKED for the work. Once the row lives in the
            // invoked agent's own timeline, its own name is what the
            // surrounding group header already says — the invoking parent is
            // the part that distinguishes two rows from each other. Read
            // straight out of the `context_id`, so it costs nothing and is
            // available for ephemeral rows too (`GET /session/{id}` serves
            // them even though the listing does not).
            //
            // Emitted as an id, not a name: resolving it would need a
            // registry lookup per row, and the client already holds the
            // agents list it would be resolved against. Omitted — not
            // null — when the context carries no readable parent, matching
            // `agent_name` above.
            if let Some(parent) = parse_subagent_parent(&session.context_id) {
                obj["parent_agent_id"] = serde_json::json!(parent);
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
/// `agent_name` for the DM / notification / subagent cases) and additionally
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
    let _admission_guard =
        crate::runs::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id)
            .await;

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
///
/// Each record carries **two** identifiers, and clients must not confuse them
/// (#5): `tool_id` is the LLM provider's call id, which pairs a call record
/// with its result record and appears in no SSE event; `tool_invocation_id` is
/// ALMS's own correlator, which is what `tool_start` / `tool_end` /
/// `subagent_started` / `subagent_completed` carry and what the frontend keys
/// row identity on. A row reconstructed from this endpoint must use the
/// latter, or it cannot be matched against the live stream at all — which is
/// the bug the field was added to close. It is `Option` because rows written
/// before #5 have none; the frontend falls back to `tool_id` there, which is
/// the pre-#5 behaviour.
///
/// No serialization work is needed here for either field: `SessionToolCall`
/// flattens `ToolCallRecord`, so the response shape follows the struct.
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

    async fn response_body(response: axum::response::Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("embedded response body")
            .to_vec()
    }

    #[tokio::test]
    async fn embedded_ui_serves_index_and_every_bundled_asset_with_mime_types() {
        let index = serve_embedded_index().await;
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(
            index.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );
        assert_eq!(
            index.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let index_html = String::from_utf8(response_body(index).await).unwrap();

        let mut referenced_assets = 0;
        for prefix in ["src=\"/ui/", "href=\"/ui/"] {
            for tail in index_html.split(prefix).skip(1) {
                let path = tail.split('"').next().unwrap();
                assert!(
                    UiAssets::get(path).is_some(),
                    "index references missing embedded asset: {path}"
                );
                referenced_assets += 1;
            }
        }
        assert!(
            referenced_assets >= 2,
            "index should reference built JS and CSS assets"
        );

        for asset in UiAssets::iter() {
            let response = serve_embedded_file(asset.as_ref());
            assert_eq!(response.status(), StatusCode::OK, "failed asset: {asset}");
            assert!(
                response.headers().get(header::CONTENT_TYPE).is_some(),
                "asset has no MIME type: {asset}"
            );
            assert!(
                !response_body(response).await.is_empty(),
                "empty asset: {asset}"
            );
        }
    }

    #[tokio::test]
    async fn embedded_ui_spa_fallback_and_missing_asset_behavior_are_distinct() {
        let fallback = serve_embedded_file("sessions/deep-link");
        assert_eq!(fallback.status(), StatusCode::OK);
        assert_eq!(
            fallback.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );
        assert!(!response_body(fallback).await.is_empty());

        let missing = serve_embedded_file("assets/not-present.js");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        for response in [
            serve_embedded_index().await,
            serve_embedded_asset(Path(String::new())).await,
        ] {
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

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
        let _ = state.run_manager.insert_run(parent_run);

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
        let _ = state.run_manager.insert_run(sub_run);

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
        let _ = state.run_manager.insert_run(sub_run);

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
        let _ = state.run_manager.insert_run(parent_run);

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
        let _ = state.run_manager.insert_run(original_sub_run);

        // ...then a LATER run lands on the same session without a
        // parent breadcrumb. Use a strictly-later `created_at` so the
        // newest-first sort in `list_by_session` puts this run ahead of
        // the original parent-linked one — otherwise the lookup could
        // pass for the wrong reason (the original ordering happens to
        // keep the breadcrumb first).
        let mut parentless_later_run =
            Run::new(sub_session_id, sub_agent_id, "follow-up input".into());
        parentless_later_run.created_at = chrono::Utc::now() + chrono::Duration::seconds(60);
        let _ = state.run_manager.insert_run(parentless_later_run);

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

    // -----------------------------------------------------------------
    // Subagent envelope owner enrichment (#1277)
    // -----------------------------------------------------------------
    //
    // A subagent session is stored under a DERIVED agent id, never the
    // subagent's registry id, so the frontend cannot resolve its owner
    // from the agents list — it fell back to the active agent and put the
    // PARENT's name on the subagent's bubbles. `agent_name` is the only
    // channel that carries the real owner across, so these rows assert the
    // field's VALUE, not merely its presence.

    #[tokio::test]
    async fn get_session_metadata_named_subagent_envelope_carries_the_subagent_name() {
        let state = test_app_state();
        let parent_agent_id = AgentId::new();
        // Stored under the derived id (`AgentId::deterministic`), as
        // `derive_subagent_identity` does — deliberately NOT a registered
        // agent id, which is what defeats the client-side lookup.
        let derived_id = AgentId::deterministic(parent_agent_id, "reviewer");
        let session = state.session_manager.get_or_create(
            derived_id,
            format!("subagent_{}_reviewer", parent_agent_id.0),
        );
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert_eq!(body["agent_name"], "reviewer");
        assert_ne!(
            body["agent_name"],
            serde_json::json!(parent_agent_id.0.to_string()),
            "the envelope must name the subagent, never the parent"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_ephemeral_subagent_envelope_carries_a_non_name_marker() {
        let state = test_app_state();
        let parent_agent_id = AgentId::new();
        let task_id = uuid::Uuid::new_v4();
        let session = state.session_manager.get_or_create(
            AgentId::new(),
            format!("subagent_{}_{}", parent_agent_id.0, task_id),
        );
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        // Asserted as a LITERAL, not against `EPHEMERAL_SUBAGENT_LABEL`.
        // Comparing the constant to itself is tautological: it moves on both
        // sides of the assertion, so changing the marker's value would leave
        // this row (and the `is_err()` row below, since most illegal strings
        // stay illegal) passing, while `frontend/subagent-session-label.test.ts`
        // never sees the Rust constant at all. Spelling the value out here is
        // what makes the label a wire contract a change has to come through,
        // and gives the frontend's "Mirrors `EPHEMERAL_SUBAGENT_LABEL`"
        // comment something that actually breaks when the mirror drifts.
        assert_eq!(body["agent_name"], "(subagent)");
        // The failure this guards is the task id being rendered as a name.
        assert_ne!(
            body["agent_name"],
            serde_json::json!(task_id.to_string()),
            "an ephemeral subagent's task id must never be surfaced as its name"
        );
        assert!(
            alms_core::validate_agent_name(EPHEMERAL_SUBAGENT_LABEL).is_err(),
            "the ephemeral marker must be un-registrable as an agent name so it \
             can never be mistaken for one"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_unreadable_subagent_envelope_omits_agent_name() {
        let state = test_app_state();
        // Legacy pre-#1185 ephemeral shape: no parent segment, so the owner
        // is not recoverable. Omitting the field makes the UI render no
        // name — the required degradation is blank, not a guess.
        let session = state
            .session_manager
            .get_or_create(AgentId::new(), format!("subagent_{}", uuid::Uuid::new_v4()));
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert!(
            body.get("agent_name").is_none(),
            "an unreadable subagent context must carry no agent_name; got {body}"
        );
    }

    // -----------------------------------------------------------------
    // GET /sessions — job-session inclusion (#1197)
    // -----------------------------------------------------------------
    //
    // Scheduled jobs run on stable per-job `job_{job_id}` sessions that
    // used to be filtered out of the sidebar list entirely (the
    // `_ if is_internal` arm). #1197 surfaces them like notification
    // sessions: unconditionally included, subject to the `agent_id`
    // filter. Subagent / episodic sessions must stay excluded.

    /// Drive the `list_sessions` handler and return the parsed
    /// `sessions` array from the JSON body.
    async fn invoke_list_sessions(
        state: AppState,
        agent_id: Option<AgentId>,
        include_dms: Option<bool>,
    ) -> Vec<serde_json::Value> {
        use axum::body::to_bytes;
        let resp = list_sessions(
            axum::extract::State(state),
            axum::extract::Query(ListSessionsQuery {
                agent_id,
                include_dms,
            }),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("body to bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON body");
        body["sessions"].as_array().expect("sessions array").clone()
    }

    #[tokio::test]
    async fn list_sessions_includes_job_sessions() {
        let state = test_app_state();
        let agent_id = AgentId::new();
        state.session_manager.get_or_create(agent_id, "web-chat-1");
        let job_context = format!("job_{}", uuid::Uuid::new_v4());
        state.session_manager.get_or_create(agent_id, &job_context);

        let sessions = invoke_list_sessions(state, None, None).await;

        let job_row = sessions
            .iter()
            .find(|s| s["context_id"] == serde_json::json!(job_context))
            .unwrap_or_else(|| panic!("job session missing from GET /sessions: {sessions:?}"));
        assert_eq!(job_row["session_type"], "job");
        assert_eq!(job_row["agent_id"], serde_json::json!(agent_id));
        // The chat session is still present alongside it.
        assert!(
            sessions.iter().any(|s| s["context_id"] == "web-chat-1"),
            "chat session missing from GET /sessions: {sessions:?}"
        );
    }

    #[tokio::test]
    async fn list_sessions_applies_agent_id_filter_to_job_sessions() {
        // Mirror of the notification-arm contract: a per-agent fetch only
        // returns the job sessions owned by that agent; the cross-agent
        // fetch (agent_id unset) returns every job session.
        let state = test_app_state();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        let job_a = format!("job_{}", uuid::Uuid::new_v4());
        let job_b = format!("job_{}", uuid::Uuid::new_v4());
        state.session_manager.get_or_create(agent_a, &job_a);
        state.session_manager.get_or_create(agent_b, &job_b);

        let for_a = invoke_list_sessions(state.clone(), Some(agent_a), None).await;
        assert!(
            for_a
                .iter()
                .any(|s| s["context_id"] == serde_json::json!(job_a)),
            "agent A's job session missing from per-agent fetch: {for_a:?}"
        );
        assert!(
            !for_a
                .iter()
                .any(|s| s["context_id"] == serde_json::json!(job_b)),
            "agent B's job session leaked into agent A's fetch: {for_a:?}"
        );

        let cross = invoke_list_sessions(state, None, None).await;
        for ctx in [&job_a, &job_b] {
            assert!(
                cross
                    .iter()
                    .any(|s| s["context_id"] == serde_json::json!(ctx)),
                "job session {ctx} missing from cross-agent fetch: {cross:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Named subagent sessions in the invoked agent's timeline (#1278)
    // -----------------------------------------------------------------

    /// File a session exactly as post-#1278 dispatch does for a named
    /// subagent: under the INVOKED agent's registry id, on a context that
    /// still names the invoking parent.
    fn seed_named_subagent(
        state: &AppState,
        invoked_agent_id: AgentId,
        parent_agent_id: AgentId,
        name: &str,
    ) -> String {
        let context_id = alms_core::named_subagent_context_id(parent_agent_id, name);
        state
            .session_manager
            .get_or_create(invoked_agent_id, &context_id);
        context_id
    }

    #[tokio::test]
    async fn list_sessions_surfaces_a_named_subagent_session_in_its_own_timeline() {
        let state = test_app_state();
        let reviewer = AgentId::new();
        let parent = AgentId::new();
        let context_id = seed_named_subagent(&state, reviewer, parent, "reviewer");

        let sessions = invoke_list_sessions(state, None, None).await;

        let row = sessions
            .iter()
            .find(|s| s["context_id"] == serde_json::json!(context_id))
            .unwrap_or_else(|| panic!("named subagent session missing: {sessions:?}"));
        assert_eq!(row["session_type"], "subagent");
        assert_eq!(
            row["agent_id"],
            serde_json::json!(reviewer),
            "the row must group under the agent that did the work"
        );
        assert_eq!(row["agent_name"], "reviewer");
        assert_eq!(
            row["parent_agent_id"],
            serde_json::json!(parent),
            "the row must carry who asked for the work"
        );
    }

    #[tokio::test]
    async fn list_sessions_scopes_a_named_subagent_row_to_the_invoked_agent() {
        // The `agent_id` filter is what makes this a *timeline* rather than
        // a global list: reviewer's per-agent fetch returns the row, and
        // the parent's does not — the parent's own chat sessions are a
        // different surface, and its `invoke_agent` breadcrumbs live on the
        // run it started.
        let state = test_app_state();
        let reviewer = AgentId::new();
        let parent = AgentId::new();
        let context_id = seed_named_subagent(&state, reviewer, parent, "reviewer");

        let for_reviewer = invoke_list_sessions(state.clone(), Some(reviewer), None).await;
        assert!(
            for_reviewer
                .iter()
                .any(|s| s["context_id"] == serde_json::json!(context_id)),
            "invoked agent's own fetch is missing its subagent session: {for_reviewer:?}"
        );

        let for_parent = invoke_list_sessions(state, Some(parent), None).await;
        assert!(
            !for_parent
                .iter()
                .any(|s| s["context_id"] == serde_json::json!(context_id)),
            "the subagent row leaked into the invoking parent's fetch: {for_parent:?}"
        );
    }

    #[tokio::test]
    async fn list_sessions_shows_a_self_invoked_subagent_row_to_its_own_parent() {
        // The exception to the rule above, and the reason it is stated as
        // "scoped to the invoked agent" rather than "hidden from the
        // parent". `invoke_agent { name: "atlas" }` from atlas is not
        // forbidden — self-invoke guards are out of scope for #1278 — and
        // it yields `subagent_{atlas}_atlas` filed under atlas. Here the
        // invoked agent and the invoking parent are the same agent, so the
        // row correctly appears in "the parent's" fetch. Pinned so a future
        // reader does not read the sibling test's message as a rule and
        // "fix" this into a leak.
        let state = test_app_state();
        let atlas = AgentId::new();
        let context_id = seed_named_subagent(&state, atlas, atlas, "atlas");

        let rows = invoke_list_sessions(state, Some(atlas), None).await;
        let row = rows
            .iter()
            .find(|s| s["context_id"] == serde_json::json!(context_id))
            .unwrap_or_else(|| panic!("self-invoked subagent row missing: {rows:?}"));
        assert_eq!(
            row["parent_agent_id"],
            serde_json::json!(atlas),
            "the parent recovered from the context is the agent itself"
        );
        assert_eq!(row["agent_id"], serde_json::json!(atlas));
    }

    #[tokio::test]
    async fn list_sessions_separates_two_parents_rows_for_one_invoked_agent() {
        let state = test_app_state();
        let reviewer = AgentId::new();
        let parent_a = AgentId::new();
        let parent_b = AgentId::new();
        let ctx_a = seed_named_subagent(&state, reviewer, parent_a, "reviewer");
        let ctx_b = seed_named_subagent(&state, reviewer, parent_b, "reviewer");

        let rows = invoke_list_sessions(state, Some(reviewer), None).await;

        let find = |ctx: &String| {
            rows.iter()
                .find(|s| s["context_id"] == serde_json::json!(ctx))
                .unwrap_or_else(|| panic!("row for {ctx} missing: {rows:?}"))
                .clone()
        };
        // Both live in reviewer's timeline, and the ONLY thing telling
        // them apart in the sidebar is the invoking parent.
        assert_eq!(find(&ctx_a)["parent_agent_id"], serde_json::json!(parent_a));
        assert_eq!(find(&ctx_b)["parent_agent_id"], serde_json::json!(parent_b));
    }

    #[tokio::test]
    async fn list_sessions_still_excludes_episodic_and_nameless_subagent_sessions() {
        // The #1278 carve-out is for NAMED subagent sessions only. An
        // ephemeral one has no registry agent, so there is no timeline for
        // it to appear in — and listing them would add a permanent sidebar
        // row per one-shot invoke_agent call. Episodic sessions and
        // contexts this binary cannot parse stay hidden as before.
        let state = test_app_state();
        let agent_id = AgentId::new();
        let parent = AgentId::new();
        let ephemeral = format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4());
        let legacy = format!("subagent_{}", uuid::Uuid::new_v4());
        let unreadable = format!("subagent_{}_Not_A_Name", parent.0);
        for ctx in [&ephemeral, &legacy, &unreadable] {
            state.session_manager.get_or_create(agent_id, ctx);
        }
        state
            .session_manager
            .get_or_create(agent_id, "episodic:some-summary");

        let sessions = invoke_list_sessions(state, None, Some(true)).await;

        assert!(
            !sessions
                .iter()
                .any(|s| s["session_type"] == "subagent" || s["session_type"] == "episodic"),
            "internal subagent/episodic sessions leaked into GET /sessions: {sessions:?}"
        );
    }

    #[tokio::test]
    async fn get_session_metadata_subagent_envelope_carries_the_invoking_parent() {
        // `GET /session/{id}` serves the sessions the listing does not, so
        // the parent attribution has to be there for ephemeral rows too.
        let state = test_app_state();
        let parent = AgentId::new();
        let named = state.session_manager.get_or_create(
            AgentId::new(),
            alms_core::named_subagent_context_id(parent, "reviewer"),
        );
        let ephemeral = state.session_manager.get_or_create(
            AgentId::new(),
            format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4()),
        );

        for session_id in [named.id, ephemeral.id] {
            let (status, body) = invoke_get_session_metadata(state.clone(), session_id).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["session_type"], "subagent");
            assert_eq!(body["parent_agent_id"], serde_json::json!(parent));
        }
    }

    #[tokio::test]
    async fn get_session_metadata_omits_parent_agent_id_when_the_context_is_unreadable() {
        // Same rule `agent_name` follows: an unreadable owner is reported
        // as absent, never guessed at (#1277).
        let state = test_app_state();
        let session = state
            .session_manager
            .get_or_create(AgentId::new(), format!("subagent_{}", uuid::Uuid::new_v4()));
        let session_id = session.id;

        let (status, body) = invoke_get_session_metadata(state, session_id).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_type"], "subagent");
        assert!(
            body.get("parent_agent_id").is_none(),
            "a legacy context carries no readable parent; got {body}"
        );
        assert!(body.get("agent_name").is_none());
    }
}
