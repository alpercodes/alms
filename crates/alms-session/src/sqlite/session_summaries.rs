//! Per-session episodic summary persistence (cross-session memory).

use super::*;

impl SqliteStore {
    // -- Session summaries (episodic memory) ---------------------------------

    /// Insert or update the episodic summary for a `(agent_id, session_id)` pair.
    ///
    /// Each session has at most one summary row.  Successive runs in the same
    /// session call this to extend or replace the summary text.
    pub fn upsert_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
        summary_text: &str,
        run_id: Option<RunId>,
    ) -> AlmsResult<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .lock()
            .execute(
                "INSERT INTO session_summaries \
                     (agent_id, session_id, summary, last_run_id, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(agent_id, session_id) DO UPDATE SET \
                     summary     = excluded.summary, \
                     last_run_id = excluded.last_run_id, \
                     updated_at  = excluded.updated_at",
                params![
                    agent_id.0.to_string(),
                    session_id.0.to_string(),
                    summary_text,
                    run_id.map(|r| r.0.to_string()),
                    &now,
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite upsert_session_summary: {e}")))?;
        Ok(())
    }

    /// Load all episodic summaries for an agent, ordered by `updated_at DESC`
    /// (most recently active session first), up to `limit`.
    pub fn load_session_summaries(
        &self,
        agent_id: AgentId,
        limit: usize,
    ) -> AlmsResult<Vec<SessionSummary>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, session_id, summary, last_run_id, updated_at \
                 FROM session_summaries \
                 WHERE agent_id = ?1 \
                 ORDER BY updated_at DESC \
                 LIMIT ?2",
            )
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite prepare load_session_summaries: {e}"))
            })?;

        let rows = stmt
            .query_map(
                params![agent_id.0.to_string(), limit as i64],
                parse_session_summary_row,
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite query load_session_summaries: {e}")))?
            .filter_map(|r| match r {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("Skipping unparseable session_summary row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Load a single episodic summary by `(agent_id, session_id)`.
    pub fn load_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
    ) -> AlmsResult<Option<SessionSummary>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT agent_id, session_id, summary, last_run_id, updated_at \
             FROM session_summaries \
             WHERE agent_id = ?1 AND session_id = ?2",
            params![agent_id.0.to_string(), session_id.0.to_string()],
            parse_session_summary_row,
        );
        match result {
            Ok(summary) => Ok(Some(summary)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!(
                "SQLite load_session_summary: {e}"
            ))),
        }
    }

    /// Delete the episodic summary for a `(agent_id, session_id)` pair.
    pub fn delete_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
    ) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "DELETE FROM session_summaries WHERE agent_id = ?1 AND session_id = ?2",
                params![agent_id.0.to_string(), session_id.0.to_string()],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete_session_summary: {e}")))?;
        Ok(())
    }
}

/// Parse a `session_summaries` row into a [`SessionSummary`].
///
/// Expected column order:
///   0: agent_id, 1: session_id, 2: summary, 3: last_run_id, 4: updated_at
fn parse_session_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    let agent_id_str: String = row.get(0)?;
    let session_id_str: String = row.get(1)?;
    let summary: String = row.get(2)?;
    let last_run_id_str: Option<String> = row.get(3)?;
    let updated_at_str: String = row.get(4)?;

    let agent_uuid = uuid::Uuid::parse_str(&agent_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let session_uuid = uuid::Uuid::parse_str(&session_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let last_run_id = last_run_id_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .map(RunId);
    let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?
        .with_timezone(&chrono::Utc);

    Ok(SessionSummary {
        agent_id: AgentId(agent_uuid),
        session_id: SessionId(session_uuid),
        summary,
        last_run_id,
        updated_at: Timestamp(updated_at),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::new_session;
    use super::super::*;

    #[test]
    fn test_upsert_and_load_single() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let run_id = RunId::new();
        store
            .upsert_session_summary(
                session.agent_id,
                session.id,
                "Helped debug CORS issue.",
                Some(run_id),
            )
            .unwrap();

        let loaded = store
            .load_session_summary(session.agent_id, session.id)
            .unwrap()
            .expect("summary should exist");
        assert_eq!(loaded.agent_id, session.agent_id);
        assert_eq!(loaded.session_id, session.id);
        assert_eq!(loaded.summary, "Helped debug CORS issue.");
        assert_eq!(loaded.last_run_id, Some(run_id));
    }

    #[test]
    fn test_upsert_overwrites_existing() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let run1 = RunId::new();
        store
            .upsert_session_summary(session.agent_id, session.id, "First summary.", Some(run1))
            .unwrap();

        let run2 = RunId::new();
        store
            .upsert_session_summary(
                session.agent_id,
                session.id,
                "Extended summary with more detail.",
                Some(run2),
            )
            .unwrap();

        let loaded = store
            .load_session_summary(session.agent_id, session.id)
            .unwrap()
            .expect("summary should exist");
        assert_eq!(loaded.summary, "Extended summary with more detail.");
        assert_eq!(loaded.last_run_id, Some(run2));
    }

    #[test]
    fn test_load_session_summaries_ordered_by_recency() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        // Create three sessions with summaries inserted in chronological order.
        let s1 = Session::new(agent_id, "ctx-1");
        let s2 = Session::new(agent_id, "ctx-2");
        let s3 = Session::new(agent_id, "ctx-3");
        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();
        store.save_session(&s3).unwrap();

        // Insert in order; each subsequent call gets a later timestamp.
        store
            .upsert_session_summary(agent_id, s1.id, "oldest", None)
            .unwrap();
        // Small sleep so timestamps differ (SQLite TEXT comparison)
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .upsert_session_summary(agent_id, s2.id, "middle", None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .upsert_session_summary(agent_id, s3.id, "newest", None)
            .unwrap();

        let all = store.load_session_summaries(agent_id, 10).unwrap();
        assert_eq!(all.len(), 3);
        // Newest first
        assert_eq!(all[0].summary, "newest");
        assert_eq!(all[1].summary, "middle");
        assert_eq!(all[2].summary, "oldest");
    }

    #[test]
    fn test_load_session_summaries_respects_limit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        for i in 0..5 {
            let s = Session::new(agent_id, format!("ctx-{i}"));
            store.save_session(&s).unwrap();
            store
                .upsert_session_summary(agent_id, s.id, &format!("summary {i}"), None)
                .unwrap();
        }

        let limited = store.load_session_summaries(agent_id, 2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_load_session_summaries_filters_by_agent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        let sa = Session::new(agent_a, "ctx-a");
        let sb = Session::new(agent_b, "ctx-b");
        store.save_session(&sa).unwrap();
        store.save_session(&sb).unwrap();

        store
            .upsert_session_summary(agent_a, sa.id, "agent A summary", None)
            .unwrap();
        store
            .upsert_session_summary(agent_b, sb.id, "agent B summary", None)
            .unwrap();

        let a_summaries = store.load_session_summaries(agent_a, 10).unwrap();
        assert_eq!(a_summaries.len(), 1);
        assert_eq!(a_summaries[0].summary, "agent A summary");

        let b_summaries = store.load_session_summaries(agent_b, 10).unwrap();
        assert_eq!(b_summaries.len(), 1);
        assert_eq!(b_summaries[0].summary, "agent B summary");
    }

    #[test]
    fn test_load_session_summary_not_found() {
        let store = SqliteStore::open_in_memory().unwrap();
        let result = store
            .load_session_summary(AgentId::new(), SessionId::new())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_delete_session_summary() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        store
            .upsert_session_summary(session.agent_id, session.id, "to be deleted", None)
            .unwrap();
        assert!(
            store
                .load_session_summary(session.agent_id, session.id)
                .unwrap()
                .is_some()
        );

        store
            .delete_session_summary(session.agent_id, session.id)
            .unwrap();
        assert!(
            store
                .load_session_summary(session.agent_id, session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_session_summary_idempotent() {
        let store = SqliteStore::open_in_memory().unwrap();
        // Deleting a non-existent summary should not error.
        store
            .delete_session_summary(AgentId::new(), SessionId::new())
            .unwrap();
    }

    #[test]
    fn test_upsert_without_run_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        store
            .upsert_session_summary(session.agent_id, session.id, "heuristic summary", None)
            .unwrap();

        let loaded = store
            .load_session_summary(session.agent_id, session.id)
            .unwrap()
            .expect("summary should exist");
        assert!(loaded.last_run_id.is_none());
        assert_eq!(loaded.summary, "heuristic summary");
    }
}
