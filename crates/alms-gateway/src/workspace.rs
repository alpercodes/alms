//! Workspace HTTP API
//!
//! GET  /agents/{id_or_name}/workspace          — read all workspace files
//! PUT  /agents/{id_or_name}/workspace/{file}   — overwrite a workspace file (user-facing)

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

    // Write directly using the filesystem path — user API allows all files including personality.
    if let Err(e) = workspace.ensure_dir() {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "IO_ERROR", e).into_response();
    }

    let path = workspace.dir().join(workspace_file.filename());
    match std::fs::write(&path, &body.content) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "file": workspace_file.filename(),
        }))
        .into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "IO_ERROR", e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceFileRequest {
    pub content: String,
}
