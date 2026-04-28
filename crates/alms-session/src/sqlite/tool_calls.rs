//! Per-run tool call records.

use super::*;

impl SqliteStore {
    // ── Run Tool Calls ────────────────────────────────────────────────────

    /// Persist a single tool call record for a run.
    pub fn save_tool_call(&self, run_id: RunId, record: &ToolCallRecord) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO run_tool_calls \
                 (run_id, seq, role, tool_name, tool_id, params, result, timestamp, from_agent) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    run_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
                    record.from_agent.as_deref(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_tool_call: {e}")))?;
        Ok(())
    }

    /// Persist a batch of tool call records for a run in a single transaction.
    pub fn save_tool_calls(&self, run_id: RunId, records: &[ToolCallRecord]) -> AlmsResult<()> {
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
                 (run_id, seq, role, tool_name, tool_id, params, result, timestamp, from_agent) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    run_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
                    record.from_agent.as_deref(),
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
                "SELECT seq, role, tool_name, tool_id, params, result, timestamp, from_agent \
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
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_tool_calls: {e}")))?
            .filter_map(|r| match r {
                Ok((seq, role_str, tool_name, tool_id, params, result, ts_str, from_agent)) => {
                    let role: ToolCallRole = role_str
                        .parse()
                        .inspect_err(|e| {
                            tracing::warn!(
                                run_id = %run_id.0,
                                "Skipping tool call record: bad role: {e}"
                            );
                        })
                        .ok()?;
                    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                        .inspect_err(|e| {
                            tracing::warn!(
                                run_id = %run_id.0,
                                "Skipping tool call record: bad timestamp: {e}"
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
                    })
                }
                Err(e) => {
                    tracing::warn!("Skipping unparseable tool call row: {e}");
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

    /// Load all tool call records for a session by joining through the `runs`
    /// table, ordered by run creation time then sequence number.
    ///
    /// Each returned [`SessionToolCall`] includes the `run_id` so the frontend
    /// can group or correlate calls with their originating run.
    pub fn load_tool_calls_for_session(
        &self,
        session_id: SessionId,
    ) -> AlmsResult<Vec<SessionToolCall>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT tc.run_id, tc.seq, tc.role, tc.tool_name, tc.tool_id, \
                        tc.params, tc.result, tc.timestamp, tc.from_agent \
                 FROM run_tool_calls tc \
                 INNER JOIN runs r ON tc.run_id = r.run_id \
                 WHERE r.session_id = ?1 \
                 ORDER BY r.created_at, tc.seq",
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
                )) => {
                    let run_id: RunId = uuid::Uuid::parse_str(&run_id_str)
                        .inspect_err(|e| {
                            tracing::warn!(
                                session_id = %session_id.0,
                                "Skipping tool call record: bad run_id: {e}"
                            );
                        })
                        .ok()
                        .map(RunId)?;
                    let role: ToolCallRole = role_str
                        .parse()
                        .inspect_err(|e| {
                            tracing::warn!(
                                session_id = %session_id.0,
                                "Skipping tool call record: bad role: {e}"
                            );
                        })
                        .ok()?;
                    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts_str)
                        .inspect_err(|e| {
                            tracing::warn!(
                                session_id = %session_id.0,
                                "Skipping tool call record: bad timestamp: {e}"
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
                        },
                    })
                }
                Err(e) => {
                    tracing::warn!("Skipping unparseable tool call row: {e}");
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

    #[test]
    fn test_save_and_load_tool_calls() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();

        let records = vec![
            new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            new_tool_call_record(1, ToolCallRole::Tool, "echo"),
            new_tool_call_record(2, ToolCallRole::Assistant, "math"),
            new_tool_call_record(3, ToolCallRole::Tool, "math"),
        ];

        store.save_tool_calls(run_id, &records).unwrap();
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

        assert_eq!(store.count_tool_calls(run_id).unwrap(), 0);

        let records = vec![
            new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            new_tool_call_record(1, ToolCallRole::Tool, "echo"),
        ];
        store.save_tool_calls(run_id, &records).unwrap();

        assert_eq!(store.count_tool_calls(run_id).unwrap(), 2);
    }

    #[test]
    fn test_tool_calls_isolated_by_run() {
        let store = SqliteStore::open_in_memory().unwrap();
        let run1 = RunId::new();
        let run2 = RunId::new();

        store
            .save_tool_call(
                run1,
                &new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
            )
            .unwrap();
        store
            .save_tool_call(
                run2,
                &new_tool_call_record(0, ToolCallRole::Assistant, "math"),
            )
            .unwrap();
        store
            .save_tool_call(run2, &new_tool_call_record(1, ToolCallRole::Tool, "math"))
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
        // Empty batch should succeed without error.
        store.save_tool_calls(run_id, &[]).unwrap();
        assert_eq!(store.load_tool_calls(run_id).unwrap().len(), 0);
    }

    #[test]
    fn test_tool_call_record_nullable_columns_roundtrip() {
        // N4: verify that assistant records with params=None and tool records
        // with result=None round-trip correctly through SQLite.
        let store = SqliteStore::open_in_memory().unwrap();
        let run_id = RunId::new();

        let records = vec![
            ToolCallRecord {
                seq: 0,
                role: ToolCallRole::Assistant,
                tool_name: Some("shell_exec".to_string()),
                tool_id: Some("call_0".to_string()),
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
                params: None,
                result: None, // Tool returned nothing
                timestamp: chrono::Utc::now(),
                from_agent: None,
            },
        ];

        store.save_tool_calls(run_id, &records).unwrap();
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
                &[
                    new_tool_call_record(0, ToolCallRole::Assistant, "echo"),
                    new_tool_call_record(1, ToolCallRole::Tool, "echo"),
                ],
            )
            .unwrap();
        store
            .save_tool_calls(
                run2.run_id,
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
                &new_tool_call_record(0, ToolCallRole::Assistant, "fs_read"),
            )
            .unwrap();
        store
            .save_tool_call(
                run_b.run_id,
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
            params: Some(r#"{"text":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: Some("alice".to_string()),
        };
        store.save_tool_call(run.run_id, &record).unwrap();

        let loaded = store.load_tool_calls(run.run_id).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].from_agent.as_deref(), Some("alice"));

        let session_calls = store.load_tool_calls_for_session(session_id).unwrap();
        assert_eq!(session_calls.len(), 1);
        assert_eq!(session_calls[0].record.from_agent.as_deref(), Some("alice"));
    }
}
