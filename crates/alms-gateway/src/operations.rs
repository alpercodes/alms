use crate::server::AppState;
use alms_session::sqlite::{DegradedField, PersistenceTable};
use axum::{Json, extract::State};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SubscriberMetrics {
    pub runs: usize,
    pub sessions: usize,
    pub agents: usize,
    pub activity: usize,
}

/// Process-lifetime counters behind `GET /operations/metrics`.
///
/// The thirteen scalar counters fall into three groups, declared in group
/// order because the wire names alone do not distinguish them. **This grouping
/// is mirrored in `docs/api.md` § 8.1 and the two must be changed together** —
/// the names do not carry the group, so the prose has to, and prose drifts.
///
/// 1. **Rejections** — a request, transition, or dispatch the daemon refused
///    or had to retry. Expected to be non-zero under load; read as a rate,
///    not as an absolute. Nothing durable is in doubt.
/// 2. **Quarantine and degradation counters** — durable state the daemon
///    could not take at face value (`*_failures_total`,
///    `persistence_rows_skipped_total`, `persistence_fields_degraded_total`).
///    Any non-zero value means the daemon is running on a view of the database
///    that does not match what is on disk. See `docs/architecture.md` §
///    "Reconciliation policy: absence must be a safe belief" for what each
///    site owes an operator.
///
///    **The group holds two different faults, and the second is the worse
///    one.** A *quarantine* counter means trust was withheld: the row was
///    dropped and kept out of live state, so the policy's obligation 3 holds
///    and the daemon's view is merely incomplete. A *degradation* counter
///    (`persistence_fields_degraded_total`, #1246) means trust was misplaced:
///    the row **is** in live state carrying a column the parser could not
///    read. Obligation 3 cannot apply — there is no way to withhold one
///    column without dropping the row, and at those sites dropping is worse —
///    so the fault propagates instead of being contained, and is invisible
///    from the outside. Incomplete beats wrong:
///    `persistence_fields_degraded_total` deserves the louder alert of the
///    two. Which *field* moved decides the urgency; see `docs/api.md` § 8.1
///    for the per-field consequences.
/// 3. **Workload counters** — `job_boot_catch_ups_total` is the only one, and
///    it measures work done rather than trust withheld. A large value after a
///    long outage is expected, not a fault.
///
/// `subscribers` belongs to none of the three: it is a point-in-time gauge,
/// not a process-lifetime counter, and is declared last for that reason.
#[derive(Debug, Clone, Serialize)]
pub struct OperationalMetricsSnapshot {
    // -- Rejections ---------------------------------------------------------
    pub queue_saturation_rejections_total: u64,
    pub lifecycle_transition_rejections_total: u64,
    pub replay_gaps_total: u64,
    pub replay_epoch_mismatches_total: u64,
    pub persistence_snapshot_rejections_total: u64,
    pub job_dispatch_retry_attempts_total: u64,
    pub job_dispatch_retry_exhaustions_total: u64,
    // -- Quarantine ---------------------------------------------------------
    /// Episode-close job writes that exhausted their retry budget (#1233).
    /// Non-zero means at least one job advanced its schedule in memory only.
    pub job_rearm_failures_total: u64,
    /// Run rows the startup sweep could not reconcile and skipped (#1236).
    /// Non-zero means the daemon booted with at least one durable row still
    /// claiming `queued`/`running` from a dead process.
    pub stale_run_recovery_failures_total: u64,
    /// Jobs that could not be re-registered with the scheduler at startup and
    /// were skipped so the daemon could still start. Non-zero means those jobs
    /// will not fire until repaired or recreated.
    pub job_bootstrap_failures_total: u64,
    /// Durable rows a loader could not parse and dropped (#1241), summed
    /// across every table. Unlike the two sweep counters above this is not a
    /// startup-only number: the loaders run on every read, so a corrupt row
    /// on a hot path increments this repeatedly. Watch the rate, not the
    /// absolute value.
    pub persistence_rows_skipped_total: u64,
    /// Per-table breakdown of `persistence_rows_skipped_total`, keyed by SQL
    /// table name (`timeline` is the `messages`/`runs` union behind
    /// `GET /timeline`, not a table). Every known table is reported,
    /// including the zeroes, so the key set is stable across deployments.
    pub persistence_rows_skipped_by_table: BTreeMap<&'static str, u64>,
    /// Durable columns a parser could not read and replaced with a fallback,
    /// **keeping the row** (#1246), summed across every field. Distinct from
    /// `persistence_rows_skipped_total` on purpose: these rows are served, not
    /// withheld, so the bad value reaches live state and is invisible from the
    /// outside. The worst of the four is
    /// `session_summaries.last_run_id`, the compare-and-swap sentinel for
    /// episodic summaries: degraded, it makes every future summary upsert for
    /// that session report a false conflict and burn three LLM
    /// summarizations. Like the row-skip counter, the loader-backed fields
    /// count occurrences rather than distinct rows.
    pub persistence_fields_degraded_total: u64,
    /// Per-field breakdown of `persistence_fields_degraded_total`, keyed
    /// `<table>.<column>` so the key names the exact cell to inspect with
    /// `sqlite3`. Every known field is reported, including the zeroes, so the
    /// key set is stable across deployments.
    pub persistence_fields_degraded_by_field: BTreeMap<&'static str, u64>,
    // -- Workload -----------------------------------------------------------
    /// Jobs that were already past due at boot and were staggered into the
    /// catch-up cohort (#1235). Sized by how long the daemon was down.
    pub job_boot_catch_ups_total: u64,
    // -- Gauges (not counters) ----------------------------------------------
    /// Live SSE subscriber counts per stream. A gauge: it goes down as well
    /// as up, so it is not comparable with anything above it.
    pub subscribers: SubscriberMetrics,
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
    let persistence_rows_skipped_total = store.map_or(0, |store| store.rows_skipped_total());
    // Report the full table key set even with no SQLite store configured, so
    // a scraper never sees keys appear and disappear between deployments.
    let persistence_rows_skipped_by_table = store.map_or_else(
        || {
            PersistenceTable::ALL
                .iter()
                .map(|table| (table.as_str(), 0))
                .collect()
        },
        |store| store.rows_skipped_by_table(),
    );
    let persistence_fields_degraded_total = store.map_or(0, |store| store.fields_degraded_total());
    // Same reasoning as the table key set above: report every known field even
    // with no SQLite store configured, so a scraper never sees keys appear and
    // disappear between deployments.
    let persistence_fields_degraded_by_field = store.map_or_else(
        || {
            DegradedField::ALL
                .iter()
                .map(|field| (field.as_str(), 0))
                .collect()
        },
        |store| store.fields_degraded_by_field(),
    );

    Json(OperationalMetricsSnapshot {
        queue_saturation_rejections_total: state.agent_queue.saturation_rejections(),
        lifecycle_transition_rejections_total: run_metrics.transition_rejections_total,
        replay_gaps_total: run_metrics.replay_gaps_total,
        replay_epoch_mismatches_total: run_metrics.replay_epoch_mismatches_total,
        persistence_snapshot_rejections_total,
        job_dispatch_retry_attempts_total: state.job_store.retry_attempts_total(),
        job_dispatch_retry_exhaustions_total: state.job_store.retry_exhaustions_total(),
        job_rearm_failures_total: state.job_store.rearm_failures_total(),
        stale_run_recovery_failures_total,
        job_bootstrap_failures_total: state.job_store.bootstrap_failures_total(),
        persistence_rows_skipped_total,
        persistence_rows_skipped_by_table,
        persistence_fields_degraded_total,
        persistence_fields_degraded_by_field,
        job_boot_catch_ups_total: state.job_store.boot_catch_ups_total(),
        subscribers: SubscriberMetrics {
            runs: run_metrics.run_subscribers,
            sessions: run_metrics.session_subscribers,
            agents: run_metrics.agent_subscribers,
            activity: run_metrics.activity_subscribers,
        },
    })
}
