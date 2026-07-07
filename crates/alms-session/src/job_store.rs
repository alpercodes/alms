//! In-memory job store with optional SQLite write-through.

use crate::sqlite::SqliteStore;
use alms_core::AlmsResult;
use alms_core::job::{CreateJobRequest, Job, JobId, JobSchedule, JobStatus};
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{info, warn};

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
    RefusedCancelled,
    /// No job with this id exists (e.g. deleted mid-flight).
    NotFound,
}

/// Manages scheduled jobs in memory, with optional SQLite write-through.
///
/// On startup, all non-cancelled jobs are loaded from SQLite so the scheduler
/// (task #20) can re-register them without losing state across restarts.
#[derive(Debug, Clone)]
pub struct JobStore {
    jobs: Arc<DashMap<JobId, Job>>,
    store: Option<Arc<SqliteStore>>,
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
        }
    }

    /// Create a store backed by SQLite at `db_path`.
    ///
    /// Opens (or creates) the database and loads all non-cancelled jobs into memory.
    pub fn with_sqlite(db_path: &str) -> AlmsResult<Self> {
        let store = SqliteStore::open(db_path)?;
        let mut s = Self::new();
        s.store = Some(Arc::new(store));
        s.load_from_store()?;
        Ok(s)
    }

    fn load_from_store(&self) -> AlmsResult<()> {
        let Some(ref store) = self.store else {
            return Ok(());
        };
        let jobs = store.load_all_jobs()?;
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
            // Computed by the scheduler in task #20
            JobSchedule::Recurring { .. } => None,
        };

        let job = Job {
            id: JobId::new(),
            agent_id: req.agent_id,
            prompt: req.prompt,
            schedule: req.schedule,
            status: JobStatus::Pending,
            created_at: Utc::now(),
            next_run_at,
            last_run_at: None,
        };

        if let Some(ref store) = self.store
            && let Err(e) = store.save_job(&job)
        {
            warn!("Failed to persist job {}: {}", job.id.0, e);
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
        if let Some(mut entry) = self.jobs.get_mut(&id) {
            entry.next_run_at = next;
            let job = entry.clone();
            drop(entry);
            if let Some(ref store) = self.store
                && let Err(e) = store.save_job(&job)
            {
                warn!("Failed to persist next_run_at for job {}: {}", id.0, e);
            }
        }
        Ok(())
    }

    /// Record that a job fired: update last_run_at, status, and next_run_at atomically.
    ///
    /// **`Cancelled` is absorbing (#1202 S1):** if the job is already
    /// `Cancelled`, the write is refused — nothing is mutated and
    /// [`RecordRunOutcome::RefusedCancelled`] is returned. This closes the
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
        let Some(mut entry) = self.jobs.get_mut(&id) else {
            return Ok(RecordRunOutcome::NotFound);
        };
        if entry.status == JobStatus::Cancelled {
            drop(entry);
            info!(
                "Job {} is cancelled — record_run refused (Cancelled is absorbing, #1202 S1)",
                id.0
            );
            return Ok(RecordRunOutcome::RefusedCancelled);
        }
        entry.last_run_at = Some(ran_at);
        entry.status = new_status;
        entry.next_run_at = next_run_at;
        let job = entry.clone();
        drop(entry);
        if let Some(ref store) = self.store
            && let Err(e) = store.save_job(&job)
        {
            warn!("Failed to persist record_run for job {}: {}", id.0, e);
        }
        info!("Job {} run recorded (status={:?})", id.0, new_status);
        Ok(RecordRunOutcome::Recorded)
    }

    /// Cancel a job.
    ///
    /// Returns:
    /// - `Ok(Some(true))` — job was found and cancelled
    /// - `Ok(Some(false))` — job exists but was already cancelled
    /// - `Ok(None)` — job not found
    pub fn cancel(&self, id: JobId) -> AlmsResult<Option<bool>> {
        let Some(mut entry) = self.jobs.get_mut(&id) else {
            return Ok(None);
        };

        if entry.status == JobStatus::Cancelled {
            return Ok(Some(false));
        }

        entry.status = JobStatus::Cancelled;
        entry.next_run_at = None;
        let job = entry.clone();
        drop(entry);

        if let Some(ref store) = self.store
            && let Err(e) = store.save_job(&job)
        {
            warn!("Failed to persist cancellation for job {}: {}", id.0, e);
        }

        info!("Cancelled job {}", id.0);
        Ok(Some(true))
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
        assert_eq!(listed[0].status, JobStatus::Cancelled);
    }

    #[test]
    fn test_cancel_unknown_returns_none() {
        let store = JobStore::new();
        assert_eq!(store.cancel(JobId::new()).unwrap(), None);
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
            RecordRunOutcome::RefusedCancelled,
            "record_run on a cancelled job must be refused"
        );

        let job = store.get(job.id).unwrap();
        assert_eq!(
            job.status,
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
        assert_eq!(job.status, JobStatus::Active);
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
        let job = Job {
            id: JobId::new(),
            agent_id: req.agent_id,
            prompt: req.prompt,
            schedule: req.schedule,
            status: JobStatus::Pending,
            created_at: Utc::now(),
            next_run_at: Some(Utc::now() + chrono::Duration::hours(1)),
            last_run_at: None,
        };
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
    fn test_sqlite_cancelled_not_loaded() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut job = Job {
            id: JobId::new(),
            agent_id: AgentId::new(),
            prompt: "x".to_string(),
            schedule: JobSchedule::Once { run_at: Utc::now() },
            status: JobStatus::Cancelled,
            created_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        store.save_job(&job).unwrap();
        // Cancelled jobs are not loaded on startup
        assert_eq!(store.load_all_jobs().unwrap().len(), 0);

        // Pending jobs are loaded
        job.id = JobId::new();
        job.status = JobStatus::Pending;
        store.save_job(&job).unwrap();
        assert_eq!(store.load_all_jobs().unwrap().len(), 1);
    }
}
