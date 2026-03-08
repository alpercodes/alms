//! Workspace HTTP API
//!
//! GET  /agents/{agent_id}/workspace          — read all workspace files
//! PUT  /agents/{agent_id}/workspace/{file}   — overwrite a workspace file (user-facing)

use crate::server::AppState;
use alms_core::AgentId;
use alms_runtime::{AgentWorkspace, WorkspaceFile};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

/// GET /agents/{agent_id}/workspace
///
/// Returns all workspace file contents for the given agent.
/// Files that don't exist yet are returned as empty strings.
/// Returns 503 if `ALMS_WORKSPACE_DIR` is not configured.
pub async fn get_workspace(
    State(state): State<AppState>,
    Path(agent_id): Path<AgentId>,
) -> impl IntoResponse {
    let Some(ref workspace_dir) = state.workspace_dir else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": { "code": "NOT_CONFIGURED", "message": "Workspace directory not configured (set ALMS_WORKSPACE_DIR)" }
            })),
        )
            .into_response();
    };

    let workspace = AgentWorkspace::new(workspace_dir, agent_id);
    let mut files = serde_json::Map::new();
    for file in WorkspaceFile::all() {
        let content = workspace.read_file(*file).unwrap_or_default();
        files.insert(
            file.filename().to_string(),
            serde_json::Value::String(content),
        );
    }

    Json(serde_json::json!({
        "agent_id": agent_id.0,
        "files": files,
    }))
    .into_response()
}

/// PUT /agents/{agent_id}/workspace/{file}
///
/// Overwrites a workspace file. `{file}` must be one of:
/// `personality`, `goals`, `memories` (without the `.md` extension).
///
/// This is the user-facing write path — all three files are writable here,
/// including `personality.md` (which the agent itself cannot overwrite).
///
/// Returns 503 if workspace is not configured, 404 for unknown file names.
pub async fn update_workspace_file(
    State(state): State<AppState>,
    Path((agent_id, file)): Path<(AgentId, String)>,
    Json(body): Json<UpdateWorkspaceFileRequest>,
) -> impl IntoResponse {
    let Some(ref workspace_dir) = state.workspace_dir else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": { "code": "NOT_CONFIGURED", "message": "Workspace directory not configured (set ALMS_WORKSPACE_DIR)" }
            })),
        )
            .into_response();
    };

    let workspace_file = match file.as_str() {
        "personality" => WorkspaceFile::Personality,
        "goals" => WorkspaceFile::Goals,
        "memories" => WorkspaceFile::Memories,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "code": "UNKNOWN_FILE",
                        "message": format!("Unknown file '{}': must be 'personality', 'goals', or 'memories'", file)
                    }
                })),
            )
                .into_response();
        }
    };

    let workspace = AgentWorkspace::new(workspace_dir, agent_id);

    // Write directly using the filesystem path — user API allows all files including personality.
    if let Err(e) = workspace.ensure_dir() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "IO_ERROR", "message": e.to_string() }
            })),
        )
            .into_response();
    }

    let path = workspace.dir().join(workspace_file.filename());
    match std::fs::write(&path, &body.content) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "file": workspace_file.filename(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "IO_ERROR", "message": e.to_string() }
            })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceFileRequest {
    pub content: String,
}
