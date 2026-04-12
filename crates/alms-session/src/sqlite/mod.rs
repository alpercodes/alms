//! SQLite-backed persistence for sessions, messages, and audit events.
//!
//! Used by `SessionManager` when `ALMS_DB_PATH` is set. Write-through on every
//! mutation; full load on startup so the in-memory DashMaps stay warm.
//!
//! Domain-specific `impl SqliteStore` blocks live in submodules:
//! - `sessions` -- session CRUD, WAL flush
//! - `messages` -- message persistence and loading
//! - `audit` -- audit event storage
//! - `jobs` -- job CRUD
//! - `summaries` -- rolling context summaries
//! - `session_summaries` -- per-session episodic summaries (cross-session memory)
//! - `agents` -- agent registry (CRUD, migration, etc.)
//! - `runs` -- run persistence (save, load, mark_stale)
//! - `tool_calls` -- per-run tool call records

mod agents;
mod audit;
mod jobs;
mod messages;
mod runs;
mod session_summaries;
mod sessions;
mod summaries;
#[cfg(test)]
mod test_helpers;
mod timeline;
mod tool_calls;

pub use timeline::TimelineEvent;
pub use tool_calls::SessionToolCall;

use crate::types::{
    Content, ContextSummary, Message, Role, Session, SessionStatus, SessionSummary,
};
use alms_core::job::{Job, JobId, JobSchedule, JobStatus};
use alms_core::registry::AgentRecord;
use alms_core::run::{Run, RunStatus, TokenUsage, ToolCallRecord, ToolCallRole};
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
    metadata   TEXT,
    seq        INTEGER NOT NULL DEFAULT 0
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
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    description     TEXT NOT NULL DEFAULT '',
    model           TEXT,
    system_prompt   TEXT,
    posture         TEXT,
    provider        TEXT,
    telegram_token  TEXT,
    is_default      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    last_active     TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agents_is_default ON agents(is_default);

CREATE TABLE IF NOT EXISTS runs (
    run_id            TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL,
    agent_id          TEXT NOT NULL,
    input             TEXT NOT NULL DEFAULT '',
    response          TEXT,
    error             TEXT,
    status            TEXT NOT NULL DEFAULT 'queued',
    started_at        TEXT,
    ended_at          TEXT,
    prompt_tokens     INTEGER,
    completion_tokens INTEGER,
    job_id            TEXT,
    parent_run_id     TEXT,
    created_at        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);

CREATE TABLE IF NOT EXISTS run_tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    tool_name  TEXT,
    tool_id    TEXT,
    params     TEXT,
    result     TEXT,
    timestamp  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_tool_calls_run ON run_tool_calls(run_id, seq);

CREATE TABLE IF NOT EXISTS session_summaries (
    agent_id     TEXT NOT NULL,
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    summary      TEXT NOT NULL DEFAULT '',
    last_run_id  TEXT,
    updated_at   TEXT NOT NULL,
    source_label TEXT,
    PRIMARY KEY (agent_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_session_summaries_agent
    ON session_summaries(agent_id, updated_at DESC);
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
        // Auto-migrate: add provider column if missing (existing DBs).
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN provider TEXT;");
        // Auto-migrate: add seq column for stable message ordering (existing DBs).
        let _ = conn.execute_batch(
            "ALTER TABLE messages ADD COLUMN seq INTEGER NOT NULL DEFAULT 0;\
             UPDATE messages SET seq = rowid WHERE seq = 0;",
        );
        // Auto-migrate: add (session_id, seq) index for message ordering queries.
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq);",
        );
        // Auto-migrate: add telegram_token column for per-agent Telegram bots.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN telegram_token TEXT;");
        // Auto-migrate: add run_tool_calls table for per-run tool call storage.
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_tool_calls (\
                 id         INTEGER PRIMARY KEY AUTOINCREMENT, \
                 run_id     TEXT NOT NULL, \
                 seq        INTEGER NOT NULL, \
                 role       TEXT NOT NULL, \
                 tool_name  TEXT, \
                 tool_id    TEXT, \
                 params     TEXT, \
                 result     TEXT, \
                 timestamp  TEXT NOT NULL\
             ); \
             CREATE INDEX IF NOT EXISTS idx_run_tool_calls_run \
                 ON run_tool_calls(run_id, seq);",
        );
        // Auto-migrate: add session_summaries table for cross-session episodic memory.
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_summaries (\
                 agent_id     TEXT NOT NULL, \
                 session_id   TEXT NOT NULL REFERENCES sessions(id), \
                 summary      TEXT NOT NULL DEFAULT '', \
                 last_run_id  TEXT, \
                 updated_at   TEXT NOT NULL, \
                 source_label TEXT, \
                 PRIMARY KEY (agent_id, session_id)\
             ); \
             CREATE INDEX IF NOT EXISTS idx_session_summaries_agent \
                 ON session_summaries(agent_id, updated_at DESC);",
        );
        // Auto-migrate: add source_label column to session_summaries (existing DBs).
        let _ = conn.execute_batch("ALTER TABLE session_summaries ADD COLUMN source_label TEXT;");
        // Auto-migrate: add parent_run_id column to runs for subagent run visibility.
        let _ = conn.execute_batch("ALTER TABLE runs ADD COLUMN parent_run_id TEXT;");
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
}

// ---------------------------------------------------------------------------
// Helpers (used by domain submodules via `use super::*`)
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

fn run_status_to_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn str_to_run_status(s: &str) -> RunStatus {
    match s {
        "running" => RunStatus::Running,
        "completed" => RunStatus::Completed,
        "failed" => RunStatus::Failed,
        "cancelled" => RunStatus::Cancelled,
        _ => RunStatus::Queued,
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
    let posture: Option<String> = row.get(4)?;
    let provider: Option<String> = row.get(5)?;
    let telegram_token: Option<String> = row.get(6)?;
    let is_default: i32 = row.get(7)?;
    let created_at_str: String = row.get(8)?;
    let last_active_str: String = row.get(9)?;

    let id_uuid = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let last_active = chrono::DateTime::parse_from_rfc3339(&last_active_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(AgentRecord {
        id: AgentId(id_uuid),
        name,
        description,
        model,
        posture,
        provider,
        telegram_token,
        is_default: is_default != 0,
        created_at: created_at.with_timezone(&chrono::Utc),
        last_active: last_active.with_timezone(&chrono::Utc),
    })
}

/// Parse a run row from a SELECT query.
///
/// Expected column order:
///   0: run_id, 1: session_id, 2: agent_id, 3: input, 4: response,
///   5: error, 6: status, 7: started_at, 8: ended_at,
///   9: prompt_tokens, 10: completion_tokens, 11: job_id, 12: created_at
fn parse_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    let run_id_str: String = row.get(0)?;
    let session_id_str: String = row.get(1)?;
    let agent_id_str: String = row.get(2)?;
    let input: String = row.get(3)?;
    let response: Option<String> = row.get(4)?;
    let error: Option<String> = row.get(5)?;
    let status_str: String = row.get(6)?;
    let started_at_str: Option<String> = row.get(7)?;
    let ended_at_str: Option<String> = row.get(8)?;
    let prompt_tokens: Option<i64> = row.get(9)?;
    let completion_tokens: Option<i64> = row.get(10)?;
    let job_id_str: Option<String> = row.get(11)?;
    let parent_run_id_str: Option<String> = row.get(12)?;
    let created_at_str: String = row.get(13)?;

    let run_id_uuid = uuid::Uuid::parse_str(&run_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let session_id_uuid = uuid::Uuid::parse_str(&session_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let agent_id_uuid = uuid::Uuid::parse_str(&agent_id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(e))
        })?
        .with_timezone(&chrono::Utc);
    let started_at = started_at_str
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .inspect_err(|e| {
                    tracing::warn!(run_id = %run_id_str, "Corrupt started_at in DB: {e}");
                })
                .ok()
        })
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let ended_at = ended_at_str
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .inspect_err(|e| {
                    tracing::warn!(run_id = %run_id_str, "Corrupt ended_at in DB: {e}");
                })
                .ok()
        })
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let usage = match (prompt_tokens, completion_tokens) {
        (Some(pt), Some(ct)) => Some(TokenUsage {
            prompt_tokens: u32::try_from(pt).unwrap_or(u32::MAX),
            completion_tokens: u32::try_from(ct).unwrap_or(u32::MAX),
        }),
        _ => None,
    };
    let job_id = job_id_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .map(alms_core::job::JobId);
    let parent_run_id = parent_run_id_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .map(RunId);

    Ok(Run {
        run_id: RunId(run_id_uuid),
        session_id: SessionId(session_id_uuid),
        agent_id: AgentId(agent_id_uuid),
        status: str_to_run_status(&status_str),
        input,
        output: response,
        error,
        usage,
        created_at,
        started_at,
        ended_at,
        job_id,
        parent_run_id,
    })
}
