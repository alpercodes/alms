// SPDX-License-Identifier: Apache-2.0

//! In-memory job store with optional SQLite write-through.

use crate::sqlite::SqliteStore;
use alms_core::TransitionOutcome;
use alms_core::job::{
    CreateJobRequest, Job, JobId, JobSchedule, JobStatus, JobTerminalReason, JobTransition,
};
use alms_core::{AlmsError, AlmsResult, MAX_LIFECYCLE_REVISION};
use dashmap::DashMap;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::AtomicU32;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tracing::info;

/// Outcome of [`JobStore::record_run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRunOutcome {
    /// The run was recorded and the job updated (and persisted, when a
    /// SQLite store is attached).
    Recorded,
    /// The job is `Cancelled` — the write was refused and nothing was
    /// mutated. `Cancelled` is absorbing: a later `record_run` must never
    /// transition a job back out of it (#1202 S1 — a `DELETE /jobs` racing
    /// the episode-close fanout would otherwise resurrect and re-arm a
    /// cancelled recurring job).
    RefusedTerminal,
    /// No job with this id exists (e.g. deleted mid-flight).
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchFailureOutcome {
    RetryScheduled {
        attempt: u32,
        retry_at: chrono::DateTime<chrono::Utc>,
    },
    Exhausted {
        attempts: u32,
    },
    RefusedTerminal,
    NotFound,
}
/// Manages scheduled jobs in memory, with optional SQLite write-through.
///
/// On startup, the complete persisted job history is loaded from SQLite —
/// terminal rows included — so cancelled, completed, and retry-exhausted jobs
/// stay observable after a restart instead of silently disappearing from
/// `GET /jobs`. `bootstrap_scheduler` re-registers only the non-terminal ones,
/// so loading history never resurrects a finished outcome.
#[derive(Debug, Clone)]
pub struct JobStore {
    jobs: Arc<DashMap<JobId, Job>>,
    store: Option<Arc<SqliteStore>>,
    retry_attempts: Arc<AtomicU64>,
    retry_exhaustions: Arc<AtomicU64>,
    rearm_failures: Arc<AtomicU64>,
    boot_catch_ups: Arc<AtomicU64>,
    bootstrap_failures: Arc<AtomicU64>,
    #[cfg(any(test, feature = "test-support"))]
    fail_next_persistence: Arc<AtomicU32>,
}

impl Default for JobStore {
    fn default() -> Self {
        Self::new()
    }
}

impl JobStore {
    /// Create an in-memory-only store (no persistence).
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(DashMap::new()),
            store: None,
            retry_attempts: Arc::new(AtomicU64::new(0)),
            retry_exhaustions: Arc::new(AtomicU64::new(0)),
            rearm_failures: Arc::new(AtomicU64::new(0)),
            boot_catch_ups: Arc::new(AtomicU64::new(0)),
            bootstrap_failures: Arc::new(AtomicU64::new(0)),
            #[cfg(any(test, feature = "test-support"))]
            fail_next_persistence: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn retry_attempts_total(&self) -> u64 {
        self.retry_attempts.load(Ordering::Relaxed)
    }

    pub fn retry_exhaustions_total(&self) -> u64 {
        self.retry_exhaustions.load(Ordering::Relaxed)
    }

    /// Episode-close writes that exhausted their retry budget (#1233).
    ///
    /// Non-zero means at least one job advanced its schedule in memory only:
    /// the durable `next_run_at` / `last_run_at` for that job is stale until
    /// its next successful run, and a restart replays the recorded tick.
    pub fn rearm_failures_total(&self) -> u64 {
        self.rearm_failures.load(Ordering::Relaxed)
    }

    /// Record an episode-close write that exhausted its retry budget (#1233).
    pub fn record_rearm_failure(&self) {
        self.rearm_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Jobs whose persisted fire time was already past due at boot and were
    /// therefore staggered into the catch-up cohort (#1235). Counts jobs that
    /// were actually registered, not merely planned.
    pub fn boot_catch_ups_total(&self) -> u64 {
        self.boot_catch_ups.load(Ordering::Relaxed)
    }

    /// Jobs that could not be re-registered with the scheduler at startup and
    /// were skipped so the daemon could still start (#1236 applied to jobs).
    ///
    /// Non-zero means those jobs will not fire until repaired or recreated.
    pub fn bootstrap_failures_total(&self) -> u64 {
        self.bootstrap_failures.load(Ordering::Relaxed)
    }

    /// Record a job that could not be bootstrapped into the scheduler.
    pub fn record_bootstrap_failure(&self) {
        self.bootstrap_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the size of the boot-time catch-up cohort (#1235).
    pub fn record_boot_catch_ups(&self, count: u64) {
        self.boot_catch_ups.fetch_add(count, Ordering::Relaxed);
    }

    /// Create a SQLite-backed store and reload the complete job history.
    pub fn with_sqlite(db_path: &str) -> AlmsResult<Self> {
        let store = SqliteStore::open(db_path)?;
        Self::with_store(Arc::new(store))
    }

    /// Create a store backed by the gateway's shared SQLite connection.
    pub fn with_store(store: Arc<SqliteStore>) -> AlmsResult<Self> {
        let mut s = Self::new();
        s.store = Some(store);
        s.load_from_store()?;
        Ok(s)
    }

    fn load_from_store(&self) -> AlmsResult<()> {
        let Some(ref store) = self.store else {
            return Ok(());
        };
        let jobs = store.load_all_jobs_unfiltered()?;
        let count = jobs.len();
        for job in jobs {
            self.jobs.insert(job.id, job);
        }
        if count > 0 {
            info!("Loaded {} job(s) from SQLite", count);
        }
        Ok(())
    }

    /// Create a new job and persist it.
    pub fn create(&self, req: CreateJobRequest) -> AlmsResult<Job> {
        let next_run_at = match &req.schedule {
            JobSchedule::Once { run_at } => Some(*run_at),
            JobSchedule::Recurring { .. } => None,
        };
        self.create_with_next_run_at(req, next_run_at)
    }

    /// Create and persist a job with its authoritative first fire time in one write.
    pub fn create_with_next_run_at(
        &self,
        req: CreateJobRequest,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> AlmsResult<Job> {
        let job = Job::new(req.agent_id, req.prompt, req.schedule, next_run_at);

        if let Some(ref store) = self.store {
            store.save_job(&job)?;
        }

        self.jobs.insert(job.id, job.clone());
        info!("Created job {}", job.id.0);
        Ok(job)
    }

    /// List all jobs (pending, active, and recently-cancelled in this session),
    /// newest first.
    pub fn list(&self) -> Vec<Job> {
        let mut jobs: Vec<Job> = self.jobs.iter().map(|e| e.value().clone()).collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.created_at));
        jobs
    }

    /// Get a single job by ID.
    pub fn get(&self, id: JobId) -> Option<Job> {
        self.jobs.get(&id).map(|e| e.value().clone())
    }

    /// Update the next scheduled fire time for a job (used after bootstrap and create).
    pub fn update_next_run_at(
        &self,
        id: JobId,
        next: Option<chrono::DateTime<chrono::Utc>>,
    ) -> AlmsResult<()> {
        let _ = self.transition_job(id, JobTransition::SetNextRunAt(next))?;
        Ok(())
    }

    /// Record that a job fired: update last_run_at, status, and next_run_at atomically.
    ///
    /// **`Cancelled` is absorbing (#1202 S1):** if the job is already
    /// `Cancelled`, the write is refused — nothing is mutated and
    /// [`RecordRunOutcome::RefusedTerminal`] is returned. This closes the
    /// cancel-vs-close TOCTOU at the persistence layer: the gateway's
    /// `close_episode` reads the job's status, `await`s the completion
    /// fanout, and only then records the run — a `DELETE /jobs` landing in
    /// that window must not be overwritten back to `Active` (a cancelled
    /// recurring job would resurrect and re-arm). The status check and the
    /// mutation happen under one continuous DashMap entry guard, and
    /// [`Self::cancel`] mutates under the same entry lock, so no interleave
    /// can slip between check and write regardless of caller timing.
    ///
    /// Callers gate their re-arm side effects on
    /// [`RecordRunOutcome::Recorded`].
    pub fn record_run(
        &self,
        id: JobId,
        ran_at: chrono::DateTime<chrono::Utc>,
        new_status: JobStatus,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> AlmsResult<RecordRunOutcome> {
        let terminal_reason =
            (new_status == JobStatus::Completed).then_some(JobTerminalReason::Completed);
        self.record_run_with_reason(id, ran_at, new_status, next_run_at, terminal_reason)
    }

    /// Record a firing with an explicit terminal reason for a one-shot job.
    pub fn record_run_with_reason(
        &self,
        id: JobId,
        ran_at: chrono::DateTime<chrono::Utc>,
        new_status: JobStatus,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        terminal_reason: Option<JobTerminalReason>,
    ) -> AlmsResult<RecordRunOutcome> {
        let transition = match new_status {
            JobStatus::Active => JobTransition::RecordRecurringRun {
                ran_at,
                next_run_at,
            },
            JobStatus::Completed => JobTransition::CompleteOneShot {
                ran_at,
                terminal_reason: terminal_reason.unwrap_or(JobTerminalReason::Completed),
            },
            JobStatus::Pending | JobStatus::Failed | JobStatus::Cancelled => {
                return Err(AlmsError::InvalidConfig(format!(
                    "record_run cannot transition a job to {new_status:?}"
                )));
            }
        };
        match self.transition_job(id, transition)? {
            None => Ok(RecordRunOutcome::NotFound),
            Some(TransitionOutcome::Applied { .. }) => {
                info!("Job {} run recorded (status={:?})", id.0, new_status);
                Ok(RecordRunOutcome::Recorded)
            }
            Some(TransitionOutcome::NoOp { .. } | TransitionOutcome::Rejected { .. }) => {
                info!("Job {} is terminal — record_run refused", id.0);
                Ok(RecordRunOutcome::RefusedTerminal)
            }
        }
    }

    /// Persist a recoverable dispatch failure and either schedule the next
    /// bounded retry or move the job to the authoritative failed state.
    pub fn record_dispatch_failure(
        &self,
        id: JobId,
        error: impl Into<String>,
        retry_at: chrono::DateTime<chrono::Utc>,
        max_attempts: u32,
    ) -> AlmsResult<DispatchFailureOutcome> {
        match self.transition_job(
            id,
            JobTransition::RecordDispatchFailure {
                error: error.into(),
                retry_at,
                max_attempts,
            },
        )? {
            None => Ok(DispatchFailureOutcome::NotFound),
            Some(TransitionOutcome::Applied {
                to: JobStatus::Failed,
                ..
            }) => {
                self.retry_attempts.fetch_add(1, Ordering::Relaxed);
                self.retry_exhaustions.fetch_add(1, Ordering::Relaxed);
                let attempts = self.get(id).map_or(max_attempts, |job| job.retry_count());
                Ok(DispatchFailureOutcome::Exhausted { attempts })
            }
            Some(TransitionOutcome::Applied { .. }) => {
                self.retry_attempts.fetch_add(1, Ordering::Relaxed);
                let attempt = self.get(id).map_or(1, |job| job.retry_count());
                Ok(DispatchFailureOutcome::RetryScheduled { attempt, retry_at })
            }
            Some(TransitionOutcome::NoOp { .. } | TransitionOutcome::Rejected { .. }) => {
                Ok(DispatchFailureOutcome::RefusedTerminal)
            }
        }
    }
    /// Cancel a job.
    /// Returns:
    /// - `Ok(Some(true))` — job was found and cancelled
    /// - `Ok(Some(false))` — job exists but was already cancelled
    /// - `Ok(None)` — job not found
    pub fn cancel(&self, id: JobId) -> AlmsResult<Option<bool>> {
        match self.transition_job(id, JobTransition::Cancel)? {
            None => Ok(None),
            Some(TransitionOutcome::Applied { .. }) => {
                info!("Cancelled job {}", id.0);
                Ok(Some(true))
            }
            Some(TransitionOutcome::NoOp { .. } | TransitionOutcome::Rejected { .. }) => {
                Ok(Some(false))
            }
        }
    }

    fn transition_job(
        &self,
        id: JobId,
        transition: JobTransition,
    ) -> AlmsResult<Option<TransitionOutcome<JobStatus>>> {
        let Some(mut entry) = self.jobs.get_mut(&id) else {
            return Ok(None);
        };
        let mut candidate = entry.clone();
        let outcome = candidate.transition(transition);
        if matches!(outcome, TransitionOutcome::Rejected { .. })
            && candidate.lifecycle_revision() >= MAX_LIFECYCLE_REVISION
        {
            return Err(AlmsError::Runtime(format!(
                "job {} lifecycle revision is exhausted",
                id.0
            )));
        }
        if outcome.is_applied() {
            #[cfg(any(test, feature = "test-support"))]
            if self.take_injected_persistence_failure() {
                return Err(AlmsError::Runtime(
                    "injected job persistence failure".to_string(),
                ));
            }
            if let Some(store) = &self.store {
                store.save_job(&candidate)?;
            }
            *entry = candidate;
        }
        drop(entry);
        Ok(Some(outcome))
    }

    /// Test hook: fail the durable write of the next applied transition.
    ///
    /// Neither memory nor SQLite advances on that failure, which is the exact
    /// shape a transient SQLite error produces at episode close (#1233).
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject_next_persistence_failure(&self) {
        self.inject_persistence_failures(1);
    }

    /// Test hook: fail the durable write of the next `count` applied
    /// transitions, so a bounded retry budget can be driven to exhaustion.
    #[cfg(any(test, feature = "test-support"))]
    pub fn inject_persistence_failures(&self, count: u32) {
        self.fail_next_persistence.store(count, Ordering::Release);
    }

    /// Consume one injected failure, if any remain.
    ///
    /// A `compare_exchange_weak` countdown rather than `fetch_update`: the
    /// latter is deprecated on current nightly (renamed to the still-unstable
    /// `try_update`), and this block only compiles under `cfg(test)` or the
    /// `test-support` feature, so it is precisely the code an unpinned
    /// toolchain bump can break without a plain `cargo clippy` noticing.
    #[cfg(any(test, feature = "test-support"))]
    fn take_injected_persistence_failure(&self) -> bool {
        let mut remaining = self.fail_next_persistence.load(Ordering::Acquire);
        loop {
            if remaining == 0 {
                return false;
            }
            match self.fail_next_persistence.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => remaining = observed,
            }
        }
    }

    /// Flush the SQLite WAL to disk. No-op if no SQLite store is attached.
    pub fn flush_wal(&self) -> AlmsResult<()> {
        if let Some(store) = &self.store {
            store.flush_wal()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, job::JobSchedule};
    use chrono::Utc;

    fn once_req() -> CreateJobRequest {
        CreateJobRequest {
            agent_id: AgentId::new(),
            prompt: "say hello".to_string(),
            schedule: JobSchedule::Once {
                run_at: Utc::now() + chrono::Duration::hours(1),
            },
        }
    }

    fn recurring_req() -> CreateJobRequest {
        CreateJobRequest {
            agent_id: AgentId::new(),
            prompt: "daily briefing".to_string(),
            schedule: JobSchedule::Recurring {
                cron: "0 9 * * 1-5".to_string(),
            },
        }
    }

    #[test]
    fn test_create_and_list() {
        let store = JobStore::new();
        let job = store.create(once_req()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].id, job.id);
    }

    #[test]
    fn test_once_job_has_next_run_at() {
        let store = JobStore::new();
        let job = store.create(once_req()).unwrap();
        assert!(job.next_run_at.is_some());
    }

    #[test]
    fn test_recurring_job_has_no_next_run_at() {
        let store = JobStore::new();
        let job = store.create(recurring_req()).unwrap();
        assert!(job.next_run_at.is_none());
    }

    #[test]
    fn test_cancel() {
        let store = JobStore::new();
        let job = store.create(once_req()).unwrap();
        assert_eq!(store.cancel(job.id).unwrap(), Some(true));
        // second cancel returns Some(false) — already cancelled
        assert_eq!(store.cancel(job.id).unwrap(), Some(false));
        // still in list with Cancelled status
        let listed = store.list();
        assert_eq!(listed[0].status(), JobStatus::Cancelled);
    }

    #[test]
    fn test_cancel_unknown_returns_none() {
        let store = JobStore::new();
        assert_eq!(store.cancel(JobId::new()).unwrap(), None);
    }

    #[test]
    fn revision_exhaustion_is_reported_instead_of_already_cancelled() {
        let store = JobStore::new();
        let job = Job::from_persisted(
            JobId::new(),
            AgentId::new(),
            "exhausted".to_string(),
            JobSchedule::Recurring {
                cron: "* * * * *".to_string(),
            },
            JobStatus::Pending,
            Utc::now(),
            None,
            None,
            MAX_LIFECYCLE_REVISION,
            None,
            0,
            None,
        );
        let job_id = job.id;
        store.jobs.insert(job_id, job);

        let error = store.cancel(job_id).unwrap_err().to_string();
        assert!(error.contains("revision is exhausted"));
        assert_eq!(store.get(job_id).unwrap().status(), JobStatus::Pending);
    }

    #[test]
    fn failed_cancel_persistence_does_not_commit_or_resurrect_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        let path = path.to_string_lossy().into_owned();
        let store = JobStore::with_sqlite(&path).unwrap();
        let job = store.create(recurring_req()).unwrap();
        let next = Utc::now() + chrono::Duration::minutes(1);
        assert_eq!(
            store
                .record_run(job.id, Utc::now(), JobStatus::Active, Some(next))
                .unwrap(),
            RecordRunOutcome::Recorded
        );

        store.inject_next_persistence_failure();
        assert!(store.cancel(job.id).is_err());
        assert_eq!(store.get(job.id).unwrap().status(), JobStatus::Active);
        drop(store);

        let restarted = JobStore::with_sqlite(&path).unwrap();
        let job = restarted
            .get(job.id)
            .expect("durable active job should load after restart");
        assert_eq!(job.status(), JobStatus::Active);
        assert_eq!(job.next_run_at, Some(next));
        assert_eq!(job.terminal_reason(), None);
    }

    #[test]
    fn job_completion_and_operator_cancellation_are_first_writer_wins() {
        let store = JobStore::new();
        let completed_first = store.create(once_req()).unwrap();
        assert_eq!(
            store
                .record_run_with_reason(
                    completed_first.id,
                    Utc::now(),
                    JobStatus::Completed,
                    None,
                    Some(JobTerminalReason::Completed),
                )
                .unwrap(),
            RecordRunOutcome::Recorded
        );
        assert_eq!(store.cancel(completed_first.id).unwrap(), Some(false));
        assert_eq!(
            store.get(completed_first.id).unwrap().terminal_reason(),
            Some(JobTerminalReason::Completed)
        );

        let cancelled_first = store.create(once_req()).unwrap();
        assert_eq!(store.cancel(cancelled_first.id).unwrap(), Some(true));
        assert_eq!(
            store
                .record_run_with_reason(
                    cancelled_first.id,
                    Utc::now(),
                    JobStatus::Completed,
                    None,
                    Some(JobTerminalReason::Completed),
                )
                .unwrap(),
            RecordRunOutcome::RefusedTerminal
        );
        assert_eq!(
            store.get(cancelled_first.id).unwrap().terminal_reason(),
            Some(JobTerminalReason::OperatorCancelled)
        );
    }

    /// #1202 S1: `Cancelled` is absorbing. A `record_run` landing after a
    /// cancel — the exact write the cancel-vs-episode-close TOCTOU produces
    /// when `DELETE /jobs` slips into `close_episode`'s fanout window —
    /// must be refused with nothing mutated: the job stays `Cancelled`,
    /// `next_run_at` stays cleared, `last_run_at` stays untouched. Pre-fix
    /// this overwrote `Cancelled -> Active` and re-set `next_run_at`,
    /// resurrecting a cancelled recurring job.
    #[test]
    fn test_record_run_refuses_to_resurrect_cancelled_job() {
        let store = JobStore::new();
        let job = store.create(recurring_req()).unwrap();
        assert_eq!(store.cancel(job.id).unwrap(), Some(true));

        let outcome = store
            .record_run(
                job.id,
                Utc::now(),
                JobStatus::Active,
                Some(Utc::now() + chrono::Duration::minutes(1)),
            )
            .unwrap();
        assert_eq!(
            outcome,
            RecordRunOutcome::RefusedTerminal,
            "record_run on a cancelled job must be refused"
        );

        let job = store.get(job.id).unwrap();
        assert_eq!(
            job.status(),
            JobStatus::Cancelled,
            "a cancelled job must never transition back to Active"
        );
        assert_eq!(
            job.next_run_at, None,
            "the refused write must not re-set next_run_at"
        );
        assert_eq!(
            job.last_run_at, None,
            "the refused write must not touch last_run_at"
        );
    }

    /// The normal path still records: status/next_run_at/last_run_at all
    /// update and the outcome is `Recorded`.
    #[test]
    fn test_record_run_records_on_active_job() {
        let store = JobStore::new();
        let job = store.create(recurring_req()).unwrap();
        let next = Utc::now() + chrono::Duration::minutes(1);

        let outcome = store
            .record_run(job.id, Utc::now(), JobStatus::Active, Some(next))
            .unwrap();
        assert_eq!(outcome, RecordRunOutcome::Recorded);

        let job = store.get(job.id).unwrap();
        assert_eq!(job.status(), JobStatus::Active);
        assert_eq!(job.next_run_at, Some(next));
        assert!(job.last_run_at.is_some());
    }

    /// Unknown job id reports `NotFound` (the pre-S1 `None` shape).
    #[test]
    fn test_record_run_unknown_job_is_not_found() {
        let store = JobStore::new();
        let outcome = store
            .record_run(JobId::new(), Utc::now(), JobStatus::Active, None)
            .unwrap();
        assert_eq!(outcome, RecordRunOutcome::NotFound);
    }

    #[test]
    fn test_sqlite_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let req = once_req();
        let expected_agent_id = req.agent_id;

        let schedule_json = serde_json::to_string(&req.schedule).unwrap();
        let job = Job::new(
            req.agent_id,
            req.prompt,
            req.schedule,
            Some(Utc::now() + chrono::Duration::hours(1)),
        );
        store.save_job(&job).unwrap();

        let jobs = store.load_all_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job.id);
        assert_eq!(jobs[0].agent_id, expected_agent_id);
        assert!(jobs[0].next_run_at.is_some());

        // Verify schedule round-trips
        let loaded_json = serde_json::to_string(&jobs[0].schedule).unwrap();
        assert_eq!(loaded_json, schedule_json);
    }

    #[test]
    fn equal_revision_job_snapshot_is_an_idempotent_noop() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut authoritative = Job::new(
            AgentId::new(),
            "once".to_string(),
            JobSchedule::Once { run_at: Utc::now() },
            Some(Utc::now()),
        );
        assert!(
            authoritative
                .transition(JobTransition::CompleteOneShot {
                    ran_at: Utc::now(),
                    terminal_reason: JobTerminalReason::Completed,
                })
                .is_applied()
        );
        let conflicting = Job::from_persisted(
            authoritative.id,
            authoritative.agent_id,
            "delayed conflicting payload".to_string(),
            authoritative.schedule.clone(),
            authoritative.status(),
            authoritative.created_at,
            authoritative.next_run_at,
            authoritative.last_run_at,
            authoritative.lifecycle_revision(),
            Some(JobTerminalReason::OperatorCancelled),
            0,
            None,
        );

        store.save_job(&authoritative).unwrap();
        store.save_job(&conflicting).unwrap();

        let loaded = store
            .load_job_by_id(authoritative.id)
            .unwrap()
            .expect("job should remain persisted");
        assert_eq!(loaded.prompt, "once");
        assert_eq!(loaded.terminal_reason(), Some(JobTerminalReason::Completed));
    }

    #[test]
    fn sqlite_refuses_stale_job_snapshot_after_terminal_transition() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut job = Job::new(
            AgentId::new(),
            "x".to_string(),
            JobSchedule::Recurring {
                cron: "* * * * *".to_string(),
            },
            Some(Utc::now()),
        );
        assert!(
            job.transition(JobTransition::RecordRecurringRun {
                ran_at: Utc::now(),
                next_run_at: None,
            })
            .is_applied()
        );
        let stale = job.clone();
        assert!(job.transition(JobTransition::Cancel).is_applied());

        store.save_job(&job).unwrap();
        store.save_job(&stale).unwrap();

        let loaded = store.load_job_by_id(job.id).unwrap().unwrap();
        assert_eq!(loaded.status(), JobStatus::Cancelled);
        assert_eq!(loaded.lifecycle_revision(), 2);
        assert_eq!(
            loaded.terminal_reason(),
            Some(JobTerminalReason::OperatorCancelled)
        );
    }

    #[test]
    fn test_sqlite_cancelled_not_loaded() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut job = Job::new(
            AgentId::new(),
            "x".to_string(),
            JobSchedule::Once { run_at: Utc::now() },
            None,
        );
        assert!(job.transition(JobTransition::Cancel).is_applied());
        store.save_job(&job).unwrap();
        // Cancelled jobs are not loaded on startup
        assert_eq!(store.load_all_jobs().unwrap().len(), 0);

        // Pending jobs are loaded.
        let pending = Job::new(
            job.agent_id,
            "pending".to_string(),
            JobSchedule::Once { run_at: Utc::now() },
            Some(Utc::now()),
        );
        store.save_job(&pending).unwrap();
        assert_eq!(store.load_all_jobs().unwrap().len(), 1);
    }

    #[test]
    fn dispatch_failure_budget_survives_restart_and_exhausts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job-retry.db");
        let path = path.to_string_lossy().into_owned();
        let store = JobStore::with_sqlite(&path).unwrap();
        let job = store.create(once_req()).unwrap();
        let first_retry = Utc::now() + chrono::Duration::seconds(5);
        assert_eq!(
            store
                .record_dispatch_failure(job.id, "first", first_retry, 3)
                .unwrap(),
            DispatchFailureOutcome::RetryScheduled {
                attempt: 1,
                retry_at: first_retry,
            }
        );
        drop(store);

        let restarted = JobStore::with_sqlite(&path).unwrap();
        let recovered = restarted.get(job.id).expect("retrying job must reload");
        assert_eq!(recovered.retry_count(), 1);
        assert_eq!(recovered.next_run_at, Some(first_retry));
        assert_eq!(recovered.last_error(), Some("first"));

        assert!(matches!(
            restarted
                .record_dispatch_failure(job.id, "second", Utc::now(), 3)
                .unwrap(),
            DispatchFailureOutcome::RetryScheduled { attempt: 2, .. }
        ));
        assert_eq!(
            restarted
                .record_dispatch_failure(job.id, "third", Utc::now(), 3)
                .unwrap(),
            DispatchFailureOutcome::Exhausted { attempts: 3 }
        );
        let failed = restarted.get(job.id).unwrap();
        assert_eq!(failed.status(), JobStatus::Failed);
        assert_eq!(
            failed.terminal_reason(),
            Some(JobTerminalReason::RetryExhausted)
        );
        assert_eq!(failed.next_run_at, None);
        assert_eq!(restarted.retry_attempts_total(), 2);
        assert_eq!(restarted.retry_exhaustions_total(), 1);
        drop(restarted);

        let final_restart = JobStore::with_sqlite(&path).unwrap();
        let failed = final_restart
            .get(job.id)
            .expect("failed job must remain observable after restart");
        assert_eq!(failed.status(), JobStatus::Failed);
        assert_eq!(
            failed.terminal_reason(),
            Some(JobTerminalReason::RetryExhausted)
        );
    }

    #[test]
    fn completed_and_cancelled_jobs_remain_observable_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job-history.db");
        let path = path.to_string_lossy().into_owned();
        let store = JobStore::with_sqlite(&path).unwrap();
        let completed = store.create(once_req()).unwrap();
        let cancelled = store.create(recurring_req()).unwrap();
        assert_eq!(
            store
                .record_run(completed.id, Utc::now(), JobStatus::Completed, None)
                .unwrap(),
            RecordRunOutcome::Recorded
        );
        assert_eq!(store.cancel(cancelled.id).unwrap(), Some(true));
        drop(store);

        let restarted = JobStore::with_sqlite(&path).unwrap();
        assert_eq!(
            restarted.get(completed.id).unwrap().status(),
            JobStatus::Completed
        );
        assert_eq!(
            restarted.get(cancelled.id).unwrap().status(),
            JobStatus::Cancelled
        );
    }
}
