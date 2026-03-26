//! Session CRUD and WAL flush.

use super::*;

impl SqliteStore {
    // ── Sessions ─────────────────────────────────────────────────────────────

    /// Upsert a session row.
    pub fn save_session(&self, session: &Session) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO sessions \
             (id, agent_id, context_id, created_at, last_activity, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session.id.0.to_string(),
                    session.agent_id.0.to_string(),
                    &session.context_id,
                    session.created_at.0.to_rfc3339(),
                    session.last_activity.0.to_rfc3339(),
                    status_to_str(session.status),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_session: {e}")))?;
        Ok(())
    }

    /// Delete a session and all its related data (messages, audit, summaries,
    /// runs, run tool calls).
    ///
    /// Wrapped in a transaction so a crash mid-delete cannot leave orphaned rows.
    pub fn delete_session(&self, session_id: SessionId) -> AlmsResult<()> {
        let mut conn = self.conn.lock();
        let id_str = session_id.0.to_string();
        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin delete_session: {e}")))?;
        // Delete dependent rows first (foreign key order)
        tx.execute(
            "DELETE FROM session_summaries WHERE session_id = ?1",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete session_summaries: {e}")))?;
        tx.execute(
            "DELETE FROM context_summaries WHERE session_id = ?1",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete summaries: {e}")))?;
        tx.execute(
            "DELETE FROM audit_events WHERE session_id = ?1",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete audit: {e}")))?;
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete messages: {e}")))?;
        // Delete tool call records for runs belonging to this session.
        tx.execute(
            "DELETE FROM run_tool_calls WHERE run_id IN \
             (SELECT run_id FROM runs WHERE session_id = ?1)",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete run tool calls: {e}")))?;
        // Delete runs belonging to this session.
        tx.execute("DELETE FROM runs WHERE session_id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete runs: {e}")))?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete session: {e}")))?;
        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit delete_session: {e}")))?;
        Ok(())
    }

    /// Flush the WAL to the main database file.
    ///
    /// Called during graceful shutdown to ensure all buffered writes are
    /// durable before the process exits.
    pub fn flush_wal(&self) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| AlmsError::Runtime(format!("SQLite WAL flush: {e}")))?;
        Ok(())
    }

    /// Load every session row, oldest first.
    pub fn load_all_sessions(&self) -> AlmsResult<Vec<Session>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, context_id, created_at, last_activity, status \
                 FROM sessions ORDER BY rowid",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare sessions: {e}")))?;

        let rows = stmt
            .query_map([], parse_session_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query sessions: {e}")))?
            .filter_map(|r| match r {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("Skipping unparseable session row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Load a single session by its UUID.
    pub fn load_session_by_id(&self, id: SessionId) -> AlmsResult<Option<Session>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, agent_id, context_id, created_at, last_activity, status \
             FROM sessions WHERE id = ?1",
            params![id.0.to_string()],
            parse_session_row,
        );
        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!(
                "SQLite load_session_by_id: {e}"
            ))),
        }
    }

    /// Load sessions for a specific agent, most recent first.
    pub fn load_sessions_by_agent(&self, agent_id: AgentId) -> AlmsResult<Vec<Session>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, context_id, created_at, last_activity, status \
                 FROM sessions WHERE agent_id = ?1 ORDER BY last_activity DESC",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare sessions_by_agent: {e}")))?;

        let rows = stmt
            .query_map([agent_id.0.to_string()], parse_session_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query sessions_by_agent: {e}")))?
            .filter_map(|r| match r {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("Skipping unparseable session row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// List all sessions, ordered by last activity (newest first).
    pub fn list_sessions(&self) -> AlmsResult<Vec<Session>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, context_id, created_at, last_activity, status \
                 FROM sessions ORDER BY last_activity DESC",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare list_sessions: {e}")))?;

        let rows = stmt
            .query_map([], parse_session_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query list_sessions: {e}")))?
            .filter_map(|r| match r {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("Skipping unparseable session row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Migrate Telegram session context IDs from the old format (`telegram_{chat_id}`)
    /// to the new per-agent format (`telegram_{agent_name}_{chat_id}`).
    ///
    /// This is a one-time, idempotent migration for backward compatibility after
    /// the per-agent Telegram feature was introduced. Only sessions whose
    /// `context_id` matches `telegram_` followed by a purely numeric chat ID
    /// (possibly negative) are migrated. Sessions already using the new
    /// `telegram_{agent_name}_{chat_id}` format are unaffected.
    ///
    /// Returns the number of sessions migrated.
    pub fn migrate_telegram_context_ids(&self, agent_name: &str) -> AlmsResult<usize> {
        let conn = self.conn.lock();

        // Collect old-format sessions: context_id = "telegram_" + digits/dash only.
        // We load them first, then update, because SQLite GLOB doesn't support
        // the "only digits after prefix" assertion we need. Instead we filter
        // in Rust for exact matching.
        let mut stmt = conn
            .prepare(
                "SELECT id, context_id FROM sessions \
                 WHERE context_id LIKE 'telegram_%'",
            )
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite prepare migrate_telegram_context_ids: {e}"))
            })?;

        let old_sessions: Vec<(String, String)> = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let ctx: String = row.get(1)?;
                Ok((id, ctx))
            })
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite query migrate_telegram_context_ids: {e}"))
            })?
            .filter_map(|r| r.ok())
            .filter(|(_id, ctx)| {
                // Old format: "telegram_" + (optional "-") + digits
                let suffix = &ctx["telegram_".len()..];
                let numeric_part = suffix.strip_prefix('-').unwrap_or(suffix);
                !numeric_part.is_empty() && numeric_part.chars().all(|c| c.is_ascii_digit())
            })
            .collect();

        let mut migrated = 0usize;
        for (session_id, old_ctx) in &old_sessions {
            let chat_id_part = &old_ctx["telegram_".len()..];
            let new_ctx = format!("telegram_{agent_name}_{chat_id_part}");
            let affected = conn
                .execute(
                    "UPDATE sessions SET context_id = ?1 WHERE id = ?2",
                    params![&new_ctx, session_id],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite update migrate_telegram_context_ids: {e}"))
                })?;
            migrated += affected;
        }
        Ok(migrated)
    }

    /// Count messages in a session without loading them.
    pub fn message_count(&self, session_id: SessionId) -> AlmsResult<usize> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id.0.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite message_count: {e}")))?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{new_message, new_session};
    use super::super::*;

    #[test]
    fn test_session_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let sessions = store.load_all_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id);
        assert_eq!(sessions[0].context_id, "test-ctx");
        assert!(matches!(sessions[0].status, SessionStatus::Active));
    }

    #[test]
    fn test_session_upsert_updates_status() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut session = new_session();
        store.save_session(&session).unwrap();

        session.status = SessionStatus::Idle;
        store.save_session(&session).unwrap();

        let sessions = store.load_all_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(sessions[0].status, SessionStatus::Idle));
    }

    #[test]
    fn test_messages_isolated_by_session() {
        let store = SqliteStore::open_in_memory().unwrap();
        let s1 = new_session();
        let s2 = Session::new(AgentId::new(), "ctx2");
        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();

        store.save_message(s1.id, &new_message("for s1")).unwrap();
        store.save_message(s2.id, &new_message("for s2")).unwrap();

        assert_eq!(store.load_messages(s1.id).unwrap().len(), 1);
        assert_eq!(store.load_messages(s2.id).unwrap().len(), 1);
    }

    #[test]
    fn test_flush_wal() {
        // In-memory DB uses journal_mode=memory, not WAL, but the
        // pragma still succeeds -- verifies the method doesn't error.
        let store = SqliteStore::open_in_memory().unwrap();
        store.save_session(&new_session()).unwrap();
        store.flush_wal().unwrap();
    }

    #[test]
    fn test_load_session_by_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let loaded = store.load_session_by_id(session.id).unwrap().unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.context_id, "test-ctx");
    }

    #[test]
    fn test_load_session_by_id_not_found() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert!(
            store
                .load_session_by_id(SessionId::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_load_sessions_by_agent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent1 = AgentId::new();
        let agent2 = AgentId::new();

        let s1 = Session::new(agent1, "ctx-a");
        let s2 = Session::new(agent1, "ctx-b");
        let s3 = Session::new(agent2, "ctx-c");
        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();
        store.save_session(&s3).unwrap();

        let agent1_sessions = store.load_sessions_by_agent(agent1).unwrap();
        assert_eq!(agent1_sessions.len(), 2);

        let agent2_sessions = store.load_sessions_by_agent(agent2).unwrap();
        assert_eq!(agent2_sessions.len(), 1);
        assert_eq!(agent2_sessions[0].context_id, "ctx-c");
    }

    #[test]
    fn test_message_count() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        assert_eq!(store.message_count(session.id).unwrap(), 0);

        store.save_message(session.id, &new_message("one")).unwrap();
        store.save_message(session.id, &new_message("two")).unwrap();
        assert_eq!(store.message_count(session.id).unwrap(), 2);
    }

    #[test]
    fn test_migrate_telegram_context_ids() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        // Old-format sessions
        let s1 = Session::new(agent_id, "telegram_123456");
        let s2 = Session::new(agent_id, "telegram_-789");
        // New-format session (should NOT be migrated)
        let s3 = Session::new(agent_id, "telegram_mybot_999");
        // Non-Telegram session (should NOT be migrated)
        let s4 = Session::new(agent_id, "web_session_1");
        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();
        store.save_session(&s3).unwrap();
        store.save_session(&s4).unwrap();

        let migrated = store.migrate_telegram_context_ids("main").unwrap();
        assert_eq!(migrated, 2);

        // Verify old-format sessions were renamed
        let loaded_s1 = store.load_session_by_id(s1.id).unwrap().unwrap();
        assert_eq!(loaded_s1.context_id, "telegram_main_123456");

        let loaded_s2 = store.load_session_by_id(s2.id).unwrap().unwrap();
        assert_eq!(loaded_s2.context_id, "telegram_main_-789");

        // New-format session unchanged
        let loaded_s3 = store.load_session_by_id(s3.id).unwrap().unwrap();
        assert_eq!(loaded_s3.context_id, "telegram_mybot_999");

        // Non-telegram session unchanged
        let loaded_s4 = store.load_session_by_id(s4.id).unwrap().unwrap();
        assert_eq!(loaded_s4.context_id, "web_session_1");
    }

    #[test]
    fn test_migrate_telegram_context_ids_idempotent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        let s1 = Session::new(agent_id, "telegram_123456");
        store.save_session(&s1).unwrap();

        let first = store.migrate_telegram_context_ids("main").unwrap();
        assert_eq!(first, 1);

        // Second call should migrate zero (already in new format)
        let second = store.migrate_telegram_context_ids("main").unwrap();
        assert_eq!(second, 0);
    }

    #[test]
    fn test_delete_session_cascades_session_summaries() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        // Create a session summary for this session.
        store
            .upsert_session_summary(session.agent_id, session.id, "summary to be cascaded", None)
            .unwrap();
        assert!(
            store
                .load_session_summary(session.agent_id, session.id)
                .unwrap()
                .is_some()
        );

        // A second session + summary that should NOT be deleted.
        let other = Session::new(AgentId::new(), "ctx-other");
        store.save_session(&other).unwrap();
        store
            .upsert_session_summary(other.agent_id, other.id, "other summary", None)
            .unwrap();

        store.delete_session(session.id).unwrap();

        // The deleted session's summary is gone.
        assert!(
            store
                .load_session_summary(session.agent_id, session.id)
                .unwrap()
                .is_none()
        );

        // The other session's summary is untouched.
        assert!(
            store
                .load_session_summary(other.agent_id, other.id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_delete_session_cascades_tool_calls_and_runs() {
        use alms_core::run::{Run, ToolCallRecord, ToolCallRole};

        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        // Create a run and attach tool calls.
        let run = Run::new(session.id, session.agent_id, "hello".to_string());
        let run_id = run.run_id;
        store.save_run(&run).unwrap();
        store
            .save_tool_calls(
                run_id,
                &[
                    ToolCallRecord {
                        seq: 0,
                        role: ToolCallRole::Assistant,
                        tool_name: Some("echo".to_string()),
                        tool_id: Some("call_0".to_string()),
                        params: Some(r#"{"text":"hello"}"#.to_string()),
                        result: None,
                        timestamp: chrono::Utc::now(),
                    },
                    ToolCallRecord {
                        seq: 1,
                        role: ToolCallRole::Tool,
                        tool_name: Some("echo".to_string()),
                        tool_id: Some("call_0".to_string()),
                        params: None,
                        result: Some(r#""result_ok""#.to_string()),
                        timestamp: chrono::Utc::now(),
                    },
                ],
            )
            .unwrap();

        // A control run on a different session that should NOT be deleted.
        let other_session = Session::new(AgentId::new(), "ctx-other");
        store.save_session(&other_session).unwrap();
        let other_run = Run::new(
            other_session.id,
            other_session.agent_id,
            "hello".to_string(),
        );
        let other_run_id = other_run.run_id;
        store.save_run(&other_run).unwrap();
        store
            .save_tool_call(
                other_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("math".to_string()),
                    tool_id: Some("call_0".to_string()),
                    params: Some(r#"{"text":"hello"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                },
            )
            .unwrap();

        // Pre-condition: tool calls and runs exist.
        assert_eq!(store.count_tool_calls(run_id).unwrap(), 2);
        assert_eq!(store.count_tool_calls(other_run_id).unwrap(), 1);

        // Delete the first session.
        store.delete_session(session.id).unwrap();

        // Tool calls and runs for the deleted session are gone.
        assert_eq!(store.count_tool_calls(run_id).unwrap(), 0);
        assert_eq!(store.load_tool_calls(run_id).unwrap().len(), 0);
        assert!(store.load_run(run_id).unwrap().is_none());

        // Control session's data is untouched.
        assert_eq!(store.count_tool_calls(other_run_id).unwrap(), 1);
        assert!(store.load_run(other_run_id).unwrap().is_some());
    }
}
