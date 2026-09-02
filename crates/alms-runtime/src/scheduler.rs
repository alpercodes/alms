// SPDX-License-Identifier: Apache-2.0

//! Minimal async scheduler backed by `tokio::time`.
//!
//! Uses `tokio::time::sleep_until` exclusively so tests can freeze/advance the
//! simulated clock with `tokio::time::pause()` + `tokio::time::advance()`.
//!
//! Jobs are identified by [`alms_core::JobId`] supplied by the caller.
//! When a job fires, its ID is sent on an optional mpsc channel so the gateway
//! can dispatch the actual agent run without blocking the scheduler loop.
//!
//! # Usage
//! ```ignore
//! let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
//! let scheduler = Scheduler::new().with_fire_channel(tx);
//! let handle = scheduler.start();
//! let id = alms_core::JobId::new();
//! scheduler.schedule_once(id, Instant::now() + Duration::from_secs(60)).await;
//! let fired_id = rx.recv().await.unwrap(); // == id
//! ```

use alms_core::JobId;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::time::{self, Instant};
use tracing::info;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Record of a single job execution (useful for testing / history).
#[derive(Debug, Clone)]
pub struct JobRun {
    pub job_id: JobId,
    pub scheduled_at: Instant,
    pub ran_at: Instant,
}

// ---------------------------------------------------------------------------
// Internal job representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PendingJob {
    id: JobId,
    run_at: Instant,
    /// Some(d) = interval-recurring; re-enqueued after each firing.
    interval: Option<Duration>,
}

// Ordered ascending by `run_at` so `BinaryHeap<Reverse<PendingJob>>` is a min-heap.
impl PartialEq for PendingJob {
    fn eq(&self, other: &Self) -> bool {
        self.run_at == other.run_at
    }
}
impl Eq for PendingJob {}
impl PartialOrd for PendingJob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PendingJob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.run_at.cmp(&other.run_at)
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Async scheduler. Clone is cheap (all fields are `Arc`).
///
/// Call [`start`] once to spawn the background runner, then use
/// [`schedule_once`] / [`schedule_recurring`] to add jobs.
#[derive(Clone, Debug)]
pub struct Scheduler {
    pending: Arc<Mutex<BinaryHeap<Reverse<PendingJob>>>>,
    cancelled: Arc<Mutex<HashSet<JobId>>>,
    history: Arc<Mutex<Vec<JobRun>>>,
    /// Wakes the runner loop when a new job is added (or a cancel occurs).
    notify: Arc<Notify>,
    /// Optional channel: fired job IDs are sent here for the gateway to dispatch.
    fire_tx: Option<Arc<mpsc::UnboundedSender<JobId>>>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(BinaryHeap::new())),
            cancelled: Arc::new(Mutex::new(HashSet::new())),
            history: Arc::new(Mutex::new(Vec::new())),
            notify: Arc::new(Notify::new()),
            fire_tx: None,
        }
    }

    /// Attach a fire channel. When a job fires, its `JobId` is sent on `tx`.
    pub fn with_fire_channel(mut self, tx: mpsc::UnboundedSender<JobId>) -> Self {
        self.fire_tx = Some(Arc::new(tx));
        self
    }

    /// Schedule a one-shot job with a caller-supplied ID to run at `at`.
    pub async fn schedule_once(&self, id: JobId, at: Instant) {
        self.pending.lock().await.push(Reverse(PendingJob {
            id,
            run_at: at,
            interval: None,
        }));
        self.notify.notify_one();
    }

    /// Schedule a recurring job with a caller-supplied ID: first fires at `start`,
    /// then every `interval`. For cron-based jobs prefer the one-shot pattern
    /// (re-arm after each firing with the next cron time).
    pub async fn schedule_recurring(&self, id: JobId, start: Instant, interval: Duration) {
        self.pending.lock().await.push(Reverse(PendingJob {
            id,
            run_at: start,
            interval: Some(interval),
        }));
        self.notify.notify_one();
    }

    /// Cancel future firings of a job. Already-recorded runs are not removed.
    pub async fn cancel(&self, id: JobId) {
        self.cancelled.lock().await.insert(id);
        // Wake the runner so it skips this job if it's currently sleeping for it.
        self.notify.notify_one();
    }

    // TODO(dead-code): test-only — can't cleanly gate because JobRun/history are woven into production struct
    /// Return a snapshot of all completed job runs in chronological order.
    pub async fn completed_runs(&self) -> Vec<JobRun> {
        self.history.lock().await.clone()
    }

    /// Number of jobs currently waiting to fire.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Spawn the background runner loop. Abort the returned handle to stop it.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let s = self.clone();
        tokio::spawn(async move { s.run_loop().await })
    }

    /// Spawn the background runner loop, stopping when `token` is cancelled.
    pub fn start_with_shutdown(
        &self,
        token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let s = self.clone();
        tokio::spawn(async move { s.run_loop_cancellable(token).await })
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    async fn run_loop(&self) {
        loop {
            // Find the earliest pending (non-cancelled) job's run time.
            // Drain cancelled entries from the heap top so peek() returns
            // the true earliest non-cancelled job.
            let next_at = {
                let mut pending = self.pending.lock().await;
                let cancelled = self.cancelled.lock().await;
                while let Some(Reverse(top)) = pending.peek() {
                    if cancelled.contains(&top.id) {
                        pending.pop();
                    } else {
                        break;
                    }
                }
                pending.peek().map(|Reverse(j)| j.run_at)
            };

            match next_at {
                None => {
                    // Nothing pending — wait for a new job to be scheduled.
                    self.notify.notified().await;
                }
                Some(run_at) => {
                    // Sleep until the earliest job is due, or until notified
                    // (e.g. a new earlier job was added, or a job was cancelled).
                    tokio::select! {
                        _ = time::sleep_until(run_at) => {}
                        _ = self.notify.notified() => {
                            // Re-evaluate — don't process yet.
                            continue;
                        }
                    }

                    self.process_due_jobs().await;
                }
            }
        }
    }

    async fn run_loop_cancellable(&self, token: tokio_util::sync::CancellationToken) {
        loop {
            let next_at = {
                let mut pending = self.pending.lock().await;
                let cancelled = self.cancelled.lock().await;
                while let Some(Reverse(top)) = pending.peek() {
                    if cancelled.contains(&top.id) {
                        pending.pop();
                    } else {
                        break;
                    }
                }
                pending.peek().map(|Reverse(j)| j.run_at)
            };

            match next_at {
                None => {
                    tokio::select! {
                        _ = self.notify.notified() => {}
                        _ = token.cancelled() => {
                            info!("Scheduler shutting down");
                            return;
                        }
                    }
                }
                Some(run_at) => {
                    tokio::select! {
                        _ = time::sleep_until(run_at) => {}
                        _ = self.notify.notified() => { continue; }
                        _ = token.cancelled() => {
                            info!("Scheduler shutting down");
                            return;
                        }
                    }
                    self.process_due_jobs().await;
                }
            }
        }
    }

    async fn process_due_jobs(&self) {
        let now = Instant::now();
        let cancelled = self.cancelled.lock().await;
        let mut pending = self.pending.lock().await;

        let mut due = Vec::new();
        // Pop all jobs whose run_at has passed.
        while let Some(Reverse(job)) = pending.peek() {
            if job.run_at <= now {
                let Reverse(job) = pending.pop().unwrap();
                due.push(job);
            } else {
                break;
            }
        }

        // Re-enqueue interval-recurring jobs (unless cancelled).
        for job in &due {
            if let Some(interval) = job.interval
                && !cancelled.contains(&job.id)
            {
                pending.push(Reverse(PendingJob {
                    id: job.id,
                    run_at: job.run_at + interval,
                    interval: Some(interval),
                }));
            }
        }
        drop(pending);
        // Keep `cancelled` alive through the firing loop so we can filter.

        let ran_at = Instant::now();
        let mut history = self.history.lock().await;
        for job in due {
            if cancelled.contains(&job.id) {
                continue;
            }
            info!(job_id = %job.id, "Scheduled job fired");
            history.push(JobRun {
                job_id: job.id,
                scheduled_at: job.run_at,
                ran_at,
            });
            if let Some(ref tx) = self.fire_tx {
                let _ = tx.send(job.id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::JobId;
    use std::time::Duration;
    use tokio::time::Instant;

    /// Helper: let spawned tasks reach their first await point, advance time,
    /// then let woken tasks run to completion.
    async fn advance(dur: Duration) {
        tokio::task::yield_now().await;
        tokio::time::advance(dur).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn test_schedule_once_fires_after_advance() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id = JobId::new();
        let at = Instant::now() + Duration::from_secs(60);
        scheduler.schedule_once(id, at).await;

        advance(Duration::from_secs(61)).await;

        let runs = scheduler.completed_runs().await;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].job_id, id);

        handle.abort();
    }

    #[tokio::test]
    async fn test_schedule_once_does_not_fire_before_time() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id = JobId::new();
        let at = Instant::now() + Duration::from_secs(60);
        scheduler.schedule_once(id, at).await;

        // Advance only 30 s — job should not have fired.
        advance(Duration::from_secs(30)).await;

        let runs = scheduler.completed_runs().await;
        assert_eq!(runs.len(), 0, "job must not fire before its scheduled time");

        handle.abort();
    }

    #[tokio::test]
    async fn test_schedule_recurring_fires_multiple_times() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id = JobId::new();
        let start = Instant::now() + Duration::from_secs(10);
        scheduler
            .schedule_recurring(id, start, Duration::from_secs(10))
            .await;

        // First firing.
        advance(Duration::from_secs(11)).await;
        assert_eq!(scheduler.completed_runs().await.len(), 1);

        // Second firing.
        advance(Duration::from_secs(10)).await;
        assert_eq!(scheduler.completed_runs().await.len(), 2);

        // Third firing.
        advance(Duration::from_secs(10)).await;
        assert_eq!(scheduler.completed_runs().await.len(), 3);

        // All runs reference the same job ID.
        for run in scheduler.completed_runs().await {
            assert_eq!(run.job_id, id);
        }

        handle.abort();
    }

    #[tokio::test]
    async fn test_cancel_stops_recurring_job() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id = JobId::new();
        let start = Instant::now() + Duration::from_secs(10);
        scheduler
            .schedule_recurring(id, start, Duration::from_secs(10))
            .await;

        // Let it fire once.
        advance(Duration::from_secs(11)).await;
        assert_eq!(scheduler.completed_runs().await.len(), 1);

        // Cancel before next firing.
        scheduler.cancel(id).await;

        // Advance past where the next firing would have been.
        advance(Duration::from_secs(10)).await;

        // Still only 1 run recorded.
        assert_eq!(
            scheduler.completed_runs().await.len(),
            1,
            "cancelled job must not fire again"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn test_multiple_one_shot_jobs_in_order() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id_a = JobId::new();
        let id_b = JobId::new();
        let now = Instant::now();
        scheduler
            .schedule_once(id_b, now + Duration::from_secs(20))
            .await;
        scheduler
            .schedule_once(id_a, now + Duration::from_secs(10))
            .await;

        advance(Duration::from_secs(25)).await;

        let runs = scheduler.completed_runs().await;
        assert_eq!(runs.len(), 2);
        let ids: Vec<JobId> = runs.iter().map(|r| r.job_id).collect();
        assert!(ids.contains(&id_a));
        assert!(ids.contains(&id_b));

        handle.abort();
    }

    #[tokio::test]
    async fn test_cancelled_head_does_not_delay_other_jobs() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();
        let now = Instant::now();

        let id_early = JobId::new(); // will be cancelled
        let id_middle = JobId::new(); // should fire at T+50
        let id_late = JobId::new(); // fires at T+100

        // Insert in order that may place id_late before id_middle in the
        // heap's backing array (children of root are unordered).
        scheduler
            .schedule_once(id_early, now + Duration::from_secs(10))
            .await;
        scheduler
            .schedule_once(id_late, now + Duration::from_secs(100))
            .await;
        scheduler
            .schedule_once(id_middle, now + Duration::from_secs(50))
            .await;

        scheduler.cancel(id_early).await;

        // Advance to T+51 — id_middle MUST have fired.
        advance(Duration::from_secs(51)).await;

        let runs = scheduler.completed_runs().await;
        let fired_ids: Vec<JobId> = runs.iter().map(|r| r.job_id).collect();
        assert!(
            fired_ids.contains(&id_middle),
            "middle job must fire on time"
        );
        assert!(
            !fired_ids.contains(&id_early),
            "cancelled job must not fire"
        );
        assert!(!fired_ids.contains(&id_late), "late job must not fire yet");

        handle.abort();
    }

    #[tokio::test]
    async fn test_cancelled_job_not_sent_on_fire_channel() {
        tokio::time::pause();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let scheduler = Scheduler::new().with_fire_channel(tx);
        let handle = scheduler.start();
        let now = Instant::now();

        let id_a = JobId::new();
        let id_b = JobId::new();

        scheduler
            .schedule_once(id_a, now + Duration::from_secs(10))
            .await;
        scheduler
            .schedule_once(id_b, now + Duration::from_secs(10))
            .await;

        scheduler.cancel(id_a).await;

        advance(Duration::from_secs(11)).await;

        let fired = rx.try_recv().expect("should receive id_b");
        assert_eq!(fired, id_b);
        assert!(
            rx.try_recv().is_err(),
            "cancelled job must not appear on fire channel"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn test_all_cancelled_no_spurious_firing() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let id = JobId::new();
        scheduler
            .schedule_once(id, Instant::now() + Duration::from_secs(10))
            .await;
        scheduler.cancel(id).await;

        advance(Duration::from_secs(20)).await;

        assert_eq!(
            scheduler.completed_runs().await.len(),
            0,
            "cancelled job must not fire"
        );
        assert_eq!(
            scheduler.pending_count().await,
            0,
            "cancelled job should be drained from heap"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn test_shutdown_stops_scheduler() {
        tokio::time::pause();

        let token = tokio_util::sync::CancellationToken::new();
        let scheduler = Scheduler::new();
        let handle = scheduler.start_with_shutdown(token.clone());

        let id = JobId::new();
        scheduler
            .schedule_once(id, Instant::now() + Duration::from_secs(60))
            .await;

        // Cancel the token — scheduler should exit
        token.cancel();

        advance(Duration::from_millis(10)).await;

        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(
            result.is_ok(),
            "scheduler should exit after token cancellation"
        );

        // Job should NOT have fired (it was scheduled 60s out)
        assert_eq!(scheduler.completed_runs().await.len(), 0);
    }

    #[tokio::test]
    async fn test_fire_channel_receives_fired_ids() {
        tokio::time::pause();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let scheduler = Scheduler::new().with_fire_channel(tx);
        let handle = scheduler.start();

        let id = JobId::new();
        scheduler
            .schedule_once(id, Instant::now() + Duration::from_secs(5))
            .await;

        advance(Duration::from_secs(6)).await;

        let fired = rx.try_recv().expect("should have received fired job id");
        assert_eq!(fired, id);

        handle.abort();
    }
}
