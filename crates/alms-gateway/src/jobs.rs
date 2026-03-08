//! HTTP handlers for scheduled job management.
//!
//! Routes:
//!   POST   /jobs           — create a new job
//!   GET    /jobs           — list all jobs
//!   GET    /jobs/{job_id}  — get a single job
//!   DELETE /jobs/{job_id}  — cancel a job

use crate::server::AppState;
use alms_core::job::{CreateJobRequest, JobId, JobSchedule};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

/// POST /jobs
pub async fn create_job(
    State(state): State<AppState>,
    Json(req): Json<CreateJobRequest>,
) -> impl IntoResponse {
    // Validate cron expression for recurring jobs (must be 5 fields).
    if let JobSchedule::Recurring { ref cron } = req.schedule {
        let fields: Vec<&str> = cron.split_whitespace().collect();
        if fields.len() != 5 {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": {
                        "code": "INVALID_CRON",
                        "message": format!(
                            "cron expression must have exactly 5 fields, got {}",
                            fields.len()
                        )
                    }
                })),
            )
                .into_response();
        }
    }

    match state.job_store.create(req) {
        Ok(job) => (StatusCode::CREATED, Json(serde_json::json!(job))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL_ERROR", "message": e.to_string() }
            })),
        )
            .into_response(),
    }
}

/// GET /jobs
pub async fn list_jobs(State(state): State<AppState>) -> impl IntoResponse {
    let jobs = state.job_store.list();
    Json(serde_json::json!({ "jobs": jobs }))
}

/// GET /jobs/{job_id}
pub async fn get_job(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> impl IntoResponse {
    match state.job_store.get(job_id) {
        Some(job) => Json(serde_json::json!(job)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "job not found" }
            })),
        )
            .into_response(),
    }
}

/// DELETE /jobs/{job_id}
pub async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> impl IntoResponse {
    match state.job_store.cancel(job_id) {
        Ok(Some(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Some(false)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": { "code": "ALREADY_CANCELLED", "message": "job is already cancelled" }
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "NOT_FOUND", "message": "job not found" }
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "code": "INTERNAL_ERROR", "message": e.to_string() }
            })),
        )
            .into_response(),
    }
}
