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
}

pub async fn get_operational_metrics(
    State(state): State<AppState>,
) -> Json<OperationalMetricsSnapshot> {
    let run_metrics = state.run_manager.operational_metrics();
    let persistence_snapshot_rejections_total = state
        .session_manager
        .store()
        .map_or(0, |store| store.persistence_snapshot_rejections_total());

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
    })
}
