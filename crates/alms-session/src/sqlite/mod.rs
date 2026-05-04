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

pub use timeline::{TimelineEvent, TimelinePage};
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
    id                     TEXT PRIMARY KEY,
    name                   TEXT NOT NULL UNIQUE,
    description            TEXT NOT NULL DEFAULT '',
    model                  TEXT,
    system_prompt          TEXT,
    posture                TEXT,
    provider               TEXT,
    telegram_token         TEXT,
    is_default             INTEGER NOT NULL DEFAULT 0,
    created_at             TEXT NOT NULL,
    last_active            TEXT NOT NULL,
    thinking_budget_tokens INTEGER,
    reasoning_effort       TEXT,
    gemini_thinking_budget INTEGER,
    summary_provider       TEXT,
    summary_model          TEXT,
    worktree_mode          TEXT
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
    created_at        TEXT NOT NULL,
    resolved_config   TEXT
);

CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_agent_id ON runs(agent_id);

CREATE TABLE IF NOT EXISTS run_tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    tool_name  TEXT,
    tool_id    TEXT,
    params     TEXT,
    result     TEXT,
    timestamp  TEXT NOT NULL,
    from_agent TEXT
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
        // Auto-migrate: add thinking_budget_tokens column for per-agent
        // Anthropic extended-thinking opt-in (issue #767). NULL means
        // "inherit the server default from [llm.anthropic]"; any integer
        // (including 0) is an explicit per-agent override.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN thinking_budget_tokens INTEGER;");
        // Auto-migrate: add reasoning_effort column for per-agent
        // OpenAI-compat reasoning-model opt-in (issue #768). NULL means
        // "inherit the server default from [llm.openai]"; a string
        // ("low"/"medium"/"high"/"minimal") is an explicit per-agent
        // override. Stored as TEXT to match the `ReasoningEffort` enum's
        // lowercase serde wire format.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN reasoning_effort TEXT;");
        // Auto-migrate: add gemini_thinking_budget column for per-agent
        // Gemini extended-thinking opt-in (issue #794). NULL means
        // "inherit the server default from [llm.gemini]"; any integer
        // (including 0) is an explicit per-agent override.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN gemini_thinking_budget INTEGER;");
        // Auto-migrate: add summary_provider / summary_model columns for
        // per-agent summary-task overrides (issue #872). NULL on either
        // means "fall through to the server-level [context] settings";
        // both must be set together (PATCH validator enforces the pair
        // invariant). Additive, reversible: existing rows stay NULL on
        // both columns and behave identically to today's server-level
        // path.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN summary_provider TEXT;");
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN summary_model TEXT;");
        // Auto-migrate: add worktree_mode column for per-agent git worktree
        // isolation (issue #946). NULL on existing rows is treated as
        // `WorktreeMode::Off` by `parse_agent_row` so the column is
        // backward-compatible — agents created before #946 keep their
        // project-root sandbox without any operator action.
        let _ = conn.execute_batch("ALTER TABLE agents ADD COLUMN worktree_mode TEXT;");
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
                 timestamp  TEXT NOT NULL, \
                 from_agent TEXT\
             ); \
             CREATE INDEX IF NOT EXISTS idx_run_tool_calls_run \
                 ON run_tool_calls(run_id, seq);",
        );
        // Auto-migrate: add from_agent column to run_tool_calls for existing
        // DBs so the frontend fallback merge path can attribute DM reasoning
        // blocks to the correct agent (see #696).
        let _ = conn.execute_batch("ALTER TABLE run_tool_calls ADD COLUMN from_agent TEXT;");
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
        // Auto-migrate: add index on runs(agent_id) for timeline queries.
        let _ =
            conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_runs_agent_id ON runs(agent_id);");
        // Auto-migrate: add resolved_config column for the layered run-config
        // snapshot (#837). Stored as a JSON-encoded TEXT blob — the column
        // is NULL for runs created before the snapshot was wired up so the
        // backward-compat surface is "old rows hydrate with `resolved_config:
        // None`" without explicit handling.
        let _ = conn.execute_batch("ALTER TABLE runs ADD COLUMN resolved_config TEXT;");
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
    // Per-agent Anthropic thinking budget (issue #767). Stored as INTEGER;
    // parsed as i64 → saturating u32 so corrupt values don't crash the row
    // parser. NULL maps to `None` (inherit the server default).
    let thinking_budget_tokens: Option<u32> = row
        .get::<_, Option<i64>>(10)?
        .map(|v| v.clamp(0, i64::from(u32::MAX)) as u32);
    // Per-agent OpenAI-compat reasoning effort (issue #768). Stored as
    // TEXT; unrecognised values are logged and mapped to `None` (inherit
    // the server default) rather than failing row parsing — follows the
    // same defence-in-depth stance as the thinking budget above.
    let reasoning_effort: Option<alms_core::config::ReasoningEffort> = row
        .get::<_, Option<String>>(11)?
        .and_then(|s| match s.parse() {
            Ok(effort) => Some(effort),
            Err(e) => {
                tracing::warn!(
                    stored = %s,
                    error = %e,
                    "Skipping unparseable reasoning_effort in agents row"
                );
                None
            }
        });
    // Per-agent Gemini thinking budget (issue #794). Stored as INTEGER;
    // parsed as i64 → saturating u32 so corrupt values don't crash the
    // row parser. NULL maps to `None` (inherit the server default).
    let gemini_thinking_budget: Option<u32> = row
        .get::<_, Option<i64>>(12)?
        .map(|v| v.clamp(0, i64::from(u32::MAX)) as u32);
    // Per-agent summary-task provider / model (issue #872). Both stored
    // as TEXT; NULL on either means "fall through to the server-level
    // [context] settings". The PATCH validator and CRUD handlers
    // enforce the pair invariant — at the parser level we just deserialize
    // whatever is on disk and let the resolver flag invalid combinations
    // when they're observed at run time.
    let summary_provider: Option<String> = row.get(13)?;
    let summary_model: Option<String> = row.get(14)?;
    // Per-agent worktree-isolation mode (issue #946). Stored as TEXT;
    // NULL maps to `WorktreeMode::Off` for back-compat with rows
    // written before the column existed. Unrecognised values are
    // logged and mapped to `Off` so a corrupt row never crashes
    // parsing — matches the defence-in-depth stance on
    // `reasoning_effort` above.
    let worktree_mode: alms_core::WorktreeMode = match row.get::<_, Option<String>>(15)? {
        None => alms_core::WorktreeMode::Off,
        Some(s) => match s.parse() {
            Ok(mode) => mode,
            Err(e) => {
                tracing::warn!(
                    stored = %s,
                    error = %e,
                    "Skipping unparseable worktree_mode in agents row — defaulting to Off"
                );
                alms_core::WorktreeMode::Off
            }
        },
    };

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
        thinking_budget_tokens,
        reasoning_effort,
        gemini_thinking_budget,
        summary_provider,
        summary_model,
        worktree_mode,
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
///   9: prompt_tokens, 10: completion_tokens, 11: job_id,
///   12: parent_run_id, 13: created_at, 14: resolved_config (#837)
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
    // #837: optional JSON-encoded snapshot of the layered run config.
    // Older rows (or runs that never advanced past Queued) carry NULL —
    // surfaced to callers as `resolved_config: None`. Corrupt JSON
    // is tolerated: we log a warning and fall back to None rather
    // than failing the whole row, matching the existing `started_at` /
    // `ended_at` permissive-parse pattern.
    //
    // Why the permissive `.ok().flatten()` form: the realistic failure
    // mode this guards is a NULL-valued cell on a row that predates #837
    // (or was inserted via a code path that didn't set `resolved_config`).
    // `row.get::<_, Option<String>>(14)?` would also handle the NULL
    // cleanly — `?` and `.ok().flatten()` are technically equivalent for
    // the NULL case. The permissive form is kept as defense-in-depth: if
    // a future schema change drops or renames the column (or the
    // best-effort `ALTER TABLE ADD COLUMN` migration above ever stops
    // running on some path), the strict `?` would propagate
    // `InvalidColumnIndex` / `InvalidColumnName` and poison every row
    // hydration; `.ok().flatten()` collapses both "missing column" and
    // "NULL cell" into `resolved_config: None` so triage degrades
    // gracefully — the run still hydrates with no snapshot. Do NOT
    // tighten this to `row.get(14)?` without auditing those failure
    // paths.
    let resolved_config_str: Option<String> = row.get(14).ok().flatten();

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
            // Reasoning tokens are not persisted on the `runs` table today.
            // The live flow is: provider adapter -> `TokenUsage.reasoning_tokens`
            // -> `RunOutput.usage` -> SSE `run_finished` event + `GET /runs/{id}`
            // response (both carry the field when the provider reports it).
            // Persisting a `reasoning_tokens` column to the `runs` table is a
            // follow-up that requires a schema migration.
            reasoning_tokens: None,
            // Cache tokens (#766) are similarly not persisted on the
            // `runs` table yet — they flow through the live SSE path and
            // subagent completion markers, but a DB-level surface would
            // need a schema migration.
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }),
        _ => None,
    };
    let job_id = job_id_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .map(alms_core::job::JobId);
    let parent_run_id = parent_run_id_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
        .map(RunId);
    // #837: parse the JSON-encoded layered run-config snapshot. NULL
    // (old rows) and corrupt JSON both surface as `None` — the latter
    // is logged so triage can spot a snapshot persisted under a now-
    // incompatible serde shape.
    let resolved_config = resolved_config_str.and_then(|s| match serde_json::from_str(&s) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!(
                run_id = %run_id_str,
                "Corrupt resolved_config in DB, hydrating as None: {e}"
            );
            None
        }
    });

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
        resolved_config,
    })
}
