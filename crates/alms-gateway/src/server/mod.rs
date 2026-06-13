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

pub use run_manager::RunManager;
pub use state::{AppState, ServerLlmDefault};

use crate::auth::{AuthToken, no_cache, require_auth};
use crate::cron_utils;
use crate::gateway::Gateway;
use crate::runs::{
    completion_notification_loop, dm_event_loop, run_trigger_loop, scheduler_fire_loop,
};
use alms_core::{AlmsResult, JobStatus};
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

    // Bounded channels (#842 / B11): producers apply back-pressure instead
    // of growing without limit. `MessageBus` pushes with `send().await` so a
    // full buffer slows the producer rather than dropping a DM trigger.
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

/// Re-register all non-cancelled persisted jobs with the scheduler on startup.
async fn bootstrap_scheduler(state: &AppState) -> AlmsResult<()> {
    let now = chrono::Utc::now();
    let jobs = state.job_store.list();
    let mut registered = 0usize;

    for job in jobs {
        if job.status == JobStatus::Cancelled {
            continue;
        }
        let Some(fire_at) = cron_utils::compute_next_fire(&job, now) else {
            tracing::warn!("Job {} has no future fire time, skipping bootstrap", job.id);
            continue;
        };
        let delay = (fire_at - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        let instant = tokio::time::Instant::now() + delay;
        state.scheduler.schedule_once(job.id, instant).await;
        state.job_store.update_next_run_at(job.id, Some(fire_at))?;
        registered += 1;
    }

    if registered > 0 {
        info!("Bootstrapped {} job(s) into scheduler", registered);
    }
    Ok(())
}
