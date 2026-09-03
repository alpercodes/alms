// SPDX-License-Identifier: Apache-2.0

//! Rolling context summary persistence.

use super::*;

impl SqliteStore {
    // ── Context summaries ─────────────────────────────────────────────────────

    /// Upsert the rolling context summary for a session.
    pub fn save_summary(&self, session_id: SessionId, summary: &ContextSummary) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO context_summaries \
                 (session_id, text, messages_covered, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    session_id.0.to_string(),
                    &summary.text,
                    summary.messages_covered as i64,
                    summary.updated_at.as_ref().map(|t| t.0.to_rfc3339()),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_summary: {e}")))?;
        Ok(())
    }

    /// Load the rolling context summary for a session, if one exists.
    pub fn load_summary(&self, session_id: SessionId) -> AlmsResult<Option<ContextSummary>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT text, messages_covered, updated_at \
             FROM context_summaries WHERE session_id = ?1",
            params![session_id.0.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        );

        match result {
            Ok((text, messages_covered, updated_at_str)) => {
                let updated_at = updated_at_str
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| Timestamp(dt.with_timezone(&chrono::Utc)));
                Ok(Some(ContextSummary {
                    text,
                    messages_covered: messages_covered.max(0) as usize,
                    updated_at,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_summary: {e}"))),
        }
    }
}
