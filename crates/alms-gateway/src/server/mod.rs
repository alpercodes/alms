// SPDX-License-Identifier: Apache-2.0

//! HTTP server for ALMS Gateway
//!
//! Provides REST API endpoints per docs/api.md specification.
//!
//! This module is split into submodules by concern:
//! - [`run_manager`] — run tracking, event broadcasting, cancellation, persistence
//! - [`state`] — [`AppState`] construction and initialization
//! - [`routes`] — Axum router setup and HTTP handler functions

pub(crate) mod routes;
mod run_manager;
mod state;

pub use run_manager::{ManagedSubscription, RunManager};
pub use state::{AppState, ServerLlmDefault};

use crate::auth::{AuthToken, no_cache, require_auth};
use crate::cron_utils;
use crate::gateway::Gateway;
use crate::runs::{
    completion_notification_loop, dm_event_loop, job_episode_sweep_loop, run_trigger_loop,
    scheduler_fire_loop,
};
use alms_core::AlmsResult;
use alms_runtime::Scheduler;
use axum::{Extension, middleware};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

// Re-export SSE types
pub use crate::sse::{RunEventStream, event_channel};

/// TCP listener wrapper that sets TCP_NODELAY on every accepted connection.
///
/// Nagle's algorithm buffers small writes, which delays SSE events from
/// reaching the browser.  Disabling it ensures token_delta frames are sent
/// immediately.
struct NoDelayListener(tokio::net::TcpListener);

impl axum::serve::Listener for NoDelayListener {
    type Io = tokio::net::TcpStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    return (stream, addr);
                }
                Err(e) => {
                    tracing::error!("TCP accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// Start the gateway HTTP server from a pre-loaded config.
///
/// This is the preferred entry-point: the caller loads `AlmsConfig` once
/// and passes it here, avoiding a second parse inside the gateway.
pub async fn serve_with_config(bind_addr: &str, config: &alms_core::AlmsConfig) -> AlmsResult<()> {
    let gateway_config = crate::gateway::GatewayConfig::from_alms_config_with_env(config);
    let gateway = Gateway::new(gateway_config)?;
    serve_with_gateway(bind_addr, gateway).await
}

/// Start the gateway HTTP server (loads config from env internally).
///
/// Prefer `serve_with_config` when the caller already has an `AlmsConfig`
/// to avoid parsing config twice.
pub async fn serve(bind_addr: &str) -> AlmsResult<()> {
    let gateway = Gateway::from_env()?;
    serve_with_gateway(bind_addr, gateway).await
}

pub async fn serve_with_gateway(bind_addr: &str, gateway: Gateway) -> AlmsResult<()> {
    let shutdown_token = CancellationToken::new();

    // Create the scheduler with a fire channel so job IDs are forwarded to
    // the gateway for actual agent-run dispatch.
    let (fire_tx, fire_rx) = tokio::sync::mpsc::unbounded_channel::<alms_core::JobId>();
    let scheduler = Arc::new(Scheduler::new().with_fire_channel(fire_tx));

    let (completion_tx, completion_rx) =
        tokio::sync::mpsc::unbounded_channel::<alms_coordinator::SubagentCompletion>();

    // Bounded channels (#842 / B11): MessageBus reserves run-trigger capacity
    // before mutating durable DM state and returns an explicit error when
    // saturated. DM SSE decoration is best-effort and drops on saturation.
    let (run_trigger_tx, run_trigger_rx) =
        tokio::sync::mpsc::channel::<alms_coordinator::message_bus::RunTrigger>(
            alms_coordinator::message_bus::RUN_TRIGGER_CHANNEL_CAPACITY,
        );

    let (dm_event_tx, dm_event_rx) = tokio::sync::mpsc::channel::<
        alms_coordinator::message_bus::DmEvent,
    >(alms_coordinator::message_bus::DM_EVENT_CHANNEL_CAPACITY);

    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        run_trigger_tx,
        dm_event_tx,
    )?;

    {
        let mut gateway = state.gateway.lock().await;
        gateway.initialize_channels().await?;
        gateway.start().await?;
    }

    // Re-register persisted jobs before starting the runner so the heap is
    // populated before the first sleep.
    bootstrap_scheduler(&state).await?;

    // Start the background scheduler runner (shutdown-aware).
    let scheduler_handle = state.scheduler.start_with_shutdown(shutdown_token.clone());

    // Spawn the fire-receiver: turns fired JobIds into real agent runs.
    let fire_state = state.clone();
    let fire_handle = tokio::spawn(scheduler_fire_loop(fire_rx, fire_state));

    // Spawn the completion-notification loop: turns background subagent
    // completions into follow-up runs on the parent session.
    let completion_state = state.clone();
    let completion_handle = tokio::spawn(completion_notification_loop(
        completion_rx,
        completion_state,
    ));

    // Spawn the run-trigger loop: processes peer message triggers from the
    // MessageBus and creates runs on the target agent's session.
    let trigger_state = state.clone();
    let trigger_handle = tokio::spawn(run_trigger_loop(run_trigger_rx, trigger_state));

    // Spawn the DM event loop: forwards DM message persistence events to SSE
    // subscribers watching DM sessions (#632).
    let dm_event_state = state.clone();
    let dm_event_handle = tokio::spawn(dm_event_loop(dm_event_rx, dm_event_state));

    // Spawn the job-episode deadline sweep (#1198 D5): force-closes episodes
    // past their 4-hour deadline with detach-and-complete semantics. Exits
    // cooperatively on the shutdown token.
    let episode_sweep_state = state.clone();
    let episode_sweep_handle = tokio::spawn(job_episode_sweep_loop(episode_sweep_state));

    // Use the auth token snapshot from AppState — no mutex lock needed.
    let auth_token = AuthToken(state.auth_token_value.clone());

    // Spawn the channel message loop (Telegram polling, etc.).
    // The loop selects on the shutdown token so it exits cooperatively
    // without requiring us to lock the gateway mutex from outside.
    let background_gateway = state.gateway.clone();
    let gateway_token = shutdown_token.clone();
    let gateway_agent_queue = state.agent_queue.clone();
    let gateway_handle = tokio::spawn(async move {
        let mut gateway = background_gateway.lock().await;
        if let Err(e) = gateway
            .run_until_shutdown(gateway_token, gateway_agent_queue)
            .await
        {
            tracing::error!("Gateway message loop exited: {}", e);
        }
    });
    if auth_token.0.is_none() {
        tracing::warn!(
            "ALMS_AUTH_TOKEN is not set — API authentication is DISABLED. \
             Set it before exposing to the network."
        );
    } else {
        info!("API authentication enabled");
    }

    let app = routes::public_router()
        .merge(
            routes::protected_router()
                .layer(middleware::from_fn(no_cache))
                .layer(middleware::from_fn(require_auth))
                .layer(Extension(auth_token)),
        )
        .with_state(state.clone());

    info!("Starting ALMS Gateway HTTP server on {}", bind_addr);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    // === Graceful shutdown sequence ===
    // Wrap in NoDelayListener so every accepted connection has TCP_NODELAY.
    // This disables Nagle buffering, which is critical for SSE: without it,
    // small token_delta events sit in the TCP send buffer and never reach
    // the browser until the connection closes (observed on Windows).
    //
    // Axum's graceful shutdown waits for ALL in-flight connections to close
    // after the shutdown signal fires.  shutdown_signal already closes SSE
    // senders, but a reconnecting EventSource or a slow request could keep
    // a connection alive.  We cap Axum's connection drain with a secondary
    // timeout that starts once the shutdown token is cancelled.
    let shutdown_token_for_drain = state.shutdown_token.clone();
    let serve_future = axum::serve(NoDelayListener(listener), app)
        .with_graceful_shutdown(shutdown_signal(shutdown_token, state.run_manager.clone()));
    tokio::select! {
        result = serve_future => {
            if let Err(e) = result {
                return Err(alms_core::AlmsError::Runtime(format!("Serve error: {e}")));
            }
        }
        // Once the shutdown token fires (inside shutdown_signal), give Axum
        // 5 seconds to drain connections before we move on regardless.
        _ = async {
            shutdown_token_for_drain.cancelled().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        } => {
            tracing::warn!("Axum connection drain timed out after 5s — proceeding with shutdown");
        }
    }

    // Phase 1: Signal received. Axum stopped accepting new connections.
    let shutdown_start = std::time::Instant::now();
    info!("HTTP server stopped accepting connections, draining...");

    // Phase 2: Scheduler loop already exiting (token cancelled).
    scheduler_handle.await.ok();
    info!("Scheduler stopped");

    // Phase 3: Abort the fire loop. The scheduler is stopped so no new
    // job IDs will arrive. The fire_tx is kept alive by Arc inside the
    // fire loop's AppState clone, so rx.recv() would hang — abort instead.
    // Any in-flight runs spawned by fire_job_run are tracked by the
    // in-flight counter and will be drained in phase 5.
    fire_handle.abort();
    fire_handle.await.ok();
    info!("Scheduler fire loop stopped");

    completion_handle.abort();
    completion_handle.await.ok();
    info!("Completion notification loop stopped");

    trigger_handle.abort();
    trigger_handle.await.ok();
    info!("RunTrigger loop stopped");

    dm_event_handle.abort();
    dm_event_handle.await.ok();
    info!("DM event loop stopped");

    // The sweep loop selects on the shutdown token, so it is already
    // exiting — the abort is belt-and-braces against a tick in flight.
    episode_sweep_handle.abort();
    episode_sweep_handle.await.ok();
    info!("Job episode sweep loop stopped");

    // Phase 4: Gateway message loop already exiting (token cancelled).
    gateway_handle.await.ok();
    info!("Channel adapters stopped");

    // Phase 5: Wait for in-flight runs to complete (with timeout).
    // All run cancel-tokens were already triggered in shutdown_signal, so
    // most runs should exit within a few seconds.  We use a short 8-second
    // timeout since the runs are being cooperatively cancelled.
    let in_flight = state.run_manager.in_flight_count();
    if in_flight > 0 {
        info!(
            "Waiting for {} in-flight run(s) to finish (timeout 8s)...",
            in_flight
        );
    }
    let drain_timeout = std::time::Duration::from_secs(8);
    let drained = state.run_manager.wait_drain(drain_timeout).await;
    if drained {
        info!("All in-flight runs completed");
    } else {
        tracing::warn!(
            "Shutdown drain timeout: {} run(s) still in-flight after 8s — exiting anyway",
            state.run_manager.in_flight_count()
        );
    }

    // Phase 6: Flush SQLite WAL.
    if let Err(e) = state.session_manager.flush_wal() {
        tracing::error!("Failed to flush session WAL: {}", e);
    }
    if let Err(e) = state.job_store.flush_wal() {
        tracing::error!("Failed to flush job WAL: {}", e);
    }
    info!("SQLite WAL flushed");

    info!(
        "Shutdown complete in {:.1}s",
        shutdown_start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Returns a future that completes when a shutdown signal is received.
///
/// After the signal fires, this function:
/// 1. Cancels the shutdown token (stops scheduler, gateway message loop, etc.)
/// 2. Closes all SSE sender channels so persistent SSE connections terminate
///    and Axum's graceful shutdown can actually complete.
async fn shutdown_signal(token: CancellationToken, run_manager: RunManager) {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => { info!("Received Ctrl+C, initiating graceful shutdown"); }
            _ = sigterm.recv() => { info!("Received SIGTERM, initiating graceful shutdown"); }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to install Ctrl+C handler");
        info!("Received Ctrl+C, initiating graceful shutdown");
    }

    token.cancel();

    // Close all SSE sender channels so persistent SSE connections (especially
    // session-level streams) terminate. Without this, Axum's graceful shutdown
    // waits indefinitely for long-lived SSE connections to close.
    run_manager.close_all_senders();

    // Cancel every in-flight run so agent loops exit at their next
    // cancellation check-point.  Without this, runs continue until they
    // finish naturally and the drain timeout is the only backstop.
    let cancelled = run_manager.cancel_all_in_flight();
    if cancelled > 0 {
        info!("Cancelled {} in-flight run(s) for shutdown", cancelled);
    }
}

/// Delay before the first boot-time catch-up firing (#1235). Preserves the
/// pre-stagger head start so a single missed job still fires promptly.
const BOOT_CATCH_UP_LEAD_SECS: i64 = 1;

/// Fixed spacing between successive boot-time catch-up firings (#1235).
///
/// Phase 7 made persisted `next_run_at` win for every schedule type, so a
/// past-due tick became `now + 1s` — after any downtime, every recurring job
/// plus every unfired one-shot fired within one second of startup,
/// concurrently, bounded only by the agent queue. Restarting the gateway
/// became a spend event proportional to how long the daemon was down.
///
/// Fixed spacing is chosen over jitter or a concurrency cap because it is
/// deterministic (directly testable, reproducible in an incident) and it caps
/// the catch-up *rate* rather than just the instantaneous concurrency: a
/// concurrency cap still lets N jobs run as fast as the queue drains them.
/// The spread is intentionally unbounded in cohort size — spreading 200
/// missed jobs over 50 minutes is the desired outcome, not a burst.
const BOOT_CATCH_UP_SPACING_SECS: i64 = 15;

/// Re-register all non-terminal persisted jobs with the scheduler on startup.
///
/// Jobs whose fire time is still in the future keep their real schedule.
/// Jobs that are already past due form the **catch-up cohort** and are
/// staggered by [`plan_catch_up_cohort`] instead of all firing at once.
async fn bootstrap_scheduler(state: &AppState) -> AlmsResult<()> {
    let now = chrono::Utc::now();
    let jobs = state.job_store.list();
    let mut scheduled: Vec<(alms_core::JobId, chrono::DateTime<chrono::Utc>)> = Vec::new();
    let mut catch_up: Vec<(alms_core::JobId, chrono::DateTime<chrono::Utc>)> = Vec::new();

    for job in jobs {
        match bootstrap_fire_at(&job, now) {
            Some(BootstrapFire::Scheduled(fire_at)) => scheduled.push((job.id, fire_at)),
            Some(BootstrapFire::CatchUp { due_at }) => catch_up.push((job.id, due_at)),
            None => {
                if !job.status().is_terminal() {
                    tracing::warn!("Job {} has no future fire time, skipping bootstrap", job.id);
                }
            }
        }
    }

    // Catch-up entries are tagged so the cohort counter reflects what was
    // actually registered, not what was merely planned.
    let mut registrations: Vec<(alms_core::JobId, chrono::DateTime<chrono::Utc>, bool)> = scheduled
        .into_iter()
        .map(|(id, at)| (id, at, false))
        .collect();
    registrations.extend(
        plan_catch_up_cohort(catch_up, now)
            .into_iter()
            .map(|(id, at)| (id, at, true)),
    );

    let mut registered = 0usize;
    let mut registered_catch_ups = 0u64;
    let mut skipped = 0usize;
    for (job_id, fire_at, is_catch_up) in registrations {
        // Persist the recovery point before projecting it into the scheduler.
        //
        // A per-row failure is skipped rather than propagated (#1236's
        // argument, applied to jobs). `update_next_run_at` errors on a
        // `save_job` failure or on lifecycle-revision exhaustion; propagating
        // it aborted this whole loop AND `serve_with_gateway`, so one bad job
        // row meant no jobs scheduled at all and a daemon that would not
        // start. The stagger sharpens that: the cohort is ordered
        // most-overdue-first, so the likeliest-corrupt legacy row is processed
        // first and would take the largest share of the cohort with it. The
        // fail-open-vs-fail-closed policy this shares with the stale-run sweep
        // is tracked in #1237; both sweeps now skip rather than brick.
        if let Err(error) = state.job_store.update_next_run_at(job_id, Some(fire_at)) {
            state.job_store.record_bootstrap_failure();
            skipped += 1;
            tracing::error!(
                %job_id,
                %fire_at,
                %error,
                "Could not persist a job's startup fire time — skipping it so the remaining \
                 jobs still schedule and the daemon still starts. This job will not fire until \
                 it is repaired or recreated (#1236)"
            );
            continue;
        }
        let delay = (fire_at - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job_id, instant).await;
        registered += 1;
        if is_catch_up {
            registered_catch_ups += 1;
        }
    }

    if registered > 0 {
        info!("Bootstrapped {} job(s) into scheduler", registered);
    }
    if skipped > 0 {
        tracing::error!(
            skipped,
            "{skipped} job(s) could not be bootstrapped and will not fire — see \
             job_bootstrap_failures_total"
        );
    }
    if registered_catch_ups > 0 {
        state.job_store.record_boot_catch_ups(registered_catch_ups);
        let span_secs = BOOT_CATCH_UP_LEAD_SECS
            + BOOT_CATCH_UP_SPACING_SECS * (registered_catch_ups.saturating_sub(1) as i64);
        tracing::warn!(
            cohort = registered_catch_ups,
            spacing_secs = BOOT_CATCH_UP_SPACING_SECS,
            span_secs,
            "Boot catch-up: {registered_catch_ups} job(s) were past due and will fire staggered \
             over ~{span_secs}s, most-overdue first (#1235)"
        );
    }
    Ok(())
}

/// How one persisted job should be re-registered at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootstrapFire {
    /// A future intent — fire at exactly this time, unstaggered.
    Scheduled(chrono::DateTime<chrono::Utc>),
    /// A missed tick. Carries the original due time so the catch-up cohort
    /// can be ordered most-overdue-first.
    CatchUp {
        due_at: chrono::DateTime<chrono::Utc>,
    },
}

/// Resolve how one persisted job re-enters the scheduler at startup.
///
/// `next_run_at` is the durable scheduler intent written before the in-memory
/// projection. It therefore wins for every schedule type, not only retries.
/// The cron expression is consulted only for legacy/new rows that have no
/// persisted intent yet.
fn bootstrap_fire_at(
    job: &alms_core::job::Job,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<BootstrapFire> {
    if job.status().is_terminal() {
        return None;
    }
    let due_at = job
        .next_run_at
        .or_else(|| cron_utils::schedule_fire_at(job, now))?;
    Some(if due_at <= now {
        BootstrapFire::CatchUp { due_at }
    } else {
        BootstrapFire::Scheduled(due_at)
    })
}

/// Assign staggered fire times to the boot-time catch-up cohort (#1235).
///
/// Ordered most-overdue-first so the longest-waiting job goes first, then
/// spaced by [`BOOT_CATCH_UP_SPACING_SECS`]. Ties break on `JobId` so the
/// plan is stable rather than dependent on `DashMap` iteration order.
fn plan_catch_up_cohort(
    mut cohort: Vec<(alms_core::JobId, chrono::DateTime<chrono::Utc>)>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(alms_core::JobId, chrono::DateTime<chrono::Utc>)> {
    cohort.sort_by(|(left_id, left_due), (right_id, right_due)| {
        left_due
            .cmp(right_due)
            .then_with(|| left_id.0.cmp(&right_id.0))
    });
    cohort
        .into_iter()
        .enumerate()
        .map(|(index, (job_id, _))| {
            let offset = BOOT_CATCH_UP_LEAD_SECS + BOOT_CATCH_UP_SPACING_SECS * (index as i64);
            (job_id, now + chrono::Duration::seconds(offset))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        BOOT_CATCH_UP_LEAD_SECS, BOOT_CATCH_UP_SPACING_SECS, BootstrapFire, bootstrap_fire_at,
        plan_catch_up_cohort,
    };
    use alms_core::{
        AgentId, JobId,
        job::{Job, JobSchedule, JobTransition},
    };
    use chrono::{TimeZone as _, Utc};

    fn recurring_job(next_run_at: Option<chrono::DateTime<Utc>>) -> Job {
        Job::new(
            AgentId::new(),
            "scheduled work".to_string(),
            JobSchedule::Recurring {
                cron: "0 * * * *".to_string(),
            },
            next_run_at,
        )
    }

    #[test]
    fn bootstrap_catches_up_past_due_persisted_recurring_intent() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 1, 0).unwrap();
        let persisted = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let job = recurring_job(Some(persisted));

        assert_eq!(
            bootstrap_fire_at(&job, now),
            Some(BootstrapFire::CatchUp { due_at: persisted })
        );
    }

    #[test]
    fn bootstrap_preserves_future_persisted_recurring_intent() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 1, 0).unwrap();
        let persisted = Utc.with_ymd_and_hms(2026, 7, 14, 11, 0, 0).unwrap();
        let job = recurring_job(Some(persisted));

        assert_eq!(
            bootstrap_fire_at(&job, now),
            Some(BootstrapFire::Scheduled(persisted))
        );
    }

    #[test]
    fn bootstrap_never_schedules_terminal_jobs() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 1, 0).unwrap();
        let mut job = recurring_job(Some(now + chrono::Duration::minutes(1)));
        assert!(job.transition(JobTransition::Cancel).is_applied());

        assert_eq!(bootstrap_fire_at(&job, now), None);
    }

    /// A past-due one-shot with no persisted intent (a legacy row) must still
    /// be classified as a missed tick. `cron_utils` no longer clamps it into
    /// the future, which used to hide the past-dueness from this decision.
    #[test]
    fn bootstrap_catches_up_past_due_one_shot_without_persisted_intent() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 1, 0).unwrap();
        let run_at = Utc.with_ymd_and_hms(2026, 7, 14, 9, 0, 0).unwrap();
        let job = Job::new(
            AgentId::new(),
            "legacy one-shot".to_string(),
            JobSchedule::Once { run_at },
            None,
        );

        assert_eq!(
            bootstrap_fire_at(&job, now),
            Some(BootstrapFire::CatchUp { due_at: run_at })
        );
    }

    /// #1235: N past-due jobs must not all fire in the same instant. Before
    /// the stagger, every one of them was scheduled at `now + 1s`, so a
    /// restart after long downtime produced a concurrent burst bounded only
    /// by the agent queue.
    #[test]
    fn boot_catch_up_cohort_is_staggered_not_simultaneous() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let cohort: Vec<_> = (1..=5)
            .map(|minutes_late| (JobId::new(), now - chrono::Duration::minutes(minutes_late)))
            .collect();

        let planned = plan_catch_up_cohort(cohort, now);

        assert_eq!(planned.len(), 5);
        let fire_times: Vec<_> = planned.iter().map(|(_, at)| *at).collect();
        let unique: std::collections::BTreeSet<_> = fire_times.iter().collect();
        assert_eq!(
            unique.len(),
            fire_times.len(),
            "no two catch-up jobs may share a fire instant"
        );
        for window in fire_times.windows(2) {
            assert_eq!(
                window[1] - window[0],
                chrono::Duration::seconds(BOOT_CATCH_UP_SPACING_SECS),
                "catch-up firings are evenly spaced"
            );
        }
        assert_eq!(
            fire_times[0],
            now + chrono::Duration::seconds(BOOT_CATCH_UP_LEAD_SECS),
            "the first catch-up keeps the pre-stagger head start"
        );
    }

    /// The longest-waiting job goes first, and the plan is stable regardless
    /// of the order the store happened to hand the jobs over in.
    #[test]
    fn boot_catch_up_cohort_fires_most_overdue_first() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        let oldest = (JobId::new(), now - chrono::Duration::hours(6));
        let middle = (JobId::new(), now - chrono::Duration::hours(2));
        let newest = (JobId::new(), now - chrono::Duration::minutes(5));

        let planned = plan_catch_up_cohort(vec![newest, oldest, middle], now);

        assert_eq!(
            planned.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![oldest.0, middle.0, newest.0]
        );
    }

    #[test]
    fn empty_catch_up_cohort_plans_nothing() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 10, 0, 0).unwrap();
        assert!(plan_catch_up_cohort(Vec::new(), now).is_empty());
    }
}
