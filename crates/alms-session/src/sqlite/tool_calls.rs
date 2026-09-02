//! Per-run tool call records.

use super::*;

impl SqliteStore {
    // ── Run Tool Calls ────────────────────────────────────────────────────

    /// Persist a single tool call record for a run.
    ///
    /// `session_id` is stored alongside `run_id` (B9(b), #1154) so the row
    /// stays attributable to its session even if the `runs` row is later
    /// removed — [`Self::load_tool_calls_for_session`] no longer depends on a
    /// surviving `runs` join to find it.
    pub fn save_tool_call(
        &self,
        run_id: RunId,
        session_id: SessionId,
        record: &ToolCallRecord,
    ) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO run_tool_calls \
                 (run_id, session_id, seq, role, tool_name, tool_id, params, result, \
                  timestamp, from_agent, tool_invocation_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    run_id.0.to_string(),
                    session_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
                    record.from_agent.as_deref(),
                    record.tool_invocation_id.as_deref(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_tool_call: {e}")))?;
        Ok(())
    }

    /// Persist a batch of tool call records for a run in a single transaction.
    ///
    /// `session_id` is stored on every row — see [`Self::save_tool_call`].
    pub fn save_tool_calls(
        &self,
        run_id: RunId,
        session_id: SessionId,
        records: &[ToolCallRecord],
    ) -> AlmsResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin save_tool_calls: {e}")))?;
        for record in records {
            tx.execute(
                "INSERT INTO run_tool_calls \
                 (run_id, session_id, seq, role, tool_name, tool_id, params, result, \
                  timestamp, from_agent, tool_invocation_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    run_id.0.to_string(),
                    session_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
                    record.from_agent.as_deref(),
                    record.tool_invocation_id.as_deref(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_tool_call batch: {e}")))?;
        }
        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit save_tool_calls: {e}")))?;
        Ok(())
    }

    /// Load all tool call records for a run, ordered by sequence number.
    pub fn load_tool_calls(&self, run_id: RunId) -> AlmsResult<Vec<ToolCallRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT seq, role, tool_name, tool_id, params, result, timestamp, \
                        from_agent, tool_invocation_id \
                 FROM run_tool_calls WHERE run_id = ?1 ORDER BY seq",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare load_tool_calls: {e}")))?;

        let rows = stmt
            .query_map([run_id.0.to_string()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_tool_calls: {e}")))?
            .filter_map(|r| match r {
                Ok((
                    seq,
                    role_str,
                    tool_name,
                    tool_id,
                    params,
                    result,
                    ts_str,
                    from_agent,
                    tool_invocation_id,
                )) => {
                    let role: ToolCallRole = role_str
                        .parse()
                        .inspect_err(|e| {
                            self.record_skipped_row(
                                PersistenceTable::RunToolCalls,
                                format_args!("run {}: bad role: {e}", run_id.0),
                            );
                        })
                        .ok()?;
                    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                        .inspect_err(|e| {
                            self.record_skipped_row(
                                PersistenceTable::RunToolCalls,
                                format_args!("run {}: bad timestamp: {e}", run_id.0),
                            );
                        })
                        .ok()?
                        .with_timezone(&chrono::Utc);
                    Some(ToolCallRecord {
                        seq: seq.max(0) as u32,
                        role,
                        tool_name,
                        tool_id,
                        params,
                        result,
                        timestamp,
                        from_agent,
                        tool_invocation_id,
                    })
                }
                Err(e) => {
                    self.record_skipped_row(
                        PersistenceTable::RunToolCalls,
                        format_args!("run {}: {e}", run_id.0),
                    );
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Count tool call records for a run without loading them.
    pub fn count_tool_calls(&self, run_id: RunId) -> AlmsResult<u32> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tool_calls WHERE run_id = ?1",
                params![run_id.0.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite count_tool_calls: {e}")))?;
        Ok(count.max(0) as u32)
    }

    /// Load all tool call records for a session, ordered by run creation time
    /// then sequence number.
    ///
    /// Each returned [`SessionToolCall`] includes the `run_id` so the frontend
    /// can group or correlate calls with their originating run.
    ///
    /// B9(b) (#1154): rows are matched on the `run_tool_calls.session_id`
    /// column and the `runs` table is **LEFT JOIN**ed only to recover the
    /// `created_at` order key. The previous design INNER-JOINed `runs` and
    /// filtered on `runs.session_id`, so a tool-call row whose `runs` row was
    /// gone (e.g. a partial write where the run insert rolled back, or a
    /// future delete path that prunes runs without their tool calls) was
    /// silently dropped — and its call then could not be grouped into the
    /// collapsible DM reasoning block on reload. Now such a row survives: it
    /// matches via `tc.session_id`, the projection tolerates the missing run
    /// row (its `created_at` is NULL and it simply sorts last), and `tc.run_id`
    /// — which is `NOT NULL` — keeps grouping intact.
    ///
    /// Backward compatibility: rows written before the `session_id` column
    /// existed have `tc.session_id IS NULL`; they are still matched through
    /// the surviving `runs` join (`r.session_id = ?1`), so no historical row
    /// is lost.
    pub fn load_tool_calls_for_session(
        &self,
        session_id: SessionId,
    ) -> AlmsResult<Vec<SessionToolCall>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT tc.run_id, tc.seq, tc.role, tc.tool_name, tc.tool_id, \
                        tc.params, tc.result, tc.timestamp, tc.from_agent, \
                        tc.tool_invocation_id \
                 FROM run_tool_calls tc \
                 LEFT JOIN runs r ON tc.run_id = r.run_id \
                 WHERE tc.session_id = ?1 \
                    OR (tc.session_id IS NULL AND r.session_id = ?1) \
                 ORDER BY r.created_at IS NULL, r.created_at, tc.seq",
            )
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite prepare load_tool_calls_for_session: {e}"))
            })?;

        let rows = stmt
            .query_map([session_id.0.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite query load_tool_calls_for_session: {e}"))
            })?
            .filter_map(|r| match r {
                Ok((
                    run_id_str,
                    seq,
                    role_str,
                    tool_name,
                    tool_id,
                    params,
                    result,
                    ts_str,
                    from_agent,
                    tool_invocation_id,
                )) => {
                    let run_id: RunId = uuid::Uuid::parse_str(&run_id_str)
                        .inspect_err(|e| {
                            self.record_skipped_row(
                                PersistenceTable::RunToolCalls,
                                format_args!("session {}: bad run_id: {e}", session_id.0),
                            );
                        })
                        .ok()
                        .map(RunId)?;
                    let role: ToolCallRole = role_str
                        .parse()
                        .inspect_err(|e| {
                            self.record_skipped_row(
                                PersistenceTable::RunToolCalls,
                                format_args!("session {}: bad role: {e}", session_id.0),
                            );
                        })
                        .ok()?;
                    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                        .inspect_err(|e| {
                            self.record_skipped_row(
                                PersistenceTable::RunToolCalls,
                                format_args!("session {}: bad timestamp: {e}", session_id.0),
                            );
                        })
                        .ok()?
                        .with_timezone(&chrono::Utc);
                    Some(SessionToolCall {
                        run_id,
                        record: ToolCallRecord {
                            seq: seq.max(0) as u32,
                            role,
                            tool_name,
                            tool_id,
                            params,
                            result,
                            timestamp,
                            from_agent,
                            tool_invocation_id,
                        },
                    })
                }
                Err(e) => {
                    self.record_skipped_row(
                        PersistenceTable::RunToolCalls,
                        format_args!("session {}: {e}", session_id.0),
                    );
                    None
                }
            })
            .collect();

        Ok(rows)
    }
}

/// A tool call record enriched with its originating `run_id`, returned by
/// [`SqliteStore::load_tool_calls_for_session`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionToolCall {
    pub run_id: RunId,
    #[serde(flatten)]
    pub record: ToolCallRecord,
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use alms_core::run::{ToolCallRecord, ToolCallRole};

    fn new_tool_call_record(seq: u32, role: ToolCallRole, name: &str) -> ToolCallRecord {
        ToolCallRecord {
            seq,
            role,
            tool_name: Some(name.to_string()),
            tool_id: Some(format!("call_{seq}")),
            tool_invocation_id: None,
            params: if role == ToolCallRole::Assistant {
                Some(r#"{"text":"hello"}"#.to_string())
            } else {
                None
            },
            result: if role == ToolCallRole::Tool {
                Some(r#""result_ok""#.to_string())
            } else {
                None
            },
            timestamp: chrono::Utc::now(),
            from_agent: None,
        }
    }

    /// #5: the correlator survives both write paths and both read paths.
    ///
    /// There are two inserters (`save_tool_call`, `save_tool_calls`) and two
    /// loaders (`load_tool_calls`, `load_tool_calls_for_session`), each with
    /// its own hand-written column list and its own positional row decoder.
    /// A column added to three of the four is a silent data-loss bug that no
    /// type check catches — the tuple indices still line up, they just read
    /// the wrong thing or nothing. So all four combinations are exercised.
    #[test]
    fn tool_invocation_id_survives_both_writers_and_both_loaders() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();

        // Writer 1: the singular insert.
        let single_run = RunId::new();
        let mut single = new_tool_call_record(0, ToolCallRole::Assistant, "echo");
        single.tool_invocation_id = Some("inv-single".to_string());
        store
            .save_tool_call(single_run, session_id, &single)
            .unwrap();

        // Writer 2: the batch insert.
        let batch_run = RunId::new();
        let mut call = new_tool_call_record(0, ToolCallRole::Assistant, "echo");
        call.tool_invocation_id = Some("inv-batch-call".to_string());
        let mut result = new_tool_call_record(1, ToolCallRole::Tool, "echo");
        result.tool_invocation_id = Some("inv-batch-result".to_string());
        store
            .save_tool_calls(batch_run, session_id, &[call, result])
            .unwrap();

        // Loader 1, over each writer's rows.
        assert_eq!(
            store.load_tool_calls(single_run).unwrap()[0]
                .tool_invocation_id
                .as_deref(),
            Some("inv-single"),
            "save_tool_call -> load_tool_calls",
        );
        let batch_loaded = store.load_tool_calls(batch_run).unwrap();
        assert_eq!(
            batch_loaded
                .iter()
                .map(|r| r.tool_invocation_id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("inv-batch-call"), Some("inv-batch-result")],
            "save_tool_calls -> load_tool_calls",
        );

        // Loader 2 spans both runs, since both were written under one session.
        let by_session = store.load_tool_calls_for_session(session_id).unwrap();
        let mut correlators: Vec<&str> = by_session
            .iter()
            .filter_map(|c| c.record.tool_invocation_id.as_deref())
            .collect();
        correlators.sort();
        assert_eq!(
            correlators,
            vec!["inv-batch-call", "inv-batch-result", "inv-single"],
            "every row must surface its correlator through the session loader",
        );

        // `None` is a legitimate value (pre-#5 rows), not an error.
        let null_run = RunId::new();
        let bare = new_tool_call_record(0, ToolCallRole::Assistant, "echo");
        assert!(bare.tool_invocation_id.is_none());
        store.save_tool_call(null_run, session_id, &bare).unwrap();
        assert_eq!(
            store.load_tool_calls(null_run).unwrap()[0].tool_invocation_id,
            None,
        );
    }

    #[test]
    fn test_save_and_load_tool_calls() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let records = vec![
            new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            new_tool_call_record(1, ToolCallRole::Tool, "echo"),
            new_tool_call_record(2, ToolCallRole::Assistant, "math"),
            new_tool_call_record(3, ToolCallRole::Tool, "math"),
        ];

        store.save_tool_calls(run_id, session_id, &records).unwrap();
        let loaded = store.load_tool_calls(run_id).unwrap();

        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded[0].seq, 0);
        assert_eq!(loaded[0].role, ToolCallRole::Assistant);
        assert_eq!(loaded[0].tool_name.as_deref(), Some("echo"));
        assert_eq!(loaded[1].seq, 1);
        assert_eq!(loaded[1].role, ToolCallRole::Tool);
        assert!(loaded[1].result.is_some());
        assert_eq!(loaded[2].tool_name.as_deref(), Some("math"));
        assert_eq!(loaded[3].seq, 3);
    }

    #[test]
    fn test_count_tool_calls() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        assert_eq!(store.count_tool_calls(run_id).unwrap(), 0);

        let records = vec![
            new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            new_tool_call_record(1, ToolCallRole::Tool, "echo"),
        ];
        store.save_tool_calls(run_id, session_id, &records).unwrap();

        assert_eq!(store.count_tool_calls(run_id).unwrap(), 2);
    }

    #[test]
    fn test_tool_calls_isolated_by_run() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run1 = RunId::new();
        let run2 = RunId::new();
        let session_id = SessionId::new();

        store
            .save_tool_call(
                run1,
                session_id,
                &new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            )
            .unwrap();
        store
            .save_tool_call(
                run2,
                session_id,
                &new_tool_call_record(0, ToolCallRole::Assistant, "math"),
            )
            .unwrap();
        store
            .save_tool_call(
                run2,
                session_id,
                &new_tool_call_record(1, ToolCallRole::Tool, "math"),
            )
            .unwrap();

        assert_eq!(store.load_tool_calls(run1).unwrap().len(), 1);
        assert_eq!(store.load_tool_calls(run2).unwrap().len(), 2);
        assert_eq!(store.count_tool_calls(run1).unwrap(), 1);
        assert_eq!(store.count_tool_calls(run2).unwrap(), 2);
    }

    #[test]
    fn test_save_tool_calls_empty_batch() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();
        let session_id = SessionId::new();
        // Empty batch should succeed without error.
        store.save_tool_calls(run_id, session_id, &[]).unwrap();
        assert_eq!(store.load_tool_calls(run_id).unwrap().len(), 0);
    }

    #[test]
    fn test_tool_call_record_nullable_columns_roundtrip() {
        // N4: verify that assistant records with params=None and tool records
        // with result=None round-trip correctly through SQLite.
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();
        let session_id = SessionId::new();

        let records = vec![
            ToolCallRecord {
                seq: 0,
                role: ToolCallRole::Assistant,
                tool_name: Some("shell_exec".to_string()),
                tool_id: Some("call_0".to_string()),
                tool_invocation_id: None,
                params: None, // Empty-args tool call
                result: None,
                timestamp: chrono::Utc::now(),
                from_agent: None,
            },
            ToolCallRecord {
                seq: 1,
                role: ToolCallRole::Tool,
                tool_name: Some("shell_exec".to_string()),
                tool_id: Some("call_0".to_string()),
                tool_invocation_id: None,
                params: None,
                result: None, // Tool returned nothing
                timestamp: chrono::Utc::now(),
                from_agent: None,
            },
        ];

        store.save_tool_calls(run_id, session_id, &records).unwrap();
        let loaded = store.load_tool_calls(run_id).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, ToolCallRole::Assistant);
        assert!(loaded[0].params.is_none());
        assert!(loaded[0].result.is_none());
        assert_eq!(loaded[1].role, ToolCallRole::Tool);
        assert!(loaded[1].params.is_none());
        assert!(loaded[1].result.is_none());
    }

    #[test]
    fn test_load_tool_calls_for_session_basic() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let agent_id = alms_core::AgentId::new();

        // Create two runs on the same session.
        let run1 = alms_core::Run::new(session_id, agent_id, "prompt1".to_string());
        let run2 = alms_core::Run::new(session_id, agent_id, "prompt2".to_string());
        store.save_run(&run1).unwrap();
        store.save_run(&run2).unwrap();

        // Save tool calls for each run.
        store
            .save_tool_calls(
                run1.run_id,
                session_id,
                &[
                    new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
                    new_tool_call_record(1, ToolCallRole::Tool, "echo"),
                ],
            )
            .unwrap();
        store
            .save_tool_calls(
                run2.run_id,
                session_id,
                &[new_tool_call_record(0, ToolCallRole::Assistant, "math")],
            )
            .unwrap();

        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert_eq!(
            session_calls.len(),
            3,
            "should load all tool calls across both runs"
        );

        // First two should be from run1.
        assert_eq!(session_calls[0].run_id, run1.run_id);
        assert_eq!(session_calls[0].record.tool_name.as_deref(), Some("echo"));
        assert_eq!(session_calls[1].run_id, run1.run_id);
        // Third from run2.
        assert_eq!(session_calls[2].run_id, run2.run_id);
        assert_eq!(session_calls[2].record.tool_name.as_deref(), Some("math"));
    }

    #[test]
    fn test_load_tool_calls_for_session_empty() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();

        // No runs exist for this session.
        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert!(session_calls.is_empty());
    }

    #[test]
    fn test_load_tool_calls_for_session_isolates_sessions() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = alms_core::AgentId::new();

        let session_a = SessionId::new();
        let session_b = SessionId::new();

        let run_a = alms_core::Run::new(session_a, agent_id, "prompt_a".to_string());
        let run_b = alms_core::Run::new(session_b, agent_id, "prompt_b".to_string());
        store.save_run(&run_a).unwrap();
        store.save_run(&run_b).unwrap();

        store
            .save_tool_call(
                run_a.run_id,
                session_a,
                &new_tool_call_record(0, ToolCallRole::Assistant, "fs_read"),
            )
            .unwrap();
        store
            .save_tool_call(
                run_b.run_id,
                session_b,
                &new_tool_call_record(0, ToolCallRole::Assistant, "shell"),
            )
            .unwrap();

        let calls_a = store.load_tool_calls_for_session(session_a).unwrap();
        assert_eq!(calls_a.len(), 1);
        assert_eq!(calls_a[0].record.tool_name.as_deref(), Some("fs_read"));

        let calls_b = store.load_tool_calls_for_session(session_b).unwrap();
        assert_eq!(calls_b.len(), 1);
        assert_eq!(calls_b[0].record.tool_name.as_deref(), Some("shell"));
    }

    /// Verify that `from_agent` is persisted and round-trips through both
    /// `load_tool_calls` (per-run) and `load_tool_calls_for_session`
    /// (session-level). This supports the frontend fallback merge path so
    /// DM reasoning blocks can be attributed to the correct agent. (#696)
    #[test]
    fn test_tool_call_from_agent_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let agent_id = alms_core::AgentId::new();
        let run = alms_core::Run::new(session_id, agent_id, "hi".to_string());
        store.save_run(&run).unwrap();

        let record = ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("call_0".to_string()),
            tool_invocation_id: None,
            params: Some(r#"{"text":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: Some("alice".to_string()),
        };
        store
            .save_tool_call(run.run_id, session_id, &record)
            .unwrap();

        let loaded = store.load_tool_calls(run.run_id).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].from_agent.as_deref(), Some("alice"));

        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert_eq!(session_calls.len(), 1);
        assert_eq!(session_calls[0].record.from_agent.as_deref(), Some("alice"));
    }

    /// B9(b) (#1154): a tool-call row whose `runs` row is gone must still be
    /// returned by `load_tool_calls_for_session` so it can be grouped into
    /// the collapsible DM reasoning block on reload. Pre-fix the INNER JOIN
    /// on `runs` dropped it. The row is matched via its own `session_id`
    /// column; the LEFT JOIN tolerates the missing run row (its `created_at`
    /// is NULL and it sorts last).
    #[test]
    fn test_load_tool_calls_for_session_survives_missing_run_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let agent_id = alms_core::AgentId::new();

        // Two runs on the same session. `gone_run` will have its `runs` row
        // deleted out from under its tool calls; `live_run` keeps its row.
        let gone_run = alms_core::Run::new(session_id, agent_id, "gone".to_string());
        let live_run = alms_core::Run::new(session_id, agent_id, "live".to_string());
        store.save_run(&gone_run).unwrap();
        store.save_run(&live_run).unwrap();

        store
            .save_tool_calls(
                gone_run.run_id,
                session_id,
                &[new_tool_call_record(0, ToolCallRole::Assistant, "fs_read")],
            )
            .unwrap();
        store
            .save_tool_calls(
                live_run.run_id,
                session_id,
                &[new_tool_call_record(0, ToolCallRole::Assistant, "echo")],
            )
            .unwrap();

        // Delete the `runs` row for `gone_run` WITHOUT touching its tool
        // calls — simulates a partial write / crash where the run insert was
        // rolled back but the tool-call rows survived.
        store
            .conn
            .lock()
            .execute(
                "DELETE FROM runs WHERE run_id = ?1",
                params![gone_run.run_id.0.to_string()],
            )
            .unwrap();

        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert_eq!(
            session_calls.len(),
            2,
            "the orphaned tool-call row must survive the missing run row (LEFT JOIN)"
        );

        // Both runs' calls are present and grouped by their own run_id.
        let run_ids: Vec<_> = session_calls.iter().map(|c| c.run_id).collect();
        assert!(
            run_ids.contains(&gone_run.run_id),
            "the orphaned row keeps its run_id for grouping"
        );
        assert!(run_ids.contains(&live_run.run_id));

        // The orphaned row (NULL created_at) sorts last.
        assert_eq!(
            session_calls[1].run_id, gone_run.run_id,
            "the row with the missing run row sorts after live rows"
        );
        assert_eq!(
            session_calls[1].record.tool_name.as_deref(),
            Some("fs_read")
        );
    }

    /// B9(b) backward compatibility: a legacy row with `session_id IS NULL`
    /// (written before the column existed) is still found via the surviving
    /// `runs` join, so no historical tool call is lost.
    #[test]
    fn test_load_tool_calls_for_session_finds_legacy_null_session_id_rows() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let agent_id = alms_core::AgentId::new();

        let run = alms_core::Run::new(session_id, agent_id, "legacy".to_string());
        store.save_run(&run).unwrap();

        // Insert a row the old way: no `session_id` column value (NULL).
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO run_tool_calls \
                 (run_id, seq, role, tool_name, tool_id, params, result, timestamp) \
                 VALUES (?1, 0, 'assistant', 'echo', 'call_0', NULL, NULL, ?2)",
                params![run.run_id.0.to_string(), chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();

        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert_eq!(
            session_calls.len(),
            1,
            "a legacy NULL-session_id row must still be found via the runs join"
        );
        assert_eq!(session_calls[0].run_id, run.run_id);
    }

    /// #1241: the tool-call loaders drop rows from *inside* a successful
    /// `query_map` (bad role, bad timestamp) rather than from the `Err` arm.
    /// Those inner drops are counted too — they are the same silent loss.
    #[test]
    fn corrupt_tool_call_row_is_dropped_and_counted() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();
        let session_id = SessionId::new();
        store
            .save_tool_calls(
                run_id,
                session_id,
                &[new_tool_call_record(0, ToolCallRole::Assistant, "echo")],
            )
            .unwrap();
        assert_eq!(store.load_tool_calls(run_id).unwrap().len(), 1);

        crate::sqlite::test_helpers::corrupt_with_sql(
            &store,
            "UPDATE run_tool_calls SET role = 'not-a-role'",
        );

        assert!(store.load_tool_calls(run_id).unwrap().is_empty());
        assert!(
            store
                .load_tool_calls_for_session(session_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.rows_skipped_for(PersistenceTable::RunToolCalls), 2);
        assert_eq!(store.rows_skipped_total(), 2);
    }
}
