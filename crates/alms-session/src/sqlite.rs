//! SQLite-backed persistence for sessions, messages, and audit events.
//!
//! Used by `SessionManager` when `ALMS_DB_PATH` is set. Write-through on every
//! mutation; full load on startup so the in-memory DashMaps stay warm.

use crate::types::{Content, ContextSummary, Message, Role, Session, SessionStatus};
use alms_core::job::{Job, JobId, JobSchedule, JobStatus};
use alms_core::registry::AgentRecord;
use alms_core::{
    AgentId, AlmsError, AlmsResult, AuditDecision, AuditEvent, RunId, SessionId, Timestamp,
};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

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

CREATE TABLE IF NOT EXISTS jobs (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT NOT NULL,
    prompt      TEXT NOT NULL,
    schedule    TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL,
    next_run_at TEXT,
    last_run_at TEXT
);

CREATE TABLE IF NOT EXISTS context_summaries (
    session_id       TEXT PRIMARY KEY REFERENCES sessions(id),
    text             TEXT NOT NULL DEFAULT '',
    messages_covered INTEGER NOT NULL DEFAULT 0,
    updated_at       TEXT
);

CREATE TABLE IF NOT EXISTS agents (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    description   TEXT NOT NULL DEFAULT '',
    model         TEXT,
    system_prompt TEXT,
    posture       TEXT,
    is_default    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    last_active   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agents_is_default ON agents(is_default);
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
        Self {
            conn: Arc::clone(&self.conn),
        }
    }
}

impl SqliteStore {
    /// Open or create a SQLite database at `path`.
    pub fn open<P: AsRef<Path>>(path: P) -> AlmsResult<Self> {
        let conn =
            Connection::open(path).map_err(|e| AlmsError::Runtime(format!("SQLite open: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AlmsError::Runtime(format!("SQLite schema init: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> AlmsResult<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| AlmsError::Runtime(format!("SQLite open_in_memory: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AlmsError::Runtime(format!("SQLite schema init: {e}")))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

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

    /// Delete a session and all its related data (messages, audit, summaries).
    ///
    /// Wrapped in a transaction so a crash mid-delete cannot leave orphaned rows.
    pub fn delete_session(&self, session_id: SessionId) -> AlmsResult<()> {
        let conn = self.conn.lock();
        let id_str = session_id.0.to_string();
        conn.execute_batch("BEGIN")
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin delete_session: {e}")))?;
        let result = (|| -> AlmsResult<()> {
            // Delete dependent rows first (foreign key order)
            conn.execute(
                "DELETE FROM context_summaries WHERE session_id = ?1",
                params![&id_str],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete summaries: {e}")))?;
            conn.execute(
                "DELETE FROM audit_events WHERE session_id = ?1",
                params![&id_str],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete audit: {e}")))?;
            conn.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![&id_str],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete messages: {e}")))?;
            conn.execute("DELETE FROM sessions WHERE id = ?1", params![&id_str])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete session: {e}")))?;
            Ok(())
        })();
        match &result {
            Ok(()) => {
                conn.execute_batch("COMMIT").map_err(|e| {
                    AlmsError::Runtime(format!("SQLite commit delete_session: {e}"))
                })?;
            }
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
        result
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

    // ── Messages ─────────────────────────────────────────────────────────────

    /// Upsert a single message row.
    pub fn save_message(&self, session_id: SessionId, msg: &Message) -> AlmsResult<()> {
        let content_json = serde_json::to_string(&msg.content)?;
        let metadata_json = msg
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.conn
            .lock()
            .execute(
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
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("Skipping unparseable message row: {e}");
                    None
                }
            })
            .filter_map(|(id, role_str, content_json, ts_str, metadata_str)| {
                let content: Content = match serde_json::from_str(&content_json) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Skipping message {id}: bad content JSON: {e}");
                        return None;
                    }
                };
                let ts = match chrono::DateTime::parse_from_rfc3339(&ts_str) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("Skipping message {id}: bad timestamp: {e}");
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

        Ok(rows)
    }

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

    // ── Jobs ──────────────────────────────────────────────────────────────────

    /// Upsert a job row (handles both insert and update via OR REPLACE).
    pub fn save_job(&self, job: &Job) -> AlmsResult<()> {
        let schedule_json = serde_json::to_string(&job.schedule)
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job serialize: {e}")))?;
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO jobs \
                 (id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    job.id.0.to_string(),
                    job.agent_id.0.to_string(),
                    &job.prompt,
                    schedule_json,
                    job_status_to_str(job.status),
                    job.created_at.to_rfc3339(),
                    job.next_run_at.map(|t| t.to_rfc3339()),
                    job.last_run_at.map(|t| t.to_rfc3339()),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite save_job: {e}")))?;
        Ok(())
    }

    /// Load all non-cancelled jobs, oldest first.
    pub fn load_all_jobs(&self) -> AlmsResult<Vec<Job>> {
        self.query_jobs(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
             FROM jobs WHERE status != 'cancelled' ORDER BY rowid",
        )
    }

    /// Load a single job by ID.
    pub fn load_job_by_id(&self, id: JobId) -> AlmsResult<Option<Job>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
             FROM jobs WHERE id = ?1",
            params![id.0.to_string()],
            parse_job_row,
        );
        match result {
            Ok(job) => Ok(Some(job)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_job_by_id: {e}"))),
        }
    }

    /// Load all jobs including cancelled, ordered by created_at DESC.
    pub fn load_all_jobs_unfiltered(&self) -> AlmsResult<Vec<Job>> {
        self.query_jobs(
            "SELECT id, agent_id, prompt, schedule, status, created_at, next_run_at, last_run_at \
             FROM jobs ORDER BY created_at DESC",
        )
    }

    /// Shared helper for job list queries.
    fn query_jobs(&self, sql: &str) -> AlmsResult<Vec<Job>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare jobs: {e}")))?;

        let rows = stmt
            .query_map([], parse_job_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query jobs: {e}")))?
            .filter_map(|r| match r {
                Ok(j) => Some(j),
                Err(e) => {
                    tracing::warn!("Skipping unparseable job row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }

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

    // ── Agents ────────────────────────────────────────────────────────────────

    /// Insert a new agent record. Fails if the name or id already exists.
    pub fn create_agent(&self, agent: &AgentRecord) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO agents \
                 (id, name, description, model, system_prompt, posture, is_default, created_at, last_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    agent.id.0.to_string(),
                    &agent.name,
                    &agent.description,
                    agent.model.as_deref(),
                    agent.system_prompt.as_deref(),
                    agent.posture.as_deref(),
                    agent.is_default as i32,
                    agent.created_at.to_rfc3339(),
                    agent.last_active.to_rfc3339(),
                ],
            )
            .map_err(|e| match &e {
                rusqlite::Error::SqliteFailure(err, _)
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    AlmsError::DuplicateName(agent.name.clone())
                }
                _ => AlmsError::Runtime(format!("SQLite create_agent: {e}")),
            })?;
        Ok(())
    }

    /// Update an existing agent's mutable config fields (matched by id).
    ///
    /// Does NOT update `name` or `is_default` — use `set_default_agent()` for
    /// default changes, and name is immutable after creation.
    pub fn update_agent(&self, agent: &AgentRecord) -> AlmsResult<()> {
        let affected = self
            .conn
            .lock()
            .execute(
                "UPDATE agents SET description = ?1, model = ?2, system_prompt = ?3, \
                 posture = ?4, last_active = ?5 WHERE id = ?6",
                params![
                    &agent.description,
                    agent.model.as_deref(),
                    agent.system_prompt.as_deref(),
                    agent.posture.as_deref(),
                    agent.last_active.to_rfc3339(),
                    agent.id.0.to_string(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite update_agent: {e}")))?;
        if affected == 0 {
            return Err(AlmsError::AgentNotFound(agent.id.0.to_string()));
        }
        Ok(())
    }

    /// Load an agent by its UUID.
    pub fn load_agent_by_id(&self, id: AgentId) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, system_prompt, posture, is_default, created_at, last_active \
             FROM agents WHERE id = ?1",
            params![id.0.to_string()],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_agent_by_id: {e}"))),
        }
    }

    /// Load an agent by its unique name slug.
    pub fn load_agent_by_name(&self, name: &str) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, system_prompt, posture, is_default, created_at, last_active \
             FROM agents WHERE name = ?1",
            params![name],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!(
                "SQLite load_agent_by_name: {e}"
            ))),
        }
    }

    /// Load the default agent, if one exists.
    pub fn get_default_agent(&self) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, system_prompt, posture, is_default, created_at, last_active \
             FROM agents WHERE is_default = 1 LIMIT 1",
            [],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite get_default_agent: {e}"))),
        }
    }

    /// List all agents, ordered by creation time.
    pub fn list_agents(&self) -> AlmsResult<Vec<AgentRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, model, system_prompt, posture, is_default, created_at, last_active \
                 FROM agents ORDER BY created_at",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare agents: {e}")))?;

        let rows = stmt
            .query_map([], parse_agent_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query agents: {e}")))?
            .filter_map(|r| match r {
                Ok(agent) => Some(agent),
                Err(e) => {
                    tracing::warn!("Skipping unparseable agent row: {}", e);
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Delete an agent and all its dependent data (sessions, messages, audit
    /// events, context summaries, jobs).
    ///
    /// Wrapped in a transaction so a crash mid-delete cannot leave orphaned
    /// rows. Returns `true` if the agent existed and was deleted.
    pub fn delete_agent(&self, id: AgentId) -> AlmsResult<bool> {
        let conn = self.conn.lock();
        let id_str = id.0.to_string();

        conn.execute_batch("BEGIN")
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin delete_agent: {e}")))?;

        let result = (|| -> AlmsResult<bool> {
            // 1. Collect session IDs belonging to this agent
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE agent_id = ?1")
                .map_err(|e| AlmsError::Runtime(format!("SQLite prepare session query: {e}")))?;
            let session_ids: Vec<String> = stmt
                .query_map(params![&id_str], |row| row.get(0))
                .map_err(|e| AlmsError::Runtime(format!("SQLite query agent sessions: {e}")))?
                .filter_map(|r| r.ok())
                .collect();

            // 2. Delete dependent rows for each session (FK order)
            for sid in &session_ids {
                conn.execute(
                    "DELETE FROM context_summaries WHERE session_id = ?1",
                    params![sid],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete summaries for session: {e}"))
                })?;
                conn.execute(
                    "DELETE FROM audit_events WHERE session_id = ?1",
                    params![sid],
                )
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete audit for session: {e}")))?;
                conn.execute("DELETE FROM messages WHERE session_id = ?1", params![sid])
                    .map_err(|e| {
                        AlmsError::Runtime(format!("SQLite delete messages for session: {e}"))
                    })?;
            }

            // 3. Delete the sessions themselves
            conn.execute("DELETE FROM sessions WHERE agent_id = ?1", params![&id_str])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete agent sessions: {e}")))?;

            // 4. Delete jobs belonging to this agent
            conn.execute("DELETE FROM jobs WHERE agent_id = ?1", params![&id_str])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete agent jobs: {e}")))?;

            // 5. Delete the agent row
            let affected = conn
                .execute("DELETE FROM agents WHERE id = ?1", params![&id_str])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete_agent: {e}")))?;

            Ok(affected > 0)
        })();

        match &result {
            Ok(_) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| AlmsError::Runtime(format!("SQLite commit delete_agent: {e}")))?;
            }
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
        result
    }

    /// Set an agent as the default, clearing any previous default.
    ///
    /// Wrapped in a transaction so a crash between the two UPDATEs cannot
    /// leave the system with zero default agents.
    ///
    /// Returns `AgentNotFound` if the given ID does not exist in the table.
    pub fn set_default_agent(&self, id: AgentId) -> AlmsResult<()> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin: {e}")))?;
        tx.execute("UPDATE agents SET is_default = 0 WHERE is_default = 1", [])
            .map_err(|e| AlmsError::Runtime(format!("SQLite clear_default: {e}")))?;
        let affected = tx
            .execute(
                "UPDATE agents SET is_default = 1 WHERE id = ?1",
                params![id.0.to_string()],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite set_default: {e}")))?;
        if affected == 0 {
            return Err(AlmsError::AgentNotFound(id.0.to_string()));
        }
        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit: {e}")))?;
        Ok(())
    }

    /// Update an agent's `last_active` timestamp.
    pub fn touch_agent(&self, id: AgentId) -> AlmsResult<()> {
        let rows = self
            .conn
            .lock()
            .execute(
                "UPDATE agents SET last_active = ?1 WHERE id = ?2",
                params![chrono::Utc::now().to_rfc3339(), id.0.to_string()],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite touch_agent: {e}")))?;
        if rows == 0 {
            tracing::debug!(agent_id = %id, "touch_agent: no agent found with this ID");
        }
        Ok(())
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

fn job_status_to_str(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Pending => "pending",
        JobStatus::Active => "active",
        JobStatus::Cancelled => "cancelled",
    }
}

fn str_to_job_status(s: &str) -> JobStatus {
    match s {
        "active" => JobStatus::Active,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Pending,
    }
}

/// Parse an agent row from a SELECT query.
fn parse_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let id_str: String = row.get(0)?;
    let agent_id_str: String = row.get(1)?;
    let prompt: String = row.get(2)?;
    let schedule_json: String = row.get(3)?;
    let status_str: String = row.get(4)?;
    let created_at_str: String = row.get(5)?;
    let next_run_at_str: Option<String> = row.get(6)?;
    let last_run_at_str: Option<String> = row.get(7)?;

    let id_uuid = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let agent_uuid = uuid::Uuid::parse_str(&agent_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let schedule: JobSchedule = serde_json::from_str(&schedule_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let next_run_at = next_run_at_str
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let last_run_at = last_run_at_str
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    Ok(Job {
        id: JobId(id_uuid),
        agent_id: AgentId(agent_uuid),
        prompt,
        schedule,
        status: str_to_job_status(&status_str),
        created_at: created_at.with_timezone(&chrono::Utc),
        next_run_at,
        last_run_at,
    })
}

fn parse_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let id_str: String = row.get(0)?;
    let agent_id_str: String = row.get(1)?;
    let context_id: String = row.get(2)?;
    let created_at_str: String = row.get(3)?;
    let last_activity_str: String = row.get(4)?;
    let status_str: String = row.get(5)?;

    let id_uuid = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let agent_uuid = uuid::Uuid::parse_str(&agent_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let last_activity = chrono::DateTime::parse_from_rfc3339(&last_activity_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(Session {
        id: SessionId(id_uuid),
        agent_id: AgentId(agent_uuid),
        context_id,
        created_at: Timestamp(created_at.with_timezone(&chrono::Utc)),
        last_activity: Timestamp(last_activity.with_timezone(&chrono::Utc)),
        status: str_to_status(&status_str),
    })
}

fn parse_agent_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    let id_str: String = row.get(0)?;
    let name: String = row.get(1)?;
    let description: String = row.get(2)?;
    let model: Option<String> = row.get(3)?;
    let system_prompt: Option<String> = row.get(4)?;
    let posture: Option<String> = row.get(5)?;
    let is_default: i32 = row.get(6)?;
    let created_at_str: String = row.get(7)?;
    let last_active_str: String = row.get(8)?;

    let id_uuid = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let last_active = chrono::DateTime::parse_from_rfc3339(&last_active_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(AgentRecord {
        id: AgentId(id_uuid),
        name,
        description,
        model,
        system_prompt,
        posture,
        is_default: is_default != 0,
        created_at: created_at.with_timezone(&chrono::Utc),
        last_active: last_active.with_timezone(&chrono::Utc),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Content, Message, Role, Session};
    use alms_core::{AgentId, RunId};

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

    #[test]
    fn test_flush_wal() {
        // In-memory DB uses journal_mode=memory, not WAL, but the
        // pragma still succeeds — verifies the method doesn't error.
        let store = SqliteStore::open_in_memory().unwrap();
        store.save_session(&new_session()).unwrap();
        store.flush_wal().unwrap();
    }

    // ── Agent registry tests ──────────────────────────────────────────────

    fn new_agent(name: &str) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(),
            name: name.to_string(),
            description: String::new(),
            model: None,
            system_prompt: None,
            posture: None,
            is_default: false,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_agent_create_and_load_by_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.id, agent.id);
        assert_eq!(loaded.name, "atlas");
        assert!(!loaded.is_default);
        assert!(loaded.model.is_none());
    }

    #[test]
    fn test_agent_load_by_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("researcher");
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_name("researcher").unwrap().unwrap();
        assert_eq!(loaded.id, agent.id);

        // Non-existent name returns None
        assert!(store.load_agent_by_name("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_agent_list_ordered() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut a1 = new_agent("alpha");
        a1.created_at = chrono::Utc::now() - chrono::Duration::seconds(10);
        store.create_agent(&a1).unwrap();

        let a2 = new_agent("beta");
        store.create_agent(&a2).unwrap();

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "alpha");
        assert_eq!(agents[1].name, "beta");
    }

    #[test]
    fn test_agent_delete() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("doomed");
        store.create_agent(&agent).unwrap();

        assert!(store.delete_agent(agent.id).unwrap());
        assert!(store.load_agent_by_id(agent.id).unwrap().is_none());

        // Deleting again returns false
        assert!(!store.delete_agent(agent.id).unwrap());
    }

    #[test]
    fn test_agent_delete_cascades_sessions_messages_audit_jobs() {
        let store = SqliteStore::open_in_memory().unwrap();

        // Create two agents — one to delete, one to keep as control
        let doomed = new_agent("doomed");
        let survivor = new_agent("survivor");
        store.create_agent(&doomed).unwrap();
        store.create_agent(&survivor).unwrap();

        // Create sessions for both agents
        let ds = Session::new(doomed.id, "ctx-doomed");
        let ss = Session::new(survivor.id, "ctx-survivor");
        store.save_session(&ds).unwrap();
        store.save_session(&ss).unwrap();

        // Add messages to both sessions
        store
            .save_message(ds.id, &new_message("doomed msg"))
            .unwrap();
        store
            .save_message(ss.id, &new_message("survivor msg"))
            .unwrap();

        // Add audit events to both sessions
        let doomed_audit = AuditEvent::allow(
            ds.id,
            "echo",
            serde_json::json!({"text": "hi"}),
            serde_json::json!("hi"),
        );
        let survivor_audit = AuditEvent::allow(
            ss.id,
            "echo",
            serde_json::json!({"text": "ok"}),
            serde_json::json!("ok"),
        );
        store.save_audit(&doomed_audit).unwrap();
        store.save_audit(&survivor_audit).unwrap();

        // Add context summaries to both sessions
        let summary = ContextSummary {
            text: "test summary".to_string(),
            messages_covered: 1,
            updated_at: Some(Timestamp::now()),
        };
        store.save_summary(ds.id, &summary).unwrap();
        store.save_summary(ss.id, &summary).unwrap();

        // Add jobs for both agents
        let doomed_job = Job {
            id: JobId::new(),
            agent_id: doomed.id,
            prompt: "doomed job".to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        let survivor_job = Job {
            id: JobId::new(),
            agent_id: survivor.id,
            prompt: "survivor job".to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        store.save_job(&doomed_job).unwrap();
        store.save_job(&survivor_job).unwrap();

        // Delete the doomed agent — should cascade
        assert!(store.delete_agent(doomed.id).unwrap());

        // Doomed agent's data is gone
        assert!(store.load_agent_by_id(doomed.id).unwrap().is_none());
        assert!(store.load_sessions_by_agent(doomed.id).unwrap().is_empty());
        assert!(store.load_messages(ds.id).unwrap().is_empty());
        assert!(store.load_audit(ds.id).unwrap().is_empty());

        // Survivor agent's data is untouched
        assert!(store.load_agent_by_id(survivor.id).unwrap().is_some());
        let survivor_sessions = store.load_sessions_by_agent(survivor.id).unwrap();
        assert_eq!(survivor_sessions.len(), 1);
        assert_eq!(store.load_messages(ss.id).unwrap().len(), 1);
        assert_eq!(store.load_audit(ss.id).unwrap().len(), 1);

        // Survivor's job still exists, doomed's job is gone
        let all_jobs = store.load_all_jobs_unfiltered().unwrap();
        assert_eq!(all_jobs.len(), 1);
        assert_eq!(all_jobs[0].agent_id, survivor.id);
    }

    #[test]
    fn test_agent_set_default_clears_previous() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut a1 = new_agent("first");
        a1.is_default = true;
        store.create_agent(&a1).unwrap();

        let a2 = new_agent("second");
        store.create_agent(&a2).unwrap();

        // Set second as default
        store.set_default_agent(a2.id).unwrap();

        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.id, a2.id);

        // First should no longer be default
        let first = store.load_agent_by_id(a1.id).unwrap().unwrap();
        assert!(!first.is_default);
    }

    #[test]
    fn test_agent_unique_name_constraint() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a1 = new_agent("unique");
        store.create_agent(&a1).unwrap();

        // Different ID, same name — should fail (UNIQUE constraint)
        let mut a2 = new_agent("unique");
        a2.id = AgentId::new(); // different UUID
        // INSERT OR REPLACE keys on PRIMARY KEY (id), not name.
        // A different id with the same name should violate UNIQUE.
        let result = store.create_agent(&a2);
        assert!(
            matches!(result, Err(alms_core::AlmsError::DuplicateName(ref name)) if name == "unique"),
            "Expected DuplicateName error, got: {:?}",
            result,
        );
    }

    #[test]
    fn test_agent_touch_updates_last_active() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("touchme");
        agent.last_active = chrono::Utc::now() - chrono::Duration::seconds(100);
        store.create_agent(&agent).unwrap();

        let before = store.load_agent_by_id(agent.id).unwrap().unwrap();
        store.touch_agent(agent.id).unwrap();
        let after = store.load_agent_by_id(agent.id).unwrap().unwrap();

        assert!(after.last_active > before.last_active);
    }

    #[test]
    fn test_agent_touch_nonexistent_succeeds() {
        let store = SqliteStore::open_in_memory().unwrap();
        let fake_id = AgentId(uuid::Uuid::new_v4());
        // Should succeed (not error) even for a nonexistent agent.
        store.touch_agent(fake_id).unwrap();
    }

    #[test]
    fn test_agent_with_overrides() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("custom");
        agent.model = Some("anthropic/claude-sonnet-4-20250514".to_string());
        agent.posture = Some("guarded".to_string());
        agent.description = "A custom agent".to_string();
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(
            loaded.model.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
        assert_eq!(loaded.posture.as_deref(), Some("guarded"));
        assert_eq!(loaded.description, "A custom agent");
        assert!(loaded.system_prompt.is_none());
    }

    #[test]
    fn test_agent_get_default_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert!(store.get_default_agent().unwrap().is_none());
    }

    #[test]
    fn test_agent_update_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("mutable");
        store.create_agent(&agent).unwrap();

        agent.description = "Updated description".to_string();
        agent.model = Some("new-model".to_string());
        agent.posture = Some("guarded".to_string());
        store.update_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.description, "Updated description");
        assert_eq!(loaded.model.as_deref(), Some("new-model"));
        assert_eq!(loaded.posture.as_deref(), Some("guarded"));
    }

    #[test]
    fn test_agent_set_default_nonexistent_errors() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("exists");
        agent.is_default = true;
        store.create_agent(&agent).unwrap();

        // Setting a nonexistent agent as default should error
        let fake_id = AgentId::new();
        let result = store.set_default_agent(fake_id);
        assert!(result.is_err());

        // The existing agent should still be default (rollback undid the clear)
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(loaded.is_default);
    }

    // ── Session query tests ─────────────────────────────────────────────

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
}
