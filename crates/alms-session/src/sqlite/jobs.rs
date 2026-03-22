//! Job CRUD.

use super::*;

impl SqliteStore {
    // ── Jobs ──────────────────────────────────────────────────────────────────

    /// Upsert a job row (handles both insert and update via OR REPLACE).
    pub fn save_job(&self, job: &Job) -> AlmsResult<()> {
        let schedule_json = serde_json::to_string(&job.schedule)
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job serialize: {e}")))?;
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO jobs \
                 (id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    job.id.0.to_string(),
                    job.agent_id.0.to_string(),
                    &job.prompt,
                    schedule_json,
                    job_status_to_str(job.status),
                    job.created_at.to_rfc3339(),
                    job.next_run_at.map(|t| t.to_rfc3339()),
                    job.last_run_at.map(|t| t.to_rfc3339()),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job: {e}")))?;
        Ok(())
    }

    /// Load all non-cancelled jobs, oldest first.
    pub fn load_all_jobs(&self) -> AlmsResult<Vec<Job>> {
        self.query_jobs(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
             FROM jobs WHERE status != 'cancelled' ORDER BY rowid",
        )
    }

    /// Load a single job by ID.
    pub fn load_job_by_id(&self, id: JobId) -> AlmsResult<Option<Job>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
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
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
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
