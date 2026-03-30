//! Audit event storage.

use super::*;

impl SqliteStore {
    // ── Audit ─────────────────────────────────────────────────────────────────

    /// Append an audit event row.
    pub fn save_audit(&self, event: &AuditEvent) -> AlmsResult<()> {
        let params_json = serde_json::to_string(&event.params)?;
        let result_json = event
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let decision = match event.decision {
            AuditDecision::Allow => "allow",
            AuditDecision::Deny => "deny",
            AuditDecision::Error => "error",
        };
        self.conn
            .lock()
            .execute(
                "INSERT INTO audit_events \
             (session_id, run_id, tool, decision, params, result, error, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.session_id.0.to_string(),
                    event.run_id.map(|r| r.0.to_string()),
                    &event.tool,
                    decision,
                    params_json,
                    result_json,
                    event.error.as_deref(),
                    event.timestamp.0.to_rfc3339(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_audit: {e}")))?;
        Ok(())
    }

    /// Load all audit events for a session in chronological order.
    pub fn load_audit(&self, session_id: SessionId) -> AlmsResult<Vec<AuditEvent>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT session_id, run_id, tool, decision, params, result, error, ts \
                 FROM audit_events WHERE session_id = ?1 ORDER BY id",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare audit: {e}")))?;

        let rows = stmt
            .query_map([session_id.0.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query audit: {e}")))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("Skipping unparseable audit row: {e}");
                    None
                }
            })
            .filter_map(
                |(
                    sid,
                    run_id_str,
                    tool,
                    decision_str,
                    params_str,
                    result_str,
                    error_str,
                    ts_str,
                )| {
                    let session_uuid = match uuid::Uuid::parse_str(&sid) {
                        Ok(u) => u,
                        Err(e) => {
                            tracing::warn!("Skipping audit row: bad session UUID {sid}: {e}");
                            return None;
                        }
                    };
                    let run_id = run_id_str
                        .and_then(|s| match uuid::Uuid::parse_str(&s) {
                            Ok(u) => Some(u),
                            Err(e) => {
                                tracing::debug!("Audit row {sid}: ignoring bad run_id UUID: {e}");
                                None
                            }
                        })
                        .map(RunId);
                    let params = match serde_json::from_str(&params_str) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!("Skipping audit row {sid}: bad params JSON: {e}");
                            return None;
                        }
                    };
                    let result = result_str.and_then(|s| match serde_json::from_str(&s) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::debug!("Audit row {sid}: ignoring bad result JSON: {e}");
                            None
                        }
                    });
                    let ts = match chrono::DateTime::parse_from_rfc3339(&ts_str) {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!("Skipping audit row {sid}: bad timestamp: {e}");
                            return None;
                        }
                    };
                    let decision = match decision_str.as_str() {
                        "allow" => AuditDecision::Allow,
                        "error" => AuditDecision::Error,
                        _ => AuditDecision::Deny,
                    };
                    Some(AuditEvent {
                        session_id: SessionId(session_uuid),
                        run_id,
                        tool,
                        decision,
                        params,
                        result,
                        error: error_str,
                        timestamp: Timestamp(ts.with_timezone(&chrono::Utc)),
                    })
                },
            )
            .collect();

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::new_session;
    use super::super::*;

    #[test]
    fn test_audit_allow_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let event = AuditEvent::allow(
            session.id,
            "echo",
            serde_json::json!({"text": "hi"}),
            serde_json::json!("hi"),
        );
        store.save_audit(&event).unwrap();

        let audit = store.load_audit(session.id).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].tool, "echo");
        assert!(matches!(audit[0].decision, AuditDecision::Allow));
        assert!(audit[0].run_id.is_none());
    }

    #[test]
    fn test_audit_with_run_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let run_id = RunId::new();
        let mut event = AuditEvent::deny(session.id, "bash", serde_json::json!({}), "denied");
        event.run_id = Some(run_id);
        store.save_audit(&event).unwrap();

        let audit = store.load_audit(session.id).unwrap();
        assert_eq!(audit[0].run_id, Some(run_id));
        assert!(matches!(audit[0].decision, AuditDecision::Deny));
        assert_eq!(audit[0].error.as_deref(), Some("denied"));
    }

    #[test]
    fn test_audit_error_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let session = new_session();
        store.save_session(&session).unwrap();

        let event = AuditEvent {
            session_id: session.id,
            run_id: None,
            tool: "shell_exec".to_string(),
            decision: AuditDecision::Error,
            params: serde_json::json!({"command": "ls"}),
            result: None,
            error: Some("process timed out".to_string()),
            timestamp: alms_core::Timestamp::now(),
        };
        store.save_audit(&event).unwrap();

        let audit = store.load_audit(session.id).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].tool, "shell_exec");
        assert!(matches!(audit[0].decision, AuditDecision::Error));
        assert_eq!(audit[0].error.as_deref(), Some("process timed out"));
    }
}
