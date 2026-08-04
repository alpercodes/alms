use crate::server::AppState;
use axum::{Json, extract::State};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SubscriberMetrics {
    pub runs: usize,
    pub sessions: usize,
    pub agents: usize,
    pub activity: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct OperationalMetricsSnapshot {
    pub queue_saturation_rejections_total: u64,
    pub lifecycle_transition_rejections_total: u64,
    pub replay_gaps_total: u64,
    pub replay_epoch_mismatches_total: u64,
    pub subscribers: SubscriberMetrics,
    pub persistence_snapshot_rejections_total: u64,
    pub job_dispatch_retry_attempts_total: u64,
    pub job_dispatch_retry_exhaustions_total: u64,
    /// Episode-close job writes that exhausted their retry budget (#1233).
    /// Non-zero means at least one job advanced its schedule in memory only.
    pub job_rearm_failures_total: u64,
    /// Run rows the startup sweep could not reconcile and skipped (#1236).
    /// Non-zero means the daemon booted with at least one durable row still
    /// claiming `queued`/`running` from a dead process.
    pub stale_run_recovery_failures_total: u64,
    /// Jobs that were already past due at boot and were staggered into the
    /// catch-up cohort (#1235). Sized by how long the daemon was down.
    pub job_boot_catch_ups_total: u64,
    /// Jobs that could not be re-registered with the scheduler at startup and
    /// were skipped so the daemon could still start. Non-zero means those jobs
    /// will not fire until repaired or recreated.
    pub job_bootstrap_failures_total: u64,
}

pub async fn get_operational_metrics(
    State(state): State<AppState>,
) -> Json<OperationalMetricsSnapshot> {
    let run_metrics = state.run_manager.operational_metrics();
    let store = state.session_manager.store();
    let persistence_snapshot_rejections_total =
        store.map_or(0, |store| store.persistence_snapshot_rejections_total());
    let stale_run_recovery_failures_total =
        store.map_or(0, |store| store.stale_run_recovery_failures_total());

    Json(OperationalMetricsSnapshot {
        queue_saturation_rejections_total: state.agent_queue.saturation_rejections(),
        lifecycle_transition_rejections_total: run_metrics.transition_rejections_total,
        replay_gaps_total: run_metrics.replay_gaps_total,
        replay_epoch_mismatches_total: run_metrics.replay_epoch_mismatches_total,
        subscribers: SubscriberMetrics {
            runs: run_metrics.run_subscribers,
            sessions: run_metrics.session_subscribers,
            agents: run_metrics.agent_subscribers,
            activity: run_metrics.activity_subscribers,
        },
        persistence_snapshot_rejections_total,
        job_dispatch_retry_attempts_total: state.job_store.retry_attempts_total(),
        job_dispatch_retry_exhaustions_total: state.job_store.retry_exhaustions_total(),
        job_rearm_failures_total: state.job_store.rearm_failures_total(),
        stale_run_recovery_failures_total,
        job_boot_catch_ups_total: state.job_store.boot_catch_ups_total(),
        job_bootstrap_failures_total: state.job_store.bootstrap_failures_total(),
    })
}
