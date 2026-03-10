//! In-memory job store with optional SQLite write-through.

use crate::sqlite::SqliteStore;
use alms_core::AlmsResult;
use alms_core::job::{CreateJobRequest, Job, JobId, JobSchedule, JobStatus};
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{info, warn};

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
    /// Returns the updated job, or `None` if the job was not found (e.g. deleted mid-flight).
    pub fn record_run(
        &self,
        id: JobId,
        ran_at: chrono::DateTime<chrono::Utc>,
        new_status: JobStatus,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> AlmsResult<Option<Job>> {
        let Some(mut entry) = self.jobs.get_mut(&id) else {
            return Ok(None);
        };
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
        Ok(Some(job))
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
