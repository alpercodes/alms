// SPDX-License-Identifier: Apache-2.0

//! Workspace HTTP API
//!
//! GET  /agents/{id_or_name}/workspace          — read all workspace files
//! PUT  /agents/{id_or_name}/workspace/{file}   — overwrite a workspace file (user-facing)
//! POST /agents/{id_or_name}/workspace/open     — open the workspace dir in the host file explorer (#858)

use crate::agents::resolve_agent;
use crate::api_error;
use crate::server::AppState;
use alms_runtime::{AgentWorkspace, WorkspaceFile};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

/// GET /agents/{id_or_name}/workspace
///
/// Returns all workspace file contents for the given agent.
/// Files that don't exist yet are returned as empty strings.
/// Returns 503 if `ALMS_WORKSPACE_DIR` is not configured.
pub async fn get_workspace(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let Some(ref workspace_dir) = state.workspace_dir else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_CONFIGURED",
            "Workspace directory not configured (set ALMS_WORKSPACE_DIR)",
        )
        .into_response();
    };

    let store = match state.session_manager.store() {
        Some(s) => s,
        None => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "NO_STORE",
                "Store not available",
            )
            .into_response();
        }
    };

    let agent = match resolve_agent(store, &id_or_name) {
        Ok(a) => a,
        Err(resp) => return resp.into_response(),
    };

    let workspace = AgentWorkspace::new(workspace_dir, &agent.name);
    let mut files = serde_json::Map::new();
    for file in WorkspaceFile::all() {
        let content = workspace.read_file(*file).unwrap_or_default();
        files.insert(
            file.filename().to_string(),
            serde_json::Value::String(content),
        );
    }

    Json(serde_json::json!({
        "agent_id": agent.id.0,
        "files": files,
    }))
    .into_response()
}

/// PUT /agents/{id_or_name}/workspace/{file}
///
/// Overwrites a workspace file. `{file}` must be one of:
/// `personality`, `goals`, `memories`, `user` (without the `.md` extension).
///
/// This is the user-facing write path — all four files are writable here.
///
/// Returns 503 if workspace is not configured, 404 for unknown file names.
pub async fn update_workspace_file(
    State(state): State<AppState>,
    Path((id_or_name, file)): Path<(String, String)>,
    Json(body): Json<UpdateWorkspaceFileRequest>,
) -> impl IntoResponse {
    let Some(ref workspace_dir) = state.workspace_dir else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_CONFIGURED",
            "Workspace directory not configured (set ALMS_WORKSPACE_DIR)",
        )
        .into_response();
    };

    let workspace_file = match file.as_str() {
        "personality" => WorkspaceFile::Personality,
        "goals" => WorkspaceFile::Goals,
        "memories" => WorkspaceFile::Memories,
        "user" => WorkspaceFile::User,
        _ => {
            return api_error(
                StatusCode::NOT_FOUND,
                "UNKNOWN_FILE",
                format!(
                    "Unknown file '{}': must be 'personality', 'goals', 'memories', or 'user'",
                    file
                ),
            )
            .into_response();
        }
    };

    let store = match state.session_manager.store() {
        Some(s) => s,
        None => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "NO_STORE",
                "Store not available",
            )
            .into_response();
        }
    };

    let agent = match resolve_agent(store, &id_or_name) {
        Ok(a) => a,
        Err(resp) => return resp.into_response(),
    };

    let workspace = AgentWorkspace::new(workspace_dir, &agent.name);

    // The operator API may write every workspace file, including any the
    // agent itself may not. That exemption is about *permission*, so it is
    // taken by calling the operator entry point — not, as before #1294, by
    // reaching past `AgentWorkspace` for the path and doing a bare
    // `std::fs::write`, which also waived the sidecar lock and left the file
    // truncated for the length of every edit made from the workspace editor.
    //
    // On the blocking pool because the write now waits on that lock with no
    // timeout, the same reasoning `workspace_write` follows (#1280): a second
    // daemon on the same data dir could otherwise wedge a tokio worker.
    let content = body.content;
    let write = tokio::task::spawn_blocking(move || {
        workspace.write_file_as_operator(workspace_file, &content)
    })
    .await;

    match write {
        Ok(Ok(())) => Json(serde_json::json!({
            "ok": true,
            "file": workspace_file.filename(),
        }))
        .into_response(),
        Ok(Err(e)) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "IO_ERROR", e).into_response(),
        Err(e) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "IO_ERROR",
            format!("Workspace write task failed: {}", e),
        )
        .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceFileRequest {
    pub content: String,
}

/// Build the platform-appropriate "open this folder in the host file explorer"
/// command for [`open_workspace`].
///
/// Pulled out as a free function so the routing/auth/agent-resolution path
/// can be unit-tested without ever touching `tokio::process::Command::spawn`.
/// The returned tuple is the launcher binary plus its single argument
/// (the resolved workspace path) so the caller can `Command::new(bin).arg(arg)`.
///
/// Platform mapping:
/// - **Windows**: `explorer.exe <path>` — note that `explorer.exe` exits with
///   status 1 even on success in many setups, so the caller must NOT treat
///   non-zero exit status as a failure on Windows.
/// - **macOS**: `open <path>` — exits 0 on success.
/// - **Linux / other Unix**: `xdg-open <path>` — relies on
///   `xdg-utils` being installed (it is on every mainstream desktop distro).
fn launcher_for_path(path: &std::path::Path) -> (&'static str, std::ffi::OsString) {
    let arg = path.as_os_str().to_os_string();
    if cfg!(target_os = "windows") {
        ("explorer.exe", arg)
    } else if cfg!(target_os = "macos") {
        ("open", arg)
    } else {
        ("xdg-open", arg)
    }
}

/// POST /agents/{id_or_name}/workspace/open
///
/// Spawns the host's native file explorer at the agent's workspace directory
/// (Windows Explorer / Finder / xdg-open). Operator-trust: the gateway is
/// expected to run on the same host as the operator's browser (this is the
/// whole point of the feature — escape the sandbox into the host UI), so the
/// existing bearer-auth gate is the only privilege check here. The endpoint
/// does NOT accept a free-form path from the client — `agent_id` is the only
/// input, the workspace path is resolved server-side via the agent registry
/// and the configured `ALMS_WORKSPACE_DIR`. Path-traversal / injection is
/// closed-by-construction because the agent name has already been validated
/// by [`alms_core::validate_agent_name`] at create time.
///
/// Returns:
/// - 200 `{ ok: true, path: "<resolved>" }` on successful spawn (the launcher
///   process detaches and continues to run after we return — we do NOT wait
///   for the file explorer to close).
/// - 503 `NOT_CONFIGURED` if `ALMS_WORKSPACE_DIR` is unset.
/// - 404 `NOT_FOUND` if the agent doesn't exist.
/// - 500 `WORKSPACE_PATH_MISSING` if the resolved directory doesn't exist on
///   disk (the agent record exists but its workspace dir was never created or
///   has been deleted out from under us).
/// - 500 `LAUNCHER_FAILED` if the launcher binary couldn't be spawned (e.g.
///   `xdg-open` not on PATH on a server-only Linux box).
///
/// Note on Windows: `explorer.exe` famously exits with status 1 even on a
/// successful folder-open, so we do NOT inspect the launcher's exit code —
/// successful spawn (i.e. `Command::spawn` returned `Ok`) is treated as
/// success on every platform. The launcher process is fire-and-forget; we
/// just need to confirm the OS accepted our exec.
///
/// We deliberately do NOT impose a timeout on the spawn. `Command::spawn` is
/// synchronous (fork+execvp / CreateProcess) and returns the child handle
/// immediately; any post-spawn hang lives in the launcher process itself,
/// which is exactly the process we want to outlive this request. If
/// `xdg-open` wedges holding a broken X session open, that's a host desktop
/// problem — bounding the gateway request would not help.
pub async fn open_workspace(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let Some(ref workspace_dir) = state.workspace_dir else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_CONFIGURED",
            "Workspace directory not configured (set ALMS_WORKSPACE_DIR)",
        )
        .into_response();
    };

    let store = match state.session_manager.store() {
        Some(s) => s,
        None => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "NO_STORE",
                "Store not available",
            )
            .into_response();
        }
    };

    let agent = match resolve_agent(store, &id_or_name) {
        Ok(a) => a,
        Err(resp) => return resp.into_response(),
    };

    let workspace = AgentWorkspace::new(workspace_dir, &agent.name);
    let path = workspace.dir();

    if !path.exists() {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WORKSPACE_PATH_MISSING",
            format!(
                "Agent workspace directory does not exist: {}",
                path.display()
            ),
        )
        .into_response();
    }

    // Test short-circuit: integration tests pin the routing + agent-resolution
    // path without spawning a real file explorer (which on a CI box would
    // either fail outright on Linux without `xdg-utils` or pop a window on
    // a developer's screen). When the env var is set we resolve the path,
    // succeed, and skip the actual `Command::spawn`.
    if std::env::var_os("ALMS_TEST_SKIP_LAUNCHER").is_some() {
        return Json(serde_json::json!({
            "ok": true,
            "path": path.display().to_string(),
        }))
        .into_response();
    }

    let (bin, arg) = launcher_for_path(&path);

    // Spawn the launcher and confirm `Command::spawn` accepted it. We do NOT
    // wait for the launcher to exit — `explorer.exe` lingers until the user
    // closes the window, and `xdg-open` can `exec()` into the file manager
    // and hold the descriptor for the lifetime of that process. We just need
    // to know the OS handed us a child handle.
    match tokio::process::Command::new(bin).arg(&arg).spawn() {
        Ok(child) => {
            // Detach the child so the tokio runtime doesn't reap it on drop —
            // the launcher should keep running past our HTTP response. We
            // don't need the pid, but holding the `Child` until the function
            // returns is enough to keep stdio handles closed cleanly.
            tracing::info!(
                target: "alms_gateway::workspace::open",
                agent_id = %agent.id.0,
                agent_name = %agent.name,
                launcher = %bin,
                path = %path.display(),
                pid = child.id(),
                "spawned host file explorer for agent workspace"
            );
            Json(serde_json::json!({
                "ok": true,
                "path": path.display().to_string(),
            }))
            .into_response()
        }
        Err(e) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "LAUNCHER_FAILED",
            format!("Failed to spawn '{}': {}", bin, e),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod open_tests {
    //! Tests for the `POST /agents/{id_or_name}/workspace/open` handler (#858).
    //!
    //! The actual `Command::spawn` is short-circuited by the
    //! `ALMS_TEST_SKIP_LAUNCHER` env var so we exercise the full
    //! routing / auth / agent-resolution / path-existence path without ever
    //! popping a real File Explorer / Finder window on a developer's screen
    //! or failing on a headless CI box.
    //!
    //! Tests that mutate the env var `ALMS_TEST_SKIP_LAUNCHER` rely on
    //! `cargo test` running each `#[tokio::test]` in its own thread but a
    //! single shared process — `serial_test` is not in the workspace, so we
    //! gate the env var check defensively (set on entry, the tests don't
    //! need to clear it because the launcher path is the *only* code that
    //! reads it). Two parallel tests both setting the same value to "1" is
    //! a no-op race, not a correctness hazard.
    use super::*;
    use crate::test_support::TestAppState;
    use alms_core::AgentRecord;
    use axum::http::StatusCode;
    use tempfile::TempDir;

    #[test]
    fn launcher_for_path_picks_platform_appropriate_binary() {
        // Pin the per-platform mapping so a future refactor of the cfg!
        // dispatch is caught here. Each branch is exclusive — only the
        // matching arm runs on a given target.
        let p = std::path::Path::new("/tmp/whatever");
        let (bin, arg) = launcher_for_path(p);

        // The arg is always the path verbatim — no quoting, no escaping
        // (Command::arg handles that).
        assert_eq!(arg, p.as_os_str());

        if cfg!(target_os = "windows") {
            assert_eq!(bin, "explorer.exe");
        } else if cfg!(target_os = "macos") {
            assert_eq!(bin, "open");
        } else {
            assert_eq!(bin, "xdg-open");
        }
    }

    /// Build an AppState backed by an in-memory SQLite store and a temp
    /// `workspace_dir`. The TempDir is returned so the caller keeps it alive
    /// for the duration of the test (its Drop removes the directory).
    pub(super) fn open_test_app_state() -> (TempDir, crate::server::AppState) {
        let tmp = TempDir::new().expect("tempdir for workspace");
        let state = TestAppState::new()
            .in_memory_sqlite()
            .workspace_dir(tmp.path())
            .build();
        (tmp, state)
    }

    pub(super) fn new_agent(name: &str) -> AgentRecord {
        AgentRecord::for_test(name)
    }

    /// Extract `(StatusCode, JSON body)` from an `IntoResponse` produced by
    /// `open_workspace`. We can't use the handler's return type directly
    /// because it's `impl IntoResponse`, so we route through the axum
    /// machinery.
    async fn invoke_open(
        state: crate::server::AppState,
        id_or_name: &str,
    ) -> (StatusCode, serde_json::Value) {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;
        let resp = open_workspace(
            axum::extract::State(state),
            axum::extract::Path(id_or_name.to_string()),
        )
        .await
        .into_response();
        let status = resp.status();
        let body_bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();
        (status, body)
    }

    #[tokio::test]
    async fn open_workspace_returns_404_for_unknown_agent() {
        // SAFETY: setting an env var in a multi-threaded test is unsafe per
        // Rust 1.78+. The launcher-skip env var is a one-way "force the
        // short-circuit branch" knob — concurrent setters all write the
        // same value, and no one reads it after this test returns.
        unsafe {
            std::env::set_var("ALMS_TEST_SKIP_LAUNCHER", "1");
        }
        let (_tmp, state) = open_test_app_state();
        let (status, body) = invoke_open(state, "does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn open_workspace_returns_500_when_path_missing() {
        // Agent record exists but the workspace directory has never been
        // created (we never called `init_workspace_files` in this test).
        // The handler must report `WORKSPACE_PATH_MISSING` cleanly rather
        // than hand a missing path to the launcher.
        unsafe {
            std::env::set_var("ALMS_TEST_SKIP_LAUNCHER", "1");
        }
        let (_tmp, state) = open_test_app_state();
        let store = state.session_manager.store().expect("sqlite store");
        let agent = new_agent("missing-ws");
        store.create_agent(&agent).unwrap();

        let (status, body) = invoke_open(state, "missing-ws").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "WORKSPACE_PATH_MISSING");
    }

    #[tokio::test]
    async fn open_workspace_succeeds_for_existing_agent_with_workspace_dir() {
        // Happy path with launcher short-circuited. Verifies routing,
        // auth/agent-resolution, and the response shape `{ ok, path }`.
        unsafe {
            std::env::set_var("ALMS_TEST_SKIP_LAUNCHER", "1");
        }
        let (tmp, state) = open_test_app_state();
        let store = state.session_manager.store().expect("sqlite store");
        let agent = new_agent("happy-agent");
        store.create_agent(&agent).unwrap();

        // Manually create the workspace dir so the path-exists check passes.
        // (`alms agent create` does this via `init_workspace_files` on the
        // real CRUD path; this test bypasses CRUD by inserting into the
        // store directly so it exercises the open handler in isolation.)
        let agent_ws_dir = tmp.path().join("happy-agent");
        std::fs::create_dir_all(&agent_ws_dir).unwrap();

        let (status, body) = invoke_open(state, "happy-agent").await;
        assert_eq!(status, StatusCode::OK, "body = {body}");
        assert_eq!(body["ok"], serde_json::json!(true));
        // `path.display()` round-trips the canonical OS path; we just
        // assert the agent name is in the suffix rather than pinning the
        // full string (Windows / Unix differ on separators and tempdir
        // prefixes).
        let path_str = body["path"].as_str().expect("path field");
        assert!(
            path_str.ends_with("happy-agent")
                || path_str.replace('\\', "/").ends_with("happy-agent"),
            "path '{path_str}' should end with the agent's workspace dir name"
        );
    }

    #[tokio::test]
    async fn open_workspace_returns_503_when_workspace_dir_unconfigured() {
        // Without `workspace_dir` set on the gateway config, the handler
        // must short-circuit with NOT_CONFIGURED before hitting the agent
        // store at all.
        unsafe {
            std::env::set_var("ALMS_TEST_SKIP_LAUNCHER", "1");
        }
        let gateway_config = crate::gateway::GatewayConfig {
            db_path: Some(":memory:".to_string()),
            workspace_dir: None, // intentionally unset
            ..crate::gateway::GatewayConfig::default()
        };
        let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
        let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
        let shutdown_token = tokio_util::sync::CancellationToken::new();
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        let (trigger_tx, _trigger_rx) = tokio::sync::mpsc::channel(8);
        let (dm_event_tx, _dm_event_rx) = tokio::sync::mpsc::channel(8);
        let state = crate::server::AppState::new(
            gateway,
            scheduler,
            shutdown_token,
            completion_tx,
            trigger_tx,
            dm_event_tx,
        )
        .unwrap();
        let (status, body) = invoke_open(state, "anything").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "NOT_CONFIGURED");
    }
}

#[cfg(test)]
mod put_tests {
    //! Tests for `PUT /agents/{id_or_name}/workspace/{file}` (#1294).

    use super::open_tests::{new_agent, open_test_app_state};
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    async fn invoke_put(
        state: crate::server::AppState,
        id_or_name: &str,
        file: &str,
        content: &str,
    ) -> (StatusCode, serde_json::Value) {
        let resp = update_workspace_file(
            axum::extract::State(state),
            axum::extract::Path((id_or_name.to_string(), file.to_string())),
            Json(UpdateWorkspaceFileRequest {
                content: content.to_string(),
            }),
        )
        .await
        .into_response();
        let status = resp.status();
        let body_bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();
        (status, body)
    }

    /// The operator write goes through `AgentWorkspace`'s locked replacement
    /// rather than around it. The sidecar lock file is the evidence: nothing
    /// else in the workspace creates it, so its absence would mean the
    /// handler had gone back to writing the path itself — which is what
    /// left a window in which a run building its context read a truncated or
    /// empty `memories.md`, silently.
    #[tokio::test]
    async fn a_put_writes_through_the_locked_replacement() {
        let (tmp, state) = open_test_app_state();
        let store = state.session_manager.store().expect("sqlite store");
        store.create_agent(&new_agent("scribe")).unwrap();

        let (status, body) = invoke_put(state, "scribe", "memories", "- learned a thing").await;
        assert_eq!(status, StatusCode::OK, "body = {body}");
        assert_eq!(body["ok"], serde_json::json!(true));

        let dir = tmp.path().join("scribe");
        assert_eq!(
            std::fs::read_to_string(dir.join("memories.md")).unwrap(),
            "- learned a thing"
        );
        assert!(
            dir.join(".memories.md.lock").exists(),
            "the PUT must have taken the workspace file's sidecar lock; \
             files on disk: {:?}",
            std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );
    }

    /// The operator may write `personality.md`, which is the reason this
    /// handler was allowed to bypass `AgentWorkspace` in the first place.
    /// Routing it through `write_file_as_operator` keeps the exemption.
    #[tokio::test]
    async fn a_put_may_still_write_personality() {
        let (tmp, state) = open_test_app_state();
        let store = state.session_manager.store().expect("sqlite store");
        store.create_agent(&new_agent("stylist")).unwrap();

        let (status, body) = invoke_put(state, "stylist", "personality", "Terse. Precise.").await;
        assert_eq!(status, StatusCode::OK, "body = {body}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("stylist").join("personality.md")).unwrap(),
            "Terse. Precise."
        );
    }

    /// A replacement leaves no staging file behind for the workspace editor
    /// (or `GET /workspace`) to trip over.
    #[tokio::test]
    async fn a_put_leaves_no_staging_file_behind() {
        let (tmp, state) = open_test_app_state();
        let store = state.session_manager.store().expect("sqlite store");
        store.create_agent(&new_agent("tidy")).unwrap();

        let (status, _) = invoke_put(state, "tidy", "goals", "- ship it").await;
        assert_eq!(status, StatusCode::OK);

        let strays: Vec<_> = std::fs::read_dir(tmp.path().join("tidy"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "staging files left behind: {strays:?}");
    }
}
