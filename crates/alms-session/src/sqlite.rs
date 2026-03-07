//! SQLite-backed persistence for sessions, messages, and audit events.
//!
//! Used by `SessionManager` when `ALMS_DB_PATH` is set. Write-through on every
//! mutation; full load on startup so the in-memory DashMaps stay warm.

use crate::types::{Content, Message, Role, Session, SessionStatus};
use alms_core::{AgentId, AlmsError, AlmsResult, AuditDecision, AuditEvent, RunId, SessionId, Timestamp};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    context_id    TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    status        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    timestamp  TEXT NOT NULL,
    metadata   TEXT
);

CREATE TABLE IF NOT EXISTS audit_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    run_id     TEXT,
    tool       TEXT NOT NULL,
    decision   TEXT NOT NULL,
    params     TEXT NOT NULL,
    result     TEXT,
    error      TEXT,
    ts         TEXT NOT NULL
);
";

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// SQLite-backed store for sessions, messages, and audit events.
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore").finish_non_exhaustive()
    }
}

impl Clone for SqliteStore {
    fn clone(&self) -> Self {
        Self { conn: Arc::clone(&self.conn) }
    }
}

impl SqliteStore {
    /// Open or create a SQLite database at `path`.
    pub fn open<P: AsRef<Path>>(path: P) -> AlmsResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| AlmsError::Runtime(format!("SQLite open: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AlmsError::Runtime(format!("SQLite schema init: {e}")))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> AlmsResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| AlmsError::Runtime(format!("SQLite open_in_memory: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AlmsError::Runtime(format!("SQLite schema init: {e}")))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ── Sessions ─────────────────────────────────────────────────────────────

    /// Upsert a session row.
    pub fn save_session(&self, session: &Session) -> AlmsResult<()> {
        self.conn.lock().execute(
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
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query sessions: {e}")))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, agent_id, context_id, created_at, last_activity, status)| {
                let id_uuid = uuid::Uuid::parse_str(&id).ok()?;
                let agent_uuid = uuid::Uuid::parse_str(&agent_id).ok()?;
                let created = chrono::DateTime::parse_from_rfc3339(&created_at).ok()?;
                let last = chrono::DateTime::parse_from_rfc3339(&last_activity).ok()?;
                Some(Session {
                    id: SessionId(id_uuid),
                    agent_id: AgentId(agent_uuid),
                    context_id,
                    created_at: Timestamp(created.with_timezone(&chrono::Utc)),
                    last_activity: Timestamp(last.with_timezone(&chrono::Utc)),
                    status: str_to_status(&status),
                })
            })
            .collect();

        Ok(rows)
    }

    // ── Messages ─────────────────────────────────────────────────────────────

    /// Upsert a single message row.
    pub fn save_message(&self, session_id: SessionId, msg: &Message) -> AlmsResult<()> {
        let content_json = serde_json::to_string(&msg.content)?;
        let metadata_json = msg.metadata.as_ref().map(serde_json::to_string).transpose()?;
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO messages \
             (id, session_id, role, content, timestamp, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &msg.id,
                session_id.0.to_string(),
                role_to_str(msg.role),
                content_json,
                msg.timestamp.0.to_rfc3339(),
                metadata_json,
            ],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite save_message: {e}")))?;
        Ok(())
    }

    /// Load all messages for a session in insertion order.
    pub fn load_messages(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, role, content, timestamp, metadata \
                 FROM messages WHERE session_id = ?1 ORDER BY rowid",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare messages: {e}")))?;

        let rows = stmt
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
            .filter_map(|r| r.ok())
            .filter_map(|(id, role_str, content_json, ts_str, metadata_str)| {
                let content: Content = serde_json::from_str(&content_json).ok()?;
                let ts = chrono::DateTime::parse_from_rfc3339(&ts_str).ok()?;
                let metadata = metadata_str.and_then(|s| serde_json::from_str(&s).ok());
                Some(Message {
                    id,
                    role: str_to_role(&role_str),
                    content,
                    timestamp: Timestamp(ts.with_timezone(&chrono::Utc)),
                    metadata,
                })
            })
            .collect();

        Ok(rows)
    }

    // ── Audit ─────────────────────────────────────────────────────────────────

    /// Append an audit event row.
    pub fn save_audit(&self, event: &AuditEvent) -> AlmsResult<()> {
        let params_json = serde_json::to_string(&event.params)?;
        let result_json = event.result.as_ref().map(serde_json::to_string).transpose()?;
        let decision = match event.decision {
            AuditDecision::Allow => "allow",
            AuditDecision::Deny => "deny",
        };
        self.conn.lock().execute(
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
            .filter_map(|r| r.ok())
            .filter_map(
                |(sid, run_id_str, tool, decision_str, params_str, result_str, error_str, ts_str)| {
                    let session_uuid = uuid::Uuid::parse_str(&sid).ok()?;
                    let run_id = run_id_str
                        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
                        .map(RunId);
                    let params = serde_json::from_str(&params_str).ok()?;
                    let result = result_str.and_then(|s| serde_json::from_str(&s).ok());
                    let ts = chrono::DateTime::parse_from_rfc3339(&ts_str).ok()?;
                    let decision = if decision_str == "allow" {
                        AuditDecision::Allow
                    } else {
                        AuditDecision::Deny
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn status_to_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Active => "active",
        SessionStatus::Idle => "idle",
        SessionStatus::Archived => "archived",
    }
}

fn str_to_status(s: &str) -> SessionStatus {
    match s {
        "idle" => SessionStatus::Idle,
        "archived" => SessionStatus::Archived,
        _ => SessionStatus::Active,
    }
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, RunId};
    use crate::types::{Content, Message, Role, Session};

    fn new_session() -> Session {
        Session::new(AgentId::new(), "test-ctx")
    }

    fn new_message(text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: None,
        }
    }

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
            store.save_message(session.id, &new_message(&format!("msg {i}"))).unwrap();
        }

        let messages = store.load_messages(session.id).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[0].content, Content::Text(t) if t == "msg 0"));
        assert!(matches!(&messages[2].content, Content::Text(t) if t == "msg 2"));
    }

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
}
