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
                 (run_id, seq, role, tool_name, tool_id, params, result, timestamp) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    run_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
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
                 (run_id, seq, role, tool_name, tool_id, params, result, timestamp) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    run_id.0.to_string(),
                    record.seq as i64,
                    record.role.to_string(),
                    record.tool_name.as_deref(),
                    record.tool_id.as_deref(),
                    record.params.as_deref(),
                    record.result.as_deref(),
                    record.timestamp.to_rfc3339(),
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
                "SELECT seq, role, tool_name, tool_id, params, result, timestamp \
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
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_tool_calls: {e}")))?
            .filter_map(|r| match r {
                Ok((seq, role_str, tool_name, tool_id, params, result, ts_str)) => {
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
            },
            ToolCallRecord {
                seq: 1,
                role: ToolCallRole::Tool,
                tool_name: Some("shell_exec".to_string()),
                tool_id: Some("call_0".to_string()),
                params: None,
                result: None, // Tool returned nothing
                timestamp: chrono::Utc::now(),
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
}
