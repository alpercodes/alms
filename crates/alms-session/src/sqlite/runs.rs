//! Run persistence -- save, load, mark_stale.

use super::*;
use alms_core::{RunTransition, TransitionOutcome};
use rusqlite::{Transaction, TransactionBehavior};

impl SqliteStore {
    // ── Runs ─────────────────────────────────────────────────────────────────

    /// Insert or update a run row (upsert).
    pub fn save_run(&self, run: &Run) -> AlmsResult<()> {
        let conn = self.conn.lock();
        let affected = Self::save_run_on(&conn, run)?;
        if affected == 0 {
            self.record_persistence_snapshot_rejection();
        }
        Ok(())
    }

    pub(super) fn save_run_on(conn: &Connection, run: &Run) -> AlmsResult<usize> {
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
        let affected = conn.execute(
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
        Ok(affected)
    }

    /// Persist a queued run, its initial user message, and the session
    /// activity timestamp as one authoritative admission transaction.
    pub fn save_run_with_initial_message(
        &self,
        run: &Run,
        message: &Message,
    ) -> AlmsResult<Timestamp> {
        let mut conn = self.conn.lock();
        let touched_at = Timestamp::now();
        let transaction = conn
            .transaction()
            .map_err(|error| AlmsError::Runtime(format!("SQLite begin run admission: {error}")))?;

        let affected = Self::save_run_on(&transaction, run)?;
        if affected != 1 {
            self.record_persistence_snapshot_rejection();
            return Err(AlmsError::Runtime(format!(
                "SQLite run admission rejected duplicate or stale run {}",
                run.run_id.0
            )));
        }
        Self::save_message_on(&transaction, run.session_id, message)?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET last_activity = CASE
                     WHEN last_activity > ?1 THEN last_activity ELSE ?1
                 END
                 WHERE id = ?2",
                params![touched_at.0.to_rfc3339(), run.session_id.0.to_string(),],
            )
            .map_err(|error| {
                AlmsError::Runtime(format!("SQLite touch admitted run session: {error}"))
            })?;
        if updated != 1 {
            return Err(AlmsError::SessionNotFound(run.session_id.0.to_string()));
        }

        transaction
            .commit()
            .map_err(|error| AlmsError::Runtime(format!("SQLite commit run admission: {error}")))?;
        Ok(touched_at)
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

    /// Reconcile durable run/input state after a gateway restart.
    ///
    /// These are stale leftovers from a previous process that crashed or was
    /// killed. Queued runs may still own an initial user message hidden by
    /// `pending_input`; release that message in the same savepoint as the
    /// terminal run transition so restart recovery cannot expose a split state.
    /// Inputs belonging to runs that were already terminal before the restart
    /// are also released without changing the run's terminal state. This covers
    /// cancellation winning before the queued worker claims its input.
    /// Returns the number of runs updated.
    ///
    /// **Per-row failures are non-fatal (#1236).** Each row is reconciled in
    /// its own savepoint: one row that cannot be reconciled rolls back only
    /// its own writes, is logged with its `run_id` and the SQL an operator
    /// would run to clear it, is counted in
    /// [`Self::stale_run_recovery_failures_total`], and recovery continues for
    /// every other row. Phase 7 made all three of those arms hard errors on a
    /// single whole-sweep transaction, which meant one inconsistent row rolled
    /// back recovery for every other run *and* stopped `Gateway::new` — an
    /// unbootable daemon with no skip-recovery flag and no repair path. This
    /// partially softens `0043d6b` ("Fail closed on partial session
    /// reconciliation") by design: a daemon that will not start is a worse
    /// outcome than one stale run row. The broader fail-open-vs-fail-closed
    /// policy question is tracked in #1237.
    ///
    /// The transaction is `Immediate` (#1224's rule): the sweep reads before it
    /// writes, which is the `SQLITE_BUSY_SNAPSHOT` shape a deferred
    /// transaction hits when a second process is writing concurrently. In-
    /// process the single `conn` mutex already serializes this, but the failure
    /// mode it prevents — "gateway will not start, confusing SQLite error" —
    /// is exactly what the per-row handling above exists to avoid.
    pub fn mark_stale_runs_failed(&self) -> AlmsResult<usize> {
        let mut conn = self.conn.lock();
        let mut transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                AlmsError::Runtime(format!("SQLite begin stale-run recovery: {error}"))
            })?;
        let stale_runs = {
            let mut stmt = transaction
                .prepare(
                    "SELECT run_id, session_id, agent_id, input, response, error, status, \
                            started_at, ended_at, prompt_tokens, completion_tokens, job_id, \
                            parent_run_id, created_at, resolved_config, lifecycle_revision, terminal_reason \
                     FROM runs WHERE status IN ('queued', 'running') ORDER BY created_at ASC",
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
        let terminal_runs_with_pending_input = {
            let mut stmt = transaction
                .prepare(
                    // Keep messages as SQLite's outer loop so startup performs one
                    // history scan plus primary-key run lookups, not one history
                    // scan for every terminal run in a long-lived session.
                    r#"SELECT DISTINCT
                            runs.run_id, runs.session_id, runs.agent_id, runs.input, runs.response,
                            runs.error, runs.status, runs.started_at, runs.ended_at,
                            runs.prompt_tokens, runs.completion_tokens, runs.job_id,
                            runs.parent_run_id, runs.created_at, runs.resolved_config,
                            runs.lifecycle_revision, runs.terminal_reason
                       FROM messages CROSS JOIN runs
                       WHERE messages.metadata IS NOT NULL
                         AND json_extract(
                             CASE WHEN json_valid(messages.metadata)
                                  THEN messages.metadata ELSE '{}' END,
                             '$.pending_input'
                         ) = 1
                         AND runs.run_id = json_extract(
                             CASE WHEN json_valid(messages.metadata)
                                  THEN messages.metadata ELSE '{}' END,
                             '$.run_id'
                         )
                         AND runs.session_id = messages.session_id
                         AND runs.status IN ('completed', 'failed', 'cancelled')
                       ORDER BY runs.created_at ASC"#,
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!(
                        "SQLite prepare terminal pending-input recovery: {e}"
                    ))
                })?;
            stmt.query_map([], parse_run_row)
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite query terminal pending-input recovery: {e}"))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite parse terminal pending-input recovery: {e}"))
                })?
        };

        let mut count = 0;
        let claimed_at = Timestamp::now().0.to_rfc3339();
        for mut run in stale_runs {
            let run_id = run.run_id;
            let session_id = run.session_id;
            let outcome = Self::in_savepoint(&mut transaction, |savepoint| {
                self.quarantine_stale_run_on(savepoint, &mut run, &claimed_at)
            });
            match outcome {
                Ok(()) => count += 1,
                Err(error) => self.report_unreconcilable_run(run_id, session_id, &error),
            }
        }
        for run in terminal_runs_with_pending_input {
            let run_id = run.run_id;
            let session_id = run.session_id;
            let outcome = Self::in_savepoint(&mut transaction, |savepoint| {
                Self::settle_pending_input_on(savepoint, session_id, run_id, &claimed_at)?;
                Ok(())
            });
            if let Err(error) = outcome {
                self.report_unreconcilable_run(run_id, session_id, &error);
            }
        }
        transaction.commit().map_err(|error| {
            AlmsError::Runtime(format!("SQLite commit stale-run recovery: {error}"))
        })?;
        Ok(count)
    }

    /// Run `body` inside a nested savepoint so a per-row failure rolls back
    /// only that row's writes and leaves the enclosing sweep transaction
    /// usable for the rows that follow (#1236).
    ///
    /// That isolation is not absolute, and the exception is the useful one:
    /// constraint and application errors (including a `RAISE(ABORT)` trigger)
    /// are contained to the savepoint, but `SQLITE_FULL`, `SQLITE_IOERR`,
    /// `SQLITE_NOMEM`, and `SQLITE_INTERRUPT` can roll back the entire
    /// transaction. That degrades in the right direction — the sweep's
    /// `commit()` then fails and `mark_stale_runs_failed` returns `Err`,
    /// restoring fail-closed behaviour for exactly the catastrophic,
    /// whole-database cases where booting past the problem is not the right
    /// answer.
    fn in_savepoint<T>(
        transaction: &mut Transaction<'_>,
        body: impl FnOnce(&Connection) -> AlmsResult<T>,
    ) -> AlmsResult<T> {
        let savepoint = transaction.savepoint().map_err(|error| {
            AlmsError::Runtime(format!("SQLite begin stale-run savepoint: {error}"))
        })?;
        // `Savepoint`'s drop path rolls back, so the `?` below discards the
        // row's partial writes without touching the enclosing transaction.
        let value = body(&savepoint)?;
        savepoint.commit().map_err(|error| {
            AlmsError::Runtime(format!("SQLite commit stale-run savepoint: {error}"))
        })?;
        Ok(value)
    }

    /// Transition one stale run to `failed` and release its pending input.
    fn quarantine_stale_run_on(
        &self,
        conn: &Connection,
        run: &mut Run,
        claimed_at: &str,
    ) -> AlmsResult<()> {
        match run.transition(RunTransition::Fail {
            error: "stale: gateway restarted".to_string(),
            terminal_reason: "gateway_restarted".to_string(),
        }) {
            TransitionOutcome::Applied { .. } => {}
            TransitionOutcome::Rejected { .. } => {
                return Err(AlmsError::Runtime(format!(
                    "stale run {} could not be quarantined at lifecycle revision {}",
                    run.run_id.0,
                    run.lifecycle_revision()
                )));
            }
            TransitionOutcome::NoOp { .. } => {
                return Err(AlmsError::Runtime(format!(
                    "stale run {} produced a no-op recovery transition",
                    run.run_id.0
                )));
            }
        }
        let affected = Self::save_run_on(conn, run)?;
        if affected != 1 {
            self.record_persistence_snapshot_rejection();
            return Err(AlmsError::Runtime(format!(
                "stale run {} lost its authoritative recovery transition",
                run.run_id.0
            )));
        }
        Self::settle_pending_input_on(conn, run.session_id, run.run_id, claimed_at)?;
        Ok(())
    }

    /// Log an unreconcilable run row so the line is actionable on its own —
    /// the `run_id` plus the exact SQL that clears it — and count it for
    /// `GET /operations/metrics` (#1236).
    fn report_unreconcilable_run(&self, run_id: RunId, session_id: SessionId, error: &AlmsError) {
        self.record_stale_run_recovery_failure();
        tracing::error!(
            run_id = %run_id.0,
            session_id = %session_id.0,
            %error,
            remediation = %Self::stale_run_remediation_sql(run_id, session_id),
            "Stale run could not be reconciled at startup — skipped so the daemon can boot. \
             The row still claims queued/running from a dead process and will never complete; \
             clear it with the `remediation` SQL after stopping the daemon (#1236)"
        );
    }

    /// The SQL an operator runs to clear one unreconcilable stale run.
    ///
    /// Two details that are easy to get wrong and are the whole point of
    /// generating this rather than writing it in a runbook:
    ///
    /// - `lifecycle_revision` is capped with `MIN(..., i64::MAX)`. Revision
    ///   exhaustion at `MAX_LIFECYCLE_REVISION` (`i64::MAX`) is one of the
    ///   ways a row becomes unreconcilable in the first place, and a bare
    ///   `+ 1` on `i64::MAX` overflows to a REAL in SQLite, after which the
    ///   column no longer reads back as an integer revision.
    /// - The `json_valid` guard is inside each `json_extract` via `CASE`, not
    ///   a preceding `AND` term, for the same reason
    ///   [`Self::settle_pending_input_on`] does it: SQLite does not promise a
    ///   `WHERE` evaluation order, so a preceding guard can be evaluated after
    ///   the `json_extract` it is supposed to protect and abort the operator's
    ///   remediation on an unrelated malformed row in the same session.
    fn stale_run_remediation_sql(run_id: RunId, session_id: SessionId) -> String {
        let run_id = run_id.0;
        let session_id = session_id.0;
        format!(
            "UPDATE runs SET status = 'failed', error = 'stale: gateway restarted', \
             terminal_reason = 'gateway_restarted', ended_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), \
             lifecycle_revision = MIN(lifecycle_revision + 1, 9223372036854775807) \
             WHERE run_id = '{run_id}'; \
             UPDATE messages SET metadata = json_set(json_set(metadata, '$.pending_input', json('false')), \
             '$.input_claimed_at', strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
             WHERE session_id = '{session_id}' AND metadata IS NOT NULL \
             AND json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{{}}' END, '$.run_id') = '{run_id}' \
             AND json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{{}}' END, '$.pending_input') = 1;"
        )
    }

    /// Release the hidden `pending_input` prompt owned by one run.
    ///
    /// One statement, evaluated inside SQLite (#1238 S6). The sibling
    /// `terminal_runs_with_pending_input` query keeps messages as the outer
    /// loop specifically so startup performs one history scan rather than one
    /// per terminal run; the previous Rust-side implementation here read and
    /// JSON-parsed every metadata-bearing message in the session once per
    /// recovered run, reintroducing exactly what that comment warns against.
    /// The `json_extract` predicate is the same shape the sibling query
    /// already uses, including the `json_valid` CASE guard — SQLite does not
    /// promise a `WHERE` evaluation order, so the guard has to be inside each
    /// `json_extract` rather than a preceding `AND` term.
    fn settle_pending_input_on(
        conn: &Connection,
        session_id: SessionId,
        run_id: RunId,
        claimed_at: &str,
    ) -> AlmsResult<usize> {
        conn.execute(
            r#"UPDATE messages
                  SET metadata = json_set(
                        json_set(metadata, '$.pending_input', json('false')),
                        '$.input_claimed_at', ?3)
                WHERE session_id = ?1
                  AND metadata IS NOT NULL
                  AND json_extract(
                        CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                        '$.run_id'
                      ) = ?2
                  AND json_extract(
                        CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                        '$.pending_input'
                      ) = 1"#,
            params![session_id.0.to_string(), run_id.0.to_string(), claimed_at],
        )
        .map_err(|error| AlmsError::Runtime(format!("SQLite settle pending input: {error}")))
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
        assert_eq!(store.persistence_snapshot_rejections_total(), 1);
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
        store.save_session(&session).unwrap();

        // Insert a queued run and a running run.
        let queued_run = new_run(session.id, session.agent_id);
        let queued_id = queued_run.run_id;
        let queued_message = Message {
            id: "stale-queued-input".to_string(),
            role: Role::User,
            content: Content::Text(queued_run.input.clone()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": queued_id.0.to_string(),
            })),
        };
        store
            .save_run_with_initial_message(&queued_run, &queued_message)
            .unwrap();

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
        let recovered_messages = store.load_messages(session.id).unwrap();
        let recovered_input = recovered_messages
            .iter()
            .find(|message| message.id == queued_message.id)
            .expect("queued input must remain durable");
        let metadata = recovered_input.metadata.as_ref().unwrap();
        assert_eq!(metadata["pending_input"], false);
        assert!(metadata["input_claimed_at"].is_string());

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

    /// #1236: an unreconcilable row rolls back to its own savepoint — the run
    /// keeps its pre-sweep status and the prompt is not partially released —
    /// but the sweep itself succeeds so the daemon still boots. Pre-#1236 the
    /// whole sweep aborted and `Gateway::new` propagated the error.
    #[test]
    fn stale_run_recovery_skips_the_unreconcilable_row_and_still_boots() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        let run = new_run(session.id, session.agent_id);
        let message = Message {
            id: "recovery-rollback-input".to_string(),
            role: Role::User,
            content: Content::Text(run.input.clone()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": run.run_id.0.to_string(),
            })),
        };
        store.save_run_with_initial_message(&run, &message).unwrap();

        // A second stale run with no injected fault — it must still recover.
        let mut healthy = new_run(session.id, session.agent_id);
        healthy.mark_running();
        let healthy_id = healthy.run_id;
        store.save_run(&healthy).unwrap();

        store
            .conn
            .lock()
            .execute_batch(
                "CREATE TEMP TRIGGER reject_pending_input_recovery
                 BEFORE UPDATE OF metadata ON messages
                 WHEN OLD.id = 'recovery-rollback-input'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected pending-input recovery failure');
                 END;",
            )
            .unwrap();

        let recovered = store
            .mark_stale_runs_failed()
            .expect("one bad row must not stop startup recovery");
        assert_eq!(recovered, 1, "the healthy stale run still recovers");
        assert_eq!(store.stale_run_recovery_failures_total(), 1);

        assert!(
            matches!(
                store.load_run(run.run_id).unwrap().unwrap().status(),
                RunStatus::Queued
            ),
            "the skipped row keeps its pre-sweep status"
        );
        assert!(matches!(
            store.load_run(healthy_id).unwrap().unwrap().status(),
            RunStatus::Failed
        ));
        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(
            messages[0].metadata.as_ref().unwrap()["pending_input"],
            true
        );
        assert!(
            messages[0].metadata.as_ref().unwrap()["input_claimed_at"].is_null(),
            "the rolled-back savepoint must not partially release the prompt"
        );
    }

    /// #1236: the remediation line an operator gets must be actionable on its
    /// own — it names the run and carries runnable SQL for both halves of the
    /// split state (the run row and its hidden pending input).
    ///
    /// Exercised against the two conditions that actually produce an
    /// unreconcilable row rather than an empty schema: a run whose lifecycle
    /// revision is already exhausted at `i64::MAX` (a bare `+ 1` there
    /// overflows to a REAL), and a sibling message with malformed JSON
    /// metadata in the same session (a preceding `AND json_valid(...)` guard
    /// can be evaluated after the `json_extract` it protects and abort).
    #[test]
    fn unreconcilable_run_remediation_sql_clears_the_row_it_names() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let stuck = Run::from_persisted(
            RunId::new(),
            session.id,
            session.agent_id,
            RunStatus::Queued,
            "stuck".to_string(),
            None,
            None,
            None,
            chrono::Utc::now(),
            None,
            None,
            None,
            None,
            None,
            alms_core::MAX_LIFECYCLE_REVISION,
            None,
        );
        store.save_run(&stuck).unwrap();
        store
            .save_message(
                session.id,
                &Message {
                    id: "stuck-input".to_string(),
                    role: Role::User,
                    content: Content::Text("stuck".to_string()),
                    timestamp: Timestamp::now(),
                    metadata: Some(serde_json::json!({
                        "pending_input": true,
                        "run_id": stuck.run_id.0.to_string(),
                    })),
                },
            )
            .unwrap();
        // A sibling row in the same session whose metadata is not valid JSON.
        store
            .save_message(
                session.id,
                &Message {
                    id: "malformed-neighbour".to_string(),
                    role: Role::Assistant,
                    content: Content::Text("noise".to_string()),
                    timestamp: Timestamp::now(),
                    metadata: Some(serde_json::json!({"unrelated": true})),
                },
            )
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "UPDATE messages SET metadata = '{not valid json' WHERE id = ?1",
                params!["malformed-neighbour"],
            )
            .unwrap();

        let sql = SqliteStore::stale_run_remediation_sql(stuck.run_id, session.id);
        assert!(sql.contains(&stuck.run_id.0.to_string()));
        assert!(sql.contains(&session.id.0.to_string()));

        store
            .conn
            .lock()
            .execute_batch(&sql)
            .expect("remediation SQL must survive an exhausted revision and malformed neighbour");

        let cleared = store.load_run(stuck.run_id).unwrap().unwrap();
        assert_eq!(
            cleared.status(),
            RunStatus::Failed,
            "the remediation must actually clear the row it names"
        );
        assert_eq!(cleared.terminal_reason(), Some("gateway_restarted"));
        assert_eq!(
            cleared.lifecycle_revision(),
            alms_core::MAX_LIFECYCLE_REVISION,
            "an exhausted revision must saturate, not overflow into a REAL"
        );

        let messages = store.load_messages(session.id).unwrap();
        let input = messages
            .iter()
            .find(|message| message.id == "stuck-input")
            .expect("the pending input must still exist");
        let metadata = input.metadata.as_ref().unwrap();
        assert_eq!(metadata["pending_input"], false);
        assert!(metadata["input_claimed_at"].is_string());
    }

    /// The messages half of the remediation must not re-stamp a message that
    /// was already claimed — it carries the same `pending_input = 1` predicate
    /// the sweep's own `settle_pending_input_on` uses.
    #[test]
    fn remediation_sql_does_not_restamp_already_claimed_input() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        let run = new_run(session.id, session.agent_id);
        store
            .save_message(
                session.id,
                &Message {
                    id: "already-claimed".to_string(),
                    role: Role::User,
                    content: Content::Text("hello".to_string()),
                    timestamp: Timestamp::now(),
                    metadata: Some(serde_json::json!({
                        "pending_input": false,
                        "input_claimed_at": "2020-01-01T00:00:00Z",
                        "run_id": run.run_id.0.to_string(),
                    })),
                },
            )
            .unwrap();

        let sql = SqliteStore::stale_run_remediation_sql(run.run_id, session.id);
        store.conn.lock().execute_batch(&sql).unwrap();

        let messages = store.load_messages(session.id).unwrap();
        let metadata = messages[0].metadata.as_ref().unwrap();
        assert_eq!(
            metadata["input_claimed_at"], "2020-01-01T00:00:00Z",
            "an already-claimed input must keep its original claim timestamp"
        );
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

    #[test]
    fn run_admission_commits_run_message_and_activity_together() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        let run = new_run(session.id, session.agent_id);
        let message = Message {
            id: "admission-message".to_string(),
            role: Role::User,
            content: Content::Text(run.input.clone()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": run.run_id.0.to_string(),
            })),
        };

        let touched_at = store.save_run_with_initial_message(&run, &message).unwrap();

        assert!(store.load_run(run.run_id).unwrap().is_some());
        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message.id);
        assert_eq!(
            store
                .load_session_by_id(session.id)
                .unwrap()
                .unwrap()
                .last_activity,
            touched_at
        );

        let mut stale_session = session.clone();
        stale_session.last_activity = Timestamp(touched_at.0 - chrono::Duration::hours(1));
        store.save_session(&stale_session).unwrap();
        assert_eq!(
            store
                .load_session_by_id(session.id)
                .unwrap()
                .unwrap()
                .last_activity,
            touched_at,
            "a delayed stale session snapshot must not regress activity"
        );
    }

    #[test]
    fn run_admission_rolls_back_run_when_message_write_fails() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        let run = new_run(session.id, session.agent_id);
        let message = Message {
            id: "rejected-admission-message".to_string(),
            role: Role::User,
            content: Content::Text(run.input.clone()),
            timestamp: Timestamp::now(),
            metadata: None,
        };
        store
            .conn
            .lock()
            .execute_batch(
                "CREATE TRIGGER reject_admission_message
                 BEFORE INSERT ON messages
                 BEGIN
                     SELECT RAISE(ABORT, 'injected admission message failure');
                 END;",
            )
            .unwrap();

        let error = store
            .save_run_with_initial_message(&run, &message)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected admission message failure")
        );
        assert!(
            store.load_run(run.run_id).unwrap().is_none(),
            "the run insert must roll back with the rejected message"
        );
        assert!(
            store.load_messages(session.id).unwrap().is_empty(),
            "the transaction must not expose a partial initial message"
        );
        assert_eq!(
            store
                .load_session_by_id(session.id)
                .unwrap()
                .unwrap()
                .last_activity,
            session.last_activity
        );
    }

    #[test]
    fn run_admission_rejects_duplicate_run_without_committing_message() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        let run = new_run(session.id, session.agent_id);
        store.save_run(&run).unwrap();
        let message = Message {
            id: "duplicate-run-message".to_string(),
            role: Role::User,
            content: Content::Text(run.input.clone()),
            timestamp: Timestamp::now(),
            metadata: None,
        };

        let error = store
            .save_run_with_initial_message(&run, &message)
            .unwrap_err();
        assert!(error.to_string().contains("duplicate or stale run"));
        assert!(store.load_messages(session.id).unwrap().is_empty());
        assert_eq!(
            store
                .load_session_by_id(session.id)
                .unwrap()
                .unwrap()
                .last_activity,
            session.last_activity
        );
        assert_eq!(store.persistence_snapshot_rejections_total(), 1);
    }
}
