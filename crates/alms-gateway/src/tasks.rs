//! HTTP handlers for coordinator task endpoints.
//!
//! GET /tasks         — list all active subagent tasks
//! GET /tasks/{id}    — get status of a specific task

use crate::api_error;
use crate::server::AppState;
use alms_coordinator::TaskId;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

/// GET /tasks — list all active subagent tasks
pub async fn list_tasks(State(state): State<AppState>) -> impl IntoResponse {
    let tasks = state.coordinator.list_active();
    let items: Vec<serde_json::Value> = tasks
        .into_iter()
        .map(|(task_id, status)| {
            serde_json::json!({
                "task_id": task_id.0,
                "status": format!("{:?}", status),
            })
        })
        .collect();
    Json(serde_json::json!({ "tasks": items }))
}

/// GET /tasks/{task_id} — get status of a specific subagent task
pub async fn get_task(
    State(state): State<AppState>,
    Path(task_id_str): Path<String>,
) -> impl IntoResponse {
    let uuid = match task_id_str.parse::<uuid::Uuid>() {
        Ok(u) => u,
        Err(_) => {
            return api_error(StatusCode::BAD_REQUEST, "BAD_REQUEST", "invalid task_id")
                .into_response();
        }
    };

    let task_id = TaskId(uuid);
    match state.coordinator.get_status(task_id) {
        Some(status) => Json(serde_json::json!({
            "task_id": uuid,
            "status": format!("{:?}", status),
        }))
        .into_response(),
        None => api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Task not found").into_response(),
    }
}
