// SPDX-License-Identifier: Apache-2.0

//! Message persistence and loading.

use super::*;

impl SqliteStore {
    // ── Messages ─────────────────────────────────────────────────────────────

    /// Persist a message for a session.
    ///
    /// On insert, `seq` is computed as `MAX(seq) + 1` for the session via a
    /// subquery so the allocation only happens when the row is actually new.
    ///
    /// On conflict (same message `id` already exists), only `content` and
    /// `metadata` are updated -- `role`, `timestamp`, and `seq` are preserved
    /// from the original insert.
    pub fn save_message(&self, session_id: SessionId, msg: &Message) -> AlmsResult<()> {
        let conn = self.conn.lock();
        Self::save_message_on(&conn, session_id, msg)
    }

    pub(super) fn save_message_on(
        conn: &Connection,
        session_id: SessionId,
        msg: &Message,
    ) -> AlmsResult<()> {
        let content_json = serde_json::to_string(&msg.content)?;
        let metadata_json = msg
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let sid = session_id.0.to_string();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, timestamp, metadata, seq) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?2)) \
             ON CONFLICT(id) DO UPDATE SET content=excluded.content, metadata=excluded.metadata",
            params![
                &msg.id,
                &sid,
                role_to_str(msg.role),
                content_json,
                msg.timestamp.0.to_rfc3339(),
                metadata_json,
            ],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite save_message: {e}")))?;
        Ok(())
    }

    /// Load all messages for a session in logical order.
    pub fn load_messages(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, role, content, timestamp, metadata \
                 FROM messages WHERE session_id = ?1 ORDER BY seq",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare messages: {e}")))?;

        // `skipped` is incremented in two sequential phases below:
        // 1. During `query_map` — rows that fail SQLite column extraction.
        // 2. During the second `filter_map` — rows with valid columns but
        //    unparseable content JSON or timestamps.
        // Both phases run sequentially, so no concurrency concern; the split
        // across two closures is just due to the two-pass processing.
        // It is a local tally for the per-session summary line below; the
        // process-lifetime accounting is `record_skipped_row` (#1241).
        let mut skipped: usize = 0;
        let raw_rows: Vec<_> = stmt
            .query_map([session_id.0.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query messages: {e}")))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    self.record_skipped_row(
                        PersistenceTable::Messages,
                        format_args!("session {}: {e}", session_id.0),
                    );
                    skipped += 1;
                    None
                }
            })
            .collect();

        let rows: Vec<Message> = raw_rows
            .into_iter()
            .filter_map(|(id, role_str, content_json, ts_str, metadata_str)| {
                let content: Content = match serde_json::from_str(&content_json) {
                    Ok(c) => c,
                    Err(e) => {
                        self.record_skipped_row(
                            PersistenceTable::Messages,
                            format_args!("message {id}: bad content JSON: {e}"),
                        );
                        skipped += 1;
                        return None;
                    }
                };
                let ts = match chrono::DateTime::parse_from_rfc3339(&ts_str) {
                    Ok(t) => t,
                    Err(e) => {
                        self.record_skipped_row(
                            PersistenceTable::Messages,
                            format_args!("message {id}: bad timestamp: {e}"),
                        );
                        skipped += 1;
                        return None;
                    }
                };
                let metadata = metadata_str.and_then(|s| match serde_json::from_str(&s) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::debug!("Message {id}: ignoring bad metadata JSON: {e}");
                        None
                    }
                });
                Some(Message {
                    id,
                    role: str_to_role(&role_str),
                    content,
                    timestamp: Timestamp(ts.with_timezone(&chrono::Utc)),
                    metadata,
                })
            })
            .collect();

        if skipped > 0 {
            tracing::warn!(
                "Session {}: loaded {} messages, skipped {} unparseable rows",
                session_id.0,
                rows.len(),
                skipped,
            );
        }

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{corrupt_with_sql, new_message, new_session};
    use super::super::*;

    #[test]
    fn test_message_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let msg = new_message("Hello, world!");
        store.save_message(session.id, &msg).unwrap();

        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0].content, Content::Text(t) if t == "Hello, world!"));
        assert!(matches!(messages[0].role, Role::User));
    }

    #[test]
    fn test_multiple_messages_ordered() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        for i in 0..3 {
            store
                .save_message(session.id, &new_message(&format!("msg {i}")))
                .unwrap();
        }

        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[0].content, Content::Text(t) if t == "msg 0"));
        assert!(matches!(&messages[2].content, Content::Text(t) if t == "msg 2"));
    }

    #[test]
    fn test_reinsert_message_preserves_ordering() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        // Insert three messages; note the id of the first one.
        let mut msg_a = new_message("first");
        let msg_b = new_message("second");
        let msg_c = new_message("third");
        let id_a = msg_a.id.clone();

        store.save_message(session.id, &msg_a).unwrap();
        store.save_message(session.id, &msg_b).unwrap();
        store.save_message(session.id, &msg_c).unwrap();

        // Re-insert msg_a with updated content (same id).
        msg_a.content = Content::Text("first-updated".to_string());
        store.save_message(session.id, &msg_a).unwrap();

        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(messages.len(), 3, "re-insert should not duplicate");

        // msg_a must still be first (seq preserved on conflict).
        assert_eq!(messages[0].id, id_a);
        assert!(
            matches!(&messages[0].content, Content::Text(t) if t == "first-updated"),
            "content should be updated"
        );
        // Ordering must be: first-updated, second, third.
        assert!(matches!(&messages[1].content, Content::Text(t) if t == "second"));
        assert!(matches!(&messages[2].content, Content::Text(t) if t == "third"));
    }

    /// #1241: the second-phase skip (valid columns, unparseable content JSON)
    /// is counted under `messages` just like the column-extraction skip.
    #[test]
    fn corrupt_message_row_is_dropped_and_counted() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();
        store
            .save_message(session.id, &new_message("keep"))
            .unwrap();
        store
            .save_message(session.id, &new_message("lose"))
            .unwrap();

        corrupt_with_sql(
            &store,
            "UPDATE messages SET content = '{not valid json' WHERE content LIKE '%lose%'",
        );

        let loaded = store.load_messages(session.id).unwrap();
        assert_eq!(loaded.len(), 1, "the readable message must still load");
        assert!(matches!(&loaded[0].content, Content::Text(t) if t == "keep"));
        assert_eq!(store.rows_skipped_for(PersistenceTable::Messages), 1);
        assert_eq!(store.rows_skipped_total(), 1);
    }
}
