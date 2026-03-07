//! Minimal async scheduler backed by `tokio::time`.
//!
//! Uses `tokio::time::sleep_until` exclusively so tests can freeze/advance the
//! simulated clock with `tokio::time::pause()` + `tokio::time::advance()`.
//!
//! # Usage
//! ```ignore
//! let scheduler = Scheduler::new();
//! let handle = scheduler.start();
//! scheduler.schedule_once("my-job", Instant::now() + Duration::from_secs(60)).await;
//! // ... later ...
//! let runs = scheduler.completed_runs().await;
//! ```

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::{self, Instant};
use tracing::info;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Opaque identifier for a scheduled job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(Uuid);

impl JobId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Record of a single job execution.
#[derive(Debug, Clone)]
pub struct JobRun {
    pub job_id: JobId,
    pub job_name: String,
    pub scheduled_at: Instant,
    pub ran_at: Instant,
}

// ---------------------------------------------------------------------------
// Internal job representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PendingJob {
    id: JobId,
    name: String,
    run_at: Instant,
    /// Some(d) = recurring; re-enqueued after each firing.
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
#[derive(Clone)]
pub struct Scheduler {
    pending: Arc<Mutex<BinaryHeap<Reverse<PendingJob>>>>,
    cancelled: Arc<Mutex<HashSet<JobId>>>,
    history: Arc<Mutex<Vec<JobRun>>>,
    /// Wakes the runner loop when a new job is added (or a cancel occurs).
    notify: Arc<Notify>,
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
        }
    }

    /// Schedule a one-shot job to run at `at`.
    pub async fn schedule_once(&self, name: impl Into<String>, at: Instant) -> JobId {
        let id = JobId::new();
        self.pending.lock().await.push(Reverse(PendingJob {
            id,
            name: name.into(),
            run_at: at,
            interval: None,
        }));
        self.notify.notify_one();
        id
    }

    /// Schedule a recurring job: first fires at `start`, then every `interval`.
    pub async fn schedule_recurring(
        &self,
        name: impl Into<String>,
        start: Instant,
        interval: Duration,
    ) -> JobId {
        let id = JobId::new();
        self.pending.lock().await.push(Reverse(PendingJob {
            id,
            name: name.into(),
            run_at: start,
            interval: Some(interval),
        }));
        self.notify.notify_one();
        id
    }

    /// Cancel future firings of a job. Runs already recorded are not removed.
    pub async fn cancel(&self, id: JobId) {
        self.cancelled.lock().await.insert(id);
        // Wake the runner so it skips this job if it's currently sleeping for it.
        self.notify.notify_one();
    }

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

    // ── Internal ─────────────────────────────────────────────────────────────

    async fn run_loop(&self) {
        loop {
            // Find the earliest pending (non-cancelled) job's run time.
            let next_at = {
                let pending = self.pending.lock().await;
                let cancelled = self.cancelled.lock().await;
                pending
                    .iter()
                    .find(|Reverse(j)| !cancelled.contains(&j.id))
                    .map(|Reverse(j)| j.run_at)
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

        // Re-enqueue recurring jobs (unless cancelled).
        for job in &due {
            if let Some(interval) = job.interval {
                if !cancelled.contains(&job.id) {
                    pending.push(Reverse(PendingJob {
                        id: job.id,
                        name: job.name.clone(),
                        run_at: job.run_at + interval,
                        interval: Some(interval),
                    }));
                }
            }
        }
        drop(pending);
        drop(cancelled);

        let ran_at = Instant::now();
        let mut history = self.history.lock().await;
        for job in due {
            info!(job_name = %job.name, job_id = %job.id, "Scheduled job fired");
            history.push(JobRun {
                job_id: job.id,
                job_name: job.name.clone(),
                scheduled_at: job.run_at,
                ran_at,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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

        let at = Instant::now() + Duration::from_secs(60);
        scheduler.schedule_once("test-job", at).await;

        advance(Duration::from_secs(61)).await;

        let runs = scheduler.completed_runs().await;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].job_name, "test-job");

        handle.abort();
    }

    #[tokio::test]
    async fn test_schedule_once_does_not_fire_before_time() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let at = Instant::now() + Duration::from_secs(60);
        scheduler.schedule_once("too-soon", at).await;

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

        let start = Instant::now() + Duration::from_secs(10);
        scheduler
            .schedule_recurring("ticker", start, Duration::from_secs(10))
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

        // All runs are named correctly.
        for run in scheduler.completed_runs().await {
            assert_eq!(run.job_name, "ticker");
        }

        handle.abort();
    }

    #[tokio::test]
    async fn test_cancel_stops_recurring_job() {
        tokio::time::pause();

        let scheduler = Scheduler::new();
        let handle = scheduler.start();

        let start = Instant::now() + Duration::from_secs(10);
        let id = scheduler
            .schedule_recurring("cancelable", start, Duration::from_secs(10))
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

        let now = Instant::now();
        scheduler.schedule_once("job-b", now + Duration::from_secs(20)).await;
        scheduler.schedule_once("job-a", now + Duration::from_secs(10)).await;

        advance(Duration::from_secs(25)).await;

        let runs = scheduler.completed_runs().await;
        assert_eq!(runs.len(), 2);
        // Both should have fired; order in history is firing order.
        let names: Vec<&str> = runs.iter().map(|r| r.job_name.as_str()).collect();
        assert!(names.contains(&"job-a"));
        assert!(names.contains(&"job-b"));

        handle.abort();
    }
}
