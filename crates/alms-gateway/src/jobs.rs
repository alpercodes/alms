//! HTTP handlers for scheduled job management.
//!
//! Routes:
//!   POST   /jobs           — create a new job
//!   GET    /jobs           — list all jobs
//!   GET    /jobs/{job_id}  — get a single job
//!   DELETE /jobs/{job_id}  — cancel a job

use crate::api_error;
use crate::cron_utils;
use crate::server::AppState;
use alms_core::job::{CreateJobRequest, JobId, JobSchedule};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use tracing::warn;

/// POST /jobs
pub async fn create_job(
    State(state): State<AppState>,
    Json(req): Json<CreateJobRequest>,
) -> impl IntoResponse {
    // Validate cron expression for recurring jobs (must be 5 fields and parseable).
    if let JobSchedule::Recurring { ref cron } = req.schedule {
        let fields: Vec<&str> = cron.split_whitespace().collect();
        if fields.len() != 5 {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_CRON",
                format!(
                    "cron expression must have exactly 5 fields, got {}",
                    fields.len()
                ),
            )
            .into_response();
        }
        if cron_utils::next_after(cron, Utc::now()).is_none() {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_CRON",
                "cron expression is invalid or has no future occurrences",
            )
            .into_response();
        }
    }

    let job = match state.job_store.create(req) {
        Ok(job) => job,
        Err(e) => {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", e)
                .into_response();
        }
    };

    // Register the job with the scheduler so it fires at the right time.
    let now = Utc::now();
    if let Some(fire_at) = cron_utils::compute_next_fire(&job, now) {
        let delay = (fire_at - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job.id, instant).await;
        if let Err(e) = state.job_store.update_next_run_at(job.id, Some(fire_at)) {
            warn!("Failed to persist next_run_at for job {}: {}", job.id.0, e);
        }
    } else {
        warn!(
            "Job {} has no computable fire time — created but will not fire",
            job.id.0
        );
    }

    (StatusCode::CREATED, Json(serde_json::json!(job))).into_response()
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
        None => api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "job not found").into_response(),
    }
}

/// DELETE /jobs/{job_id}
pub async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> impl IntoResponse {
    match state.job_store.cancel(job_id) {
        Ok(Some(true)) => {
            // Also cancel in the scheduler so it doesn't fire again.
            state.scheduler.cancel(job_id).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Some(false)) => api_error(
            StatusCode::CONFLICT,
            "ALREADY_CANCELLED",
            "job is already cancelled",
        )
        .into_response(),
        Ok(None) => api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "job not found").into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", e).into_response(),
    }
}
