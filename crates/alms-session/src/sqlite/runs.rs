//! Run persistence -- save, load, mark_stale.

use super::*;

impl SqliteStore {
    // ── Runs ─────────────────────────────────────────────────────────────────

    /// Insert or update a run row (upsert).
    pub fn save_run(&self, run: &Run) -> AlmsResult<()> {
        let (prompt_tokens, completion_tokens) = run
            .usage
            .map(|u| {
                (
                    Some(u.prompt_tokens as i64),
                    Some(u.completion_tokens as i64),
                )
            })
            .unwrap_or((None, None));

        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO runs \
                 (run_id, session_id, agent_id, input, response, error, status, \
                  started_at, ended_at, prompt_tokens, completion_tokens, job_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    run.run_id.0.to_string(),
                    run.session_id.0.to_string(),
                    run.agent_id.0.to_string(),
                    &run.input,
                    run.output.as_deref(),
                    run.error.as_deref(),
                    run_status_to_str(run.status),
                    run.started_at.map(|dt| dt.to_rfc3339()),
                    run.ended_at.map(|dt| dt.to_rfc3339()),
                    prompt_tokens,
                    completion_tokens,
                    run.job_id.map(|j| j.0.to_string()),
                    run.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_run: {e}")))?;
        Ok(())
    }

    /// Load a single run by its ID.
    pub fn load_run(&self, run_id: RunId) -> AlmsResult<Option<Run>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT run_id, session_id, agent_id, input, response, error, status, \
                    started_at, ended_at, prompt_tokens, completion_tokens, job_id, created_at \
             FROM runs WHERE run_id = ?1",
            params![run_id.0.to_string()],
            parse_run_row,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_run: {e}"))),
        }
    }

    /// Load runs for a session, newest first, up to `limit`.
    pub fn load_runs_by_session(
        &self,
        session_id: SessionId,
        limit: usize,
    ) -> AlmsResult<Vec<Run>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT run_id, session_id, agent_id, input, response, error, status, \
                        started_at, ended_at, prompt_tokens, completion_tokens, job_id, created_at \
                 FROM runs WHERE session_id = ?1 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare load_runs_by_session: {e}")))?;

        let rows = stmt
            .query_map(
                params![session_id.0.to_string(), limit as i64],
                parse_run_row,
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_runs_by_session: {e}")))?
            .filter_map(|r| match r {
                Ok(run) => Some(run),
                Err(e) => {
                    tracing::warn!("Skipping unparseable run row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Load runs for startup hydration, oldest first.
    ///
    /// Only loads runs created within the last `max_age` duration to prevent
    /// unbounded memory growth after months of operation. Defaults to 7 days.
    pub fn load_all_runs(&self) -> AlmsResult<Vec<Run>> {
        self.load_recent_runs(chrono::Duration::days(7))
    }

    /// Load runs created within the given `max_age` duration, oldest first.
    pub fn load_recent_runs(&self, max_age: chrono::Duration) -> AlmsResult<Vec<Run>> {
        let cutoff = (chrono::Utc::now() - max_age).to_rfc3339();
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT run_id, session_id, agent_id, input, response, error, status, \
                        started_at, ended_at, prompt_tokens, completion_tokens, job_id, created_at \
                 FROM runs WHERE created_at >= ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare load_all_runs: {e}")))?;

        let rows = stmt
            .query_map(params![cutoff], parse_run_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_all_runs: {e}")))?
            .filter_map(|r| match r {
                Ok(run) => Some(run),
                Err(e) => {
                    tracing::warn!("Skipping unparseable run row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Mark any `queued` or `running` runs as `failed` in SQLite.
    ///
    /// These are stale leftovers from a previous process that crashed or was
    /// killed. Returns the number of rows updated.
    pub fn mark_stale_runs_failed(&self) -> AlmsResult<usize> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        let count = conn
            .execute(
                "UPDATE runs SET status = 'failed', error = 'stale: gateway restarted', ended_at = ?1 \
                 WHERE status IN ('queued', 'running')",
                params![now],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite mark_stale_runs_failed: {e}")))?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::types::Session;
    use alms_core::run::{Run, RunStatus, TokenUsage};

    fn new_session() -> Session {
        Session::new(AgentId::new(), "test-ctx")
    }

    fn new_run(session_id: SessionId, agent_id: AgentId) -> Run {
        Run::new(session_id, agent_id, "hello".to_string())
    }

    #[test]
    fn test_run_save_and_load() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let run = new_run(session.id, session.agent_id);
        let run_id = run.run_id;

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run_id).unwrap().expect("run should exist");
        assert_eq!(loaded.run_id, run_id);
        assert_eq!(loaded.session_id, session.id);
        assert_eq!(loaded.agent_id, session.agent_id);
        assert_eq!(loaded.input, "hello");
        assert!(matches!(loaded.status, RunStatus::Queued));
        assert!(loaded.output.is_none());
        assert!(loaded.error.is_none());
        assert!(loaded.usage.is_none());
        assert!(loaded.started_at.is_none());
        assert!(loaded.ended_at.is_none());
    }

    #[test]
    fn test_run_completed_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        run.mark_running();
        run.mark_completed(
            "I am a response".to_string(),
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
            },
        );

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status, RunStatus::Completed));
        assert_eq!(loaded.output.as_deref(), Some("I am a response"));
        assert!(loaded.error.is_none());
        let usage = loaded.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert!(loaded.started_at.is_some());
        assert!(loaded.ended_at.is_some());
    }

    #[test]
    fn test_run_failed_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        run.mark_running();
        run.mark_failed("LLM error".to_string());

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status, RunStatus::Failed));
        assert_eq!(loaded.error.as_deref(), Some("LLM error"));
        assert!(loaded.output.is_none());
    }

    #[test]
    fn test_run_cancelled_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        run.mark_running();
        run.mark_cancelled();

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status, RunStatus::Cancelled));
        assert!(loaded.output.is_none());
        assert!(loaded.error.is_none());
        assert!(loaded.started_at.is_some());
        assert!(loaded.ended_at.is_some());
        assert!(loaded.usage.is_none());
    }

    #[test]
    fn test_mark_stale_runs_failed() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();

        // Insert a queued run and a running run.
        let queued_run = new_run(session.id, session.agent_id);
        let queued_id = queued_run.run_id;
        store.save_run(&queued_run).unwrap();

        let mut running_run = new_run(session.id, session.agent_id);
        running_run.mark_running();
        let running_id = running_run.run_id;
        store.save_run(&running_run).unwrap();

        // Insert a completed run (should not be affected).
        let mut completed_run = new_run(session.id, session.agent_id);
        completed_run.mark_running();
        completed_run.mark_completed(
            "done".to_string(),
            TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
            },
        );
        let completed_id = completed_run.run_id;
        store.save_run(&completed_run).unwrap();

        let count = store.mark_stale_runs_failed().unwrap();
        assert_eq!(count, 2);

        // Queued and running should now be failed.
        let q = store.load_run(queued_id).unwrap().unwrap();
        assert!(matches!(q.status, RunStatus::Failed));
        assert_eq!(q.error.as_deref(), Some("stale: gateway restarted"));
        assert!(q.ended_at.is_some());

        let r = store.load_run(running_id).unwrap().unwrap();
        assert!(matches!(r.status, RunStatus::Failed));
        assert_eq!(r.error.as_deref(), Some("stale: gateway restarted"));

        // Completed run should be unchanged.
        let c = store.load_run(completed_id).unwrap().unwrap();
        assert!(matches!(c.status, RunStatus::Completed));
        assert_eq!(c.output.as_deref(), Some("done"));
    }

    #[test]
    fn test_run_load_by_session() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session1 = new_session();
        let session2 = Session::new(AgentId::new(), "other-ctx");

        let r1 = new_run(session1.id, session1.agent_id);
        let r2 = new_run(session1.id, session1.agent_id);
        let r3 = new_run(session2.id, session2.agent_id);
        store.save_run(&r1).unwrap();
        store.save_run(&r2).unwrap();
        store.save_run(&r3).unwrap();

        let runs = store.load_runs_by_session(session1.id, 50).unwrap();
        assert_eq!(runs.len(), 2);

        let runs = store.load_runs_by_session(session2.id, 50).unwrap();
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn test_run_load_by_session_limit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();

        for _ in 0..5 {
            store
                .save_run(&new_run(session.id, session.agent_id))
                .unwrap();
        }

        let runs = store.load_runs_by_session(session.id, 3).unwrap();
        assert_eq!(runs.len(), 3);
    }

    #[test]
    fn test_run_upsert_updates() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        store.save_run(&run).unwrap();

        // Transition to completed
        run.mark_running();
        run.mark_completed(
            "done".to_string(),
            TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
            },
        );
        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status, RunStatus::Completed));
        assert_eq!(loaded.output.as_deref(), Some("done"));
    }

    #[test]
    fn test_run_load_nonexistent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let result = store.load_run(RunId::new()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_run_with_job_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let job_id = alms_core::job::JobId(uuid::Uuid::new_v4());
        let run = Run::for_job(
            session.id,
            session.agent_id,
            "job prompt".to_string(),
            job_id,
        );

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert_eq!(loaded.job_id, Some(job_id));
        assert_eq!(loaded.input, "job prompt");
    }

    #[test]
    fn test_load_all_runs() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();

        store
            .save_run(&new_run(session.id, session.agent_id))
            .unwrap();
        store
            .save_run(&new_run(session.id, session.agent_id))
            .unwrap();

        let all = store.load_all_runs().unwrap();
        assert_eq!(all.len(), 2);
    }
}
