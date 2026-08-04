//! Job CRUD.

use super::*;

impl SqliteStore {
    // ── Jobs ──────────────────────────────────────────────────────────────────

    /// Upsert a job row, refusing snapshots older than the persisted revision.
    pub fn save_job(&self, job: &Job) -> AlmsResult<()> {
        let schedule_json = serde_json::to_string(&job.schedule)
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job serialize: {e}")))?;
        let lifecycle_revision = i64::try_from(job.lifecycle_revision()).map_err(|_| {
            AlmsError::Runtime(format!(
                "job {} lifecycle revision {} exceeds SQLite INTEGER",
                job.id.0,
                job.lifecycle_revision()
            ))
        })?;
        let affected = self
            .conn
            .lock()
            .execute(
                "INSERT INTO jobs \
                 (id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at, \
                  lifecycle_revision, terminal_reason, retry_count, last_error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(id) DO UPDATE SET \
                   agent_id=excluded.agent_id, prompt=excluded.prompt, schedule=excluded.schedule, \
                   status=excluded.status, created_at=excluded.created_at, \
                   next_run_at=excluded.next_run_at, last_run_at=excluded.last_run_at, \
                   lifecycle_revision=excluded.lifecycle_revision, terminal_reason=excluded.terminal_reason, \
                   retry_count=excluded.retry_count, last_error=excluded.last_error \
                 WHERE excluded.lifecycle_revision > jobs.lifecycle_revision",
                params![
                    job.id.0.to_string(),
                    job.agent_id.0.to_string(),
                    &job.prompt,
                    schedule_json,
                    job_status_to_str(job.status()),
                    job.created_at.to_rfc3339(),
                    job.next_run_at.map(|t| t.to_rfc3339()),
                    job.last_run_at.map(|t| t.to_rfc3339()),
                    lifecycle_revision,
                    job.terminal_reason().map(job_terminal_reason_to_str),
                    job.retry_count(),
                    job.last_error(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job: {e}")))?;
        if affected == 0 {
            self.record_persistence_snapshot_rejection();
        }
        Ok(())
    }

    /// Load all non-terminal jobs, oldest first.
    ///
    /// **No production callers (#1238 N3).** `JobStore::load_from_store` uses
    /// [`Self::load_all_jobs_unfiltered`] so terminal jobs stay observable in
    /// `GET /jobs` after a restart; this filtered variant survives only as the
    /// assertion surface for the terminal-status filter that migration v3
    /// changed (`crates/alms-session/tests/schema_migrations.rs` and the
    /// `job_store` unit tests). Do not add it to a startup path — doing so
    /// would silently drop completed, failed, and cancelled jobs from the API.
    pub fn load_all_jobs(&self) -> AlmsResult<Vec<Job>> {
        self.query_jobs(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at, \
                    lifecycle_revision, terminal_reason, retry_count, last_error \
             FROM jobs WHERE status NOT IN ('completed', 'failed', 'cancelled') ORDER BY rowid",
        )
    }

    /// Load a single job by ID.
    pub fn load_job_by_id(&self, id: JobId) -> AlmsResult<Option<Job>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at, \
                    lifecycle_revision, terminal_reason, retry_count, last_error \
             FROM jobs WHERE id = ?1",
            params![id.0.to_string()],
            parse_job_row,
        );
        match result {
            Ok(job) => Ok(Some(job)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_job_by_id: {e}"))),
        }
    }

    /// Load all jobs including cancelled, ordered by created_at DESC.
    pub fn load_all_jobs_unfiltered(&self) -> AlmsResult<Vec<Job>> {
        self.query_jobs(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at, \
                    lifecycle_revision, terminal_reason, retry_count, last_error \
             FROM jobs ORDER BY created_at DESC",
        )
    }

    /// Shared helper for job list queries.
    fn query_jobs(&self, sql: &str) -> AlmsResult<Vec<Job>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare jobs: {e}")))?;

        let rows = stmt
            .query_map([], parse_job_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query jobs: {e}")))?
            .filter_map(|r| match r {
                Ok(j) => Some(j),
                Err(e) => {
                    tracing::warn!("Skipping unparseable job row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }
}
