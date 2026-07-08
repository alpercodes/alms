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
use tracing::{info, warn};

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

/// Serialize a job, attaching the open-episode snapshot (#1198 step 8)
/// when the job's episode is still in flight. Jobs with no open episode
/// keep the pre-#1198 wire shape (no `episode` key).
fn job_json(state: &AppState, job: &alms_core::job::Job) -> serde_json::Value {
    let mut value = serde_json::json!(job);
    if let Some(episode) = state.job_episodes.snapshot(job.id) {
        value["episode"] = episode;
    }
    value
}

/// GET /jobs
pub async fn list_jobs(State(state): State<AppState>) -> impl IntoResponse {
    let jobs: Vec<serde_json::Value> = state
        .job_store
        .list()
        .iter()
        .map(|job| job_json(&state, job))
        .collect();
    Json(serde_json::json!({ "jobs": jobs }))
}

/// GET /jobs/{job_id}
pub async fn get_job(
    State(state): State<AppState>,
    Path(job_id): Path<JobId>,
) -> impl IntoResponse {
    match state.job_store.get(job_id) {
        Some(job) => Json(job_json(&state, &job)).into_response(),
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
            // -- Teardown-critical mutations, all SYNCHRONOUS --
            //
            // Everything up to the first `.await` below runs even if the
            // client disconnects mid-request (Tim's S3 on PR #1210: an
            // axum handler future dropped at an await point simply stops —
            // a Cancelled job must never be left with a live episode,
            // uncancelled runs, or missing suppression intent). Keep new
            // teardown-critical state changes ABOVE the first await.

            // #1206 follow-up (Codex P2 on PR #1210): record the
            // *operator's* cancel intent. The orphan-run suppression in
            // `enqueue_triggered_run` keys on this set — NOT on
            // `JobStatus::Cancelled`, which a normally-spent one-shot also
            // carries (#763: `JobStatus` has no `Completed` variant).
            state.operator_cancelled_jobs.insert(job_id);

            // #1198 D7: remove the episode FIRST so in-flight run exits and
            // pending DM/subagent resolutions no-op (Untracked / miss); its
            // pending work is torn down below.
            let episode = state.job_episodes.remove(job_id);

            // Cancel any in-progress run that was spawned by this job so
            // we stop burning tokens on work the operator intended to halt.
            // Covers turn-1 runs AND episode continuation runs — both carry
            // the job id (#1198 step 5 stamping).
            // (cancel_runs_for_job logs each cancelled run individually)
            state.run_manager.cancel_runs_for_job(job_id);

            // -- Best-effort async teardown --

            // Also cancel in the scheduler so it doesn't fire again. (A
            // stray firing after a dropped handler is absorbed by the fire
            // path's own Cancelled check.)
            state.scheduler.cancel(job_id).await;

            // Tear down the episode's pending work: end open DMs with
            // UserCancelled (the peer gets the standard ended-notification
            // instead of being stranded) and cancel running background
            // subagents.
            if let Some(episode) = episode {
                teardown_episode(&state, episode).await;
            }

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

/// Tear down a cancelled job's open episode (#1198 D7).
///
/// - **Pending DMs**: cancel active runs on the DM session and call
///   `end_conversation(UserCancelled)` so the peer receives the standard
///   ended-notification. Unlike the D5 deadline (detach-and-complete),
///   ending the DM here is unambiguously right — the operator explicitly
///   killed the job.
/// - **Pending subagents**: fire the session-keyed cancel; the resulting
///   `SubagentCompletion(Cancelled)` finds no episode (already removed)
///   and degrades to a plain notification run on the job session.
///
/// Best-effort throughout: resolution failures are logged and skipped —
/// the depth-expiry sweep remains the eventual fallback for a DM whose
/// participants cannot be resolved.
async fn teardown_episode(state: &AppState, episode: crate::runs::job_episode::JobEpisode) {
    use alms_tools::message_sender::{ConversationEndReason, MessageSender as _};

    for dm_session_id in &episode.pending_dms {
        // Stop any in-flight turn on the DM session first.
        let cancelled = state.run_manager.cancel_runs_for_session(*dm_session_id);
        if cancelled > 0 {
            info!(
                dm_session = %dm_session_id.0,
                cancelled,
                "Cancelled active DM runs during job-episode teardown"
            );
        }

        // Resolve the pair from the DM session's context id and end the
        // conversation with the job agent as the sender.
        let Ok(session) = state.session_manager.get(*dm_session_id) else {
            warn!(
                dm_session = %dm_session_id.0,
                "Pending DM session not found during teardown — skipping end_conversation"
            );
            continue;
        };
        let Some((name_a, name_b)) = alms_core::dm_participants(&session.context_id) else {
            warn!(
                dm_session = %dm_session_id.0,
                context_id = %session.context_id,
                "Pending DM session has a non-DM context — skipping end_conversation"
            );
            continue;
        };
        let Some(store) = state.session_manager.store() else {
            warn!("No agent store available — skipping end_conversation during teardown");
            continue;
        };
        let (Ok(Some(rec_a)), Ok(Some(rec_b))) = (
            store.load_agent_by_name(name_a),
            store.load_agent_by_name(name_b),
        ) else {
            warn!(
                dm_session = %dm_session_id.0,
                "Could not resolve both DM participants during teardown — skipping"
            );
            continue;
        };
        let (sender, peer) = if rec_a.id == episode.agent_id {
            (rec_a, rec_b)
        } else {
            (rec_b, rec_a)
        };
        if let Err(e) = state
            .message_bus
            .end_conversation(
                &sender.name,
                sender.id,
                &peer.name,
                peer.id,
                ConversationEndReason::UserCancelled,
            )
            .await
        {
            warn!(
                dm_session = %dm_session_id.0,
                error = %e,
                "end_conversation failed during job-episode teardown"
            );
        }
    }

    for (task_id, subagent_session) in &episode.pending_subagents {
        let fired = state
            .coordinator
            .cancel_subagent_by_session(*subagent_session);
        info!(
            task_id = %task_id,
            subagent_session = %subagent_session.0,
            fired,
            "Cancelled pending background subagent during job-episode teardown"
        );
    }
}
