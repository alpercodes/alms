//! Run persistence -- save, load, mark_stale.

use super::*;
use alms_core::{RunTransition, TransitionOutcome};

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

        // #837: serialize the layered run-config snapshot to JSON for
        // storage in the new TEXT `resolved_config` column. `None`
        // (queued runs, old rows) maps to a SQL NULL via Option.
        // A serialization failure is non-fatal — we log and store NULL
        // so a malformed snapshot can never block a run from persisting.
        let resolved_config_json = run.resolved_config().and_then(|cfg| {
            serde_json::to_string(cfg)
                .map_err(|e| {
                    tracing::warn!(
                        run_id = %run.run_id.0,
                        "Failed to serialize resolved_config snapshot, persisting NULL: {e}"
                    );
                })
                .ok()
        });

        let lifecycle_revision = i64::try_from(run.lifecycle_revision()).map_err(|_| {
            AlmsError::Runtime(format!(
                "run {} lifecycle revision {} exceeds SQLite INTEGER",
                run.run_id.0,
                run.lifecycle_revision()
            ))
        })?;
        self.conn
            .lock()
            .execute(
                "INSERT INTO runs \
                 (run_id, session_id, agent_id, input, response, error, status, \
                  started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                  parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
                 ON CONFLICT(run_id) DO UPDATE SET \
                   session_id=excluded.session_id, agent_id=excluded.agent_id, input=excluded.input, \
                   response=excluded.response, error=excluded.error, status=excluded.status, \
                   started_at=excluded.started_at, ended_at=excluded.ended_at, \
                   prompt_tokens=excluded.prompt_tokens, completion_tokens=excluded.completion_tokens, \
                   job_id=excluded.job_id, parent_run_id=excluded.parent_run_id, \
                   created_at=excluded.created_at, resolved_config=excluded.resolved_config, \
                   lifecycle_revision=excluded.lifecycle_revision, terminal_reason=excluded.terminal_reason \
                 WHERE excluded.lifecycle_revision > runs.lifecycle_revision",
                params![
                    run.run_id.0.to_string(),
                    run.session_id.0.to_string(),
                    run.agent_id.0.to_string(),
                    &run.input,
                    run.output.as_deref(),
                    run.error.as_deref(),
                    run_status_to_str(run.status()),
                    run.started_at.map(|dt| dt.to_rfc3339()),
                    run.ended_at.map(|dt| dt.to_rfc3339()),
                    prompt_tokens,
                    completion_tokens,
                    run.job_id.map(|j| j.0.to_string()),
                    run.parent_run_id.map(|r| r.0.to_string()),
                    run.created_at.to_rfc3339(),
                    resolved_config_json,
                    lifecycle_revision,
                    run.terminal_reason(),
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
                    started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                    parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason \
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
                        started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                        parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason \
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
                        started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                        parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason \
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
        let stale_runs = {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT run_id, session_id, agent_id, input, response, error, status, \
                            started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                            parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason \
                     FROM runs WHERE status IN ('queued', 'running')",
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite prepare mark_stale_runs_failed: {e}"))
                })?;
            stmt.query_map([], parse_run_row)
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite query mark_stale_runs_failed: {e}"))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite parse mark_stale_runs_failed: {e}"))
                })?
        };

        let mut count = 0;
        for mut run in stale_runs {
            match run.transition(RunTransition::Fail {
                error: "stale: gateway restarted".to_string(),
                terminal_reason: "gateway_restarted".to_string(),
            }) {
                TransitionOutcome::Applied { .. } => {
                    self.save_run(&run)?;
                    count += 1;
                }
                TransitionOutcome::Rejected { .. } => {
                    return Err(AlmsError::Runtime(format!(
                        "stale run {} could not be quarantined at lifecycle revision {}",
                        run.run_id.0,
                        run.lifecycle_revision()
                    )));
                }
                TransitionOutcome::NoOp { .. } => {}
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::new_session;
    use super::super::*;
    use alms_core::run::{Run, RunStatus, RunTransition, TokenUsage};

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
        assert!(matches!(loaded.status(), RunStatus::Queued));
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
        let _ = run.mark_completed(
            "I am a response".to_string(),
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                ..TokenUsage::default()
            },
        );

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status(), RunStatus::Completed));
        assert_eq!(loaded.output.as_deref(), Some("I am a response"));
        assert!(loaded.error.is_none());
        let usage = loaded.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert!(loaded.started_at.is_some());
        assert!(loaded.ended_at.is_some());
    }

    #[test]
    fn stale_running_snapshot_cannot_overwrite_any_terminal_state() {
        for terminal in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            let store = SqliteStore::open_in_memory().unwrap();
            let session = new_session();
            let mut running = new_run(session.id, session.agent_id);
            assert!(running.mark_running());
            let stale = running.clone();

            match terminal {
                RunStatus::Completed => {
                    assert!(running.mark_completed("done".into(), TokenUsage::default()));
                }
                RunStatus::Failed => assert!(running.mark_failed("boom".into())),
                RunStatus::Cancelled => assert!(running.mark_cancelled()),
                _ => unreachable!(),
            }

            store.save_run(&running).unwrap();
            store.save_run(&stale).unwrap();
            let loaded = store.load_run(running.run_id).unwrap().unwrap();
            assert_eq!(loaded.status(), terminal);
            assert_eq!(loaded.lifecycle_revision(), 2);
        }
    }

    #[test]
    fn equal_revision_run_snapshot_is_an_idempotent_noop() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut authoritative = new_run(session.id, session.agent_id);
        assert!(authoritative.mark_running());
        assert!(authoritative.mark_completed("authoritative".into(), TokenUsage::default()));
        let conflicting = Run::from_persisted(
            authoritative.run_id,
            authoritative.session_id,
            authoritative.agent_id,
            authoritative.status(),
            authoritative.input.clone(),
            Some("delayed conflicting payload".to_string()),
            authoritative.error.clone(),
            authoritative.usage,
            authoritative.created_at,
            authoritative.started_at,
            authoritative.ended_at,
            authoritative.job_id,
            authoritative.parent_run_id,
            authoritative.resolved_config().cloned(),
            authoritative.lifecycle_revision(),
            Some("conflicting_reason".to_string()),
        );

        store.save_run(&authoritative).unwrap();
        store.save_run(&conflicting).unwrap();

        let loaded = store
            .load_run(authoritative.run_id)
            .unwrap()
            .expect("run should remain persisted");
        assert_eq!(loaded.output.as_deref(), Some("authoritative"));
        assert_eq!(loaded.terminal_reason(), None);
        assert_eq!(
            loaded.lifecycle_revision(),
            authoritative.lifecycle_revision()
        );
    }

    #[test]
    fn test_run_failed_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        run.mark_running();
        let _ = run.mark_failed("LLM error".to_string());

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status(), RunStatus::Failed));
        assert_eq!(loaded.error.as_deref(), Some("LLM error"));
        assert!(loaded.output.is_none());
    }

    #[test]
    fn test_run_cancelled_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        run.mark_running();
        let _ = run.mark_cancelled();

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status(), RunStatus::Cancelled));
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
        let _ = completed_run.mark_completed(
            "done".to_string(),
            TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
                ..TokenUsage::default()
            },
        );
        let completed_id = completed_run.run_id;
        store.save_run(&completed_run).unwrap();

        let count = store.mark_stale_runs_failed().unwrap();
        assert_eq!(count, 2);

        // Queued and running should now be failed.
        let q = store.load_run(queued_id).unwrap().unwrap();
        assert!(matches!(q.status(), RunStatus::Failed));
        assert_eq!(q.error.as_deref(), Some("stale: gateway restarted"));
        assert!(q.ended_at.is_some());
        assert_eq!(q.lifecycle_revision(), 1);
        assert_eq!(q.terminal_reason(), Some("gateway_restarted"));

        let r = store.load_run(running_id).unwrap().unwrap();
        assert!(matches!(r.status(), RunStatus::Failed));
        assert_eq!(r.error.as_deref(), Some("stale: gateway restarted"));
        assert_eq!(r.lifecycle_revision(), 2);
        assert_eq!(r.terminal_reason(), Some("gateway_restarted"));

        // Completed run should be unchanged.
        let c = store.load_run(completed_id).unwrap().unwrap();
        assert!(matches!(c.status(), RunStatus::Completed));
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
        let _ = run.mark_completed(
            "done".to_string(),
            TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                ..TokenUsage::default()
            },
        );
        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(matches!(loaded.status(), RunStatus::Completed));
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
    fn test_run_with_parent_run_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let parent_run_id = RunId::new();
        let run = Run::for_subagent(
            session.id,
            session.agent_id,
            "subagent task".to_string(),
            parent_run_id,
        );

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert_eq!(loaded.parent_run_id, Some(parent_run_id));
        assert_eq!(loaded.input, "subagent task");
    }

    #[test]
    fn test_run_without_parent_run_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let run = new_run(session.id, session.agent_id);

        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert_eq!(loaded.parent_run_id, None);
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

    /// #837: a run with `resolved_config: None` (the default for new
    /// runs and the only state for old SQLite rows that pre-date the
    /// snapshot column) round-trips cleanly. This is the backward-compat
    /// surface — old rows must hydrate to `None` without surfacing a
    /// parse error.
    #[test]
    fn test_run_resolved_config_none_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let run = new_run(session.id, session.agent_id);
        assert!(run.resolved_config().is_none());
        store.save_run(&run).unwrap();
        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        assert!(loaded.resolved_config().is_none());
    }

    /// #837: a populated `ResolvedRunConfig` round-trips through the
    /// JSON-encoded TEXT column without losing fields. Pinned so a
    /// future schema change to `ResolvedRunConfig` either preserves
    /// serde compatibility or fails this test loudly at CI time.
    #[test]
    fn test_run_resolved_config_some_roundtrip() {
        use alms_core::ResolvedRunConfig;

        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        let mut run = new_run(session.id, session.agent_id);
        let cfg = ResolvedRunConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 8192,
            posture: "autonomous".into(),
            debug_mode: true,
            thinking_budget_tokens: 4096,
            reasoning_effort: Some(alms_core::config::ReasoningEffort::Medium),
            gemini_thinking_budget: Some(2048),
        };
        assert!(
            run.transition(RunTransition::Start {
                resolved_config: Some(cfg.clone()),
            })
            .is_applied()
        );
        store.save_run(&run).unwrap();

        let loaded = store.load_run(run.run_id).unwrap().unwrap();
        let loaded_cfg = loaded
            .resolved_config()
            .expect("resolved_config should round-trip");
        assert_eq!(loaded_cfg, &cfg);
    }
}
