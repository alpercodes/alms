// SPDX-License-Identifier: Apache-2.0

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
mod field_degradation;
mod jobs;
mod messages;
mod migrations;
mod row_skips;
mod runs;
mod session_summaries;
mod sessions;
mod summaries;
#[cfg(test)]
mod test_helpers;
mod timeline;
mod tool_calls;

pub use field_degradation::DegradedField;
pub use migrations::CURRENT_SCHEMA_VERSION;
pub use row_skips::PersistenceTable;
pub use timeline::{TimelineEvent, TimelinePage};
pub use tool_calls::SessionToolCall;

use crate::types::{
    Content, ContextSummary, Message, Role, Session, SessionStatus, SessionSummary,
};
use alms_core::job::{Job, JobId, JobSchedule, JobStatus, JobTerminalReason};
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

// Frozen inputs to migration 1. Once a migration ships, extend the schema by
// appending a new migration; never update these definitions in place.
const V1_BASELINE_SCHEMA: &str = "
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
    worktree_mode          TEXT,
    debug_mode             INTEGER NOT NULL DEFAULT 0
);

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

CREATE TABLE IF NOT EXISTS run_tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL,
    session_id TEXT,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    tool_name  TEXT,
    tool_id    TEXT,
    params     TEXT,
    result     TEXT,
    timestamp  TEXT NOT NULL,
    from_agent TEXT,
    tool_invocation_id TEXT
);

CREATE TABLE IF NOT EXISTS session_summaries (
    agent_id     TEXT NOT NULL,
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    summary      TEXT NOT NULL DEFAULT '',
    last_run_id  TEXT,
    updated_at   TEXT NOT NULL,
    source_label TEXT,
    PRIMARY KEY (agent_id, session_id)
);

";

const V1_BASELINE_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_agents_is_default ON agents(is_default);
CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_agent_id ON runs(agent_id);
CREATE INDEX IF NOT EXISTS idx_run_tool_calls_run ON run_tool_calls(run_id, seq);
CREATE INDEX IF NOT EXISTS idx_run_tool_calls_session ON run_tool_calls(session_id);
CREATE INDEX IF NOT EXISTS idx_session_summaries_agent
    ON session_summaries(agent_id, updated_at DESC);
";

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// SQLite-backed store for sessions, messages, and audit events.
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    persistence_snapshot_rejections: Arc<std::sync::atomic::AtomicU64>,
    stale_run_recovery_failures: Arc<std::sync::atomic::AtomicU64>,
    row_skips: Arc<row_skips::RowSkipCounters>,
    field_degradations: Arc<field_degradation::FieldDegradationCounters>,
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
            persistence_snapshot_rejections: Arc::clone(&self.persistence_snapshot_rejections),
            stale_run_recovery_failures: Arc::clone(&self.stale_run_recovery_failures),
            row_skips: Arc::clone(&self.row_skips),
            field_degradations: Arc::clone(&self.field_degradations),
        }
    }
}

impl SqliteStore {
    /// Open or create a SQLite database at the supplied path.
    pub fn open<P: AsRef<Path>>(path: P) -> AlmsResult<Self> {
        let mut conn =
            Connection::open(path).map_err(|e| AlmsError::Runtime(format!("SQLite open: {e}")))?;
        migrations::configure_and_apply(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            persistence_snapshot_rejections: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            stale_run_recovery_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            row_skips: Arc::default(),
            field_degradations: Arc::default(),
        })
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> AlmsResult<Self> {
        let mut conn = Connection::open_in_memory()
            .map_err(|e| AlmsError::Runtime(format!("SQLite open_in_memory: {e}")))?;
        migrations::configure_and_apply(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            persistence_snapshot_rejections: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            stale_run_recovery_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            row_skips: Arc::default(),
            field_degradations: Arc::default(),
        })
    }

    pub fn persistence_snapshot_rejections_total(&self) -> u64 {
        self.persistence_snapshot_rejections
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn record_persistence_snapshot_rejection(&self) {
        self.persistence_snapshot_rejections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Run rows that could not be reconciled by the startup stale-run sweep
    /// (#1236). Each one is a durable row still claiming `queued`/`running`
    /// from a dead process; the sweep logs the `run_id` and its remediation
    /// SQL, then continues so one bad row cannot keep the daemon down.
    pub fn stale_run_recovery_failures_total(&self) -> u64 {
        self.stale_run_recovery_failures
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(super) fn record_stale_run_recovery_failure(&self) {
        self.stale_run_recovery_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Quarantine one durable row the store could not parse (#1241).
    ///
    /// Call this instead of a bare `tracing::warn!` at every `filter_map` skip
    /// site. Dropping the row is the correct policy — an unparseable row is a
    /// row we cannot believe — but the quarantine rule
    /// (`docs/architecture.md` § "Reconciliation policy") requires the drop to
    /// be counted, not just logged, so a corrupting database is visible on
    /// `GET /operations/metrics` rather than only to whoever is tailing logs.
    ///
    /// **Most callers are loaders, where absence just means the row is not
    /// served. Three are not.** `delete_agent` (twice) and
    /// `migrate_telegram_context_ids` collect ids in order to delete or
    /// rewrite them, so a drop there *strands* durable rows that no later read
    /// repairs. Those are the policy's "third branch", and a new write-path
    /// caller owes the same two things they do: a comment naming what it
    /// strands, and a `detail` prefixed with the site. If a drop would instead
    /// cause something to be **deleted** that should have survived, this
    /// helper is the wrong answer — that is not quarantinable.
    ///
    /// `detail` carries whatever the caller already has in hand. For most
    /// sites that is only the parse error, because the column that failed to
    /// parse is often the id itself.
    ///
    /// **This must never touch `self.conn`.** Every call site runs inside a
    /// `query_map`/`filter_map` iteration while the caller still holds
    /// `self.conn.lock()`, and that lock is a `parking_lot::Mutex` —
    /// non-reentrant, so re-locking it on the same thread is a hard deadlock,
    /// not a panic. Concretely: do **not** re-query the row to give the log a
    /// better identifier. The daemon would hang on the first corrupt row it
    /// met in production, with no error and no panic to point at. If a site
    /// needs a stronger identifier, the caller must select it in the original
    /// query and pass it in via `detail`.
    pub(super) fn record_skipped_row(
        &self,
        table: PersistenceTable,
        detail: impl std::fmt::Display,
    ) {
        self.row_skips.record(table);
        tracing::warn!(
            table = table.as_str(),
            detail = %detail,
            "Skipping unparseable persistence row"
        );
    }

    /// Durable rows the store could not parse and therefore dropped, summed
    /// across every table (#1241).
    ///
    /// Non-zero means the daemon is serving an incomplete view of the
    /// database: an agent missing from the registry, a session missing from
    /// the sidebar, a message missing from a context window. For the loader
    /// sites the count is per *read*, not per distinct row — a corrupt row on
    /// a hot path is counted again on every load that passes over it, so the
    /// rate matters more than the absolute value.
    pub fn rows_skipped_total(&self) -> u64 {
        self.row_skips.total()
    }

    /// Per-table breakdown of [`Self::rows_skipped_total`], keyed by SQL table
    /// name. Always reports every known table, including the zeroes.
    pub fn rows_skipped_by_table(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.row_skips.by_table()
    }

    /// Per-table skip count. Test affordance for asserting that a specific
    /// loader quarantined a row.
    pub fn rows_skipped_for(&self, table: PersistenceTable) -> u64 {
        self.row_skips.get(table)
    }

    /// Record one durable column the parser could not read and replaced with a
    /// fallback, **keeping the row** (#1246).
    ///
    /// This is deliberately *not* [`Self::record_skipped_row`], and the two
    /// must not be merged. A skipped row is one the daemon declined to believe
    /// and kept out of live state; a degraded field is a row that **is** in
    /// live state with one column the parser could not read. Counting these
    /// under `persistence_rows_skipped_total` would break that counter's
    /// meaning — "rows the daemon cannot see" — because these rows are
    /// perfectly visible, just wrong.
    ///
    /// Reach for this only where dropping the row is *worse* than keeping it
    /// degraded; that argument has to be made per site, and the existing four
    /// are argued in [`field_degradation`]. If the row is unusable without the
    /// column, the answer is a row skip, not this.
    ///
    /// `detail` follows the [`Self::record_skipped_row`] convention: write
    /// paths prefix it with the site (`delete_agent <id>: ...`) so an operator
    /// can tell a write-path degradation from a loader's.
    ///
    /// **This must never touch `self.conn`**, for exactly the reason spelled
    /// out on [`Self::record_skipped_row`]: every call site runs while the
    /// caller still holds the non-reentrant `parking_lot` connection lock, so
    /// re-querying the row to improve the log message is a hard deadlock. Pass
    /// a stronger identifier in via `detail` instead.
    pub(super) fn record_degraded_field(
        &self,
        field: DegradedField,
        detail: impl std::fmt::Display,
    ) {
        self.field_degradations.record(field);
        tracing::warn!(
            field = field.as_str(),
            detail = %detail,
            "Degrading unparseable persistence field"
        );
    }

    /// Durable columns the store could not read and replaced with a fallback,
    /// summed across every field (#1246).
    ///
    /// Non-zero means the daemon is serving rows that are **present but
    /// wrong**, which is a different and generally worse fault than
    /// [`Self::rows_skipped_total`]: the degraded value is projected into live
    /// state rather than withheld from it, and it is invisible from the
    /// outside — a run whose `job_id` did not parse looks exactly like a run
    /// that simply has no job.
    ///
    /// Like the row-skip counter this counts *occurrences, not distinct rows*
    /// for the loader-backed fields. How fast that climbs depends on the
    /// field: the two `runs.*` columns are read only by the boot-time sweep
    /// and hydration, so they move once or twice per restart, while
    /// `session_summaries.last_run_id` sits on a live read path and is
    /// re-counted on every episodic-summary load.
    pub fn fields_degraded_total(&self) -> u64 {
        self.field_degradations.total()
    }

    /// Per-field breakdown of [`Self::fields_degraded_total`], keyed
    /// `<table>.<column>`. Always reports every known field, including the
    /// zeroes.
    pub fn fields_degraded_by_field(&self) -> std::collections::BTreeMap<&'static str, u64> {
        self.field_degradations.by_field()
    }

    /// Per-field degradation count. Test affordance for asserting that a
    /// specific parser degraded a specific column.
    pub fn fields_degraded_for(&self, field: DegradedField) -> u64 {
        self.field_degradations.get(field)
    }

    /// Return the latest successfully committed schema migration.
    pub fn schema_version(&self) -> AlmsResult<u32> {
        migrations::read_schema_version(&self.conn.lock())
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
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn str_to_job_status(s: &str) -> JobStatus {
    match s {
        "active" => JobStatus::Active,
        "completed" => JobStatus::Completed,
        "failed" => JobStatus::Failed,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Pending,
    }
}

fn job_terminal_reason_to_str(reason: JobTerminalReason) -> &'static str {
    match reason {
        JobTerminalReason::Completed => "completed",
        JobTerminalReason::DeadlineReached => "deadline_reached",
        JobTerminalReason::OperatorCancelled => "operator_cancelled",
        JobTerminalReason::RetryExhausted => "retry_exhausted",
    }
}

fn str_to_job_terminal_reason(s: &str) -> Option<JobTerminalReason> {
    match s {
        "completed" => Some(JobTerminalReason::Completed),
        "deadline_reached" => Some(JobTerminalReason::DeadlineReached),
        "operator_cancelled" => Some(JobTerminalReason::OperatorCancelled),
        "retry_exhausted" => Some(JobTerminalReason::RetryExhausted),
        _ => None,
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

/// #1246 scope note: this is an *enum* fallback, but it is not one of the mild
/// ones. The discriminator is not "enum versus foreign key" — it is whether the
/// fallback value is one an operator could legitimately have configured.
/// `reasoning_effort -> None` and `worktree_mode -> Off` pass that test;
/// `status -> Queued` does not, because "queued" is a claim about the world.
///
/// The consequence is a misdiagnosis: `load_all_runs` has no status filter, so
/// the row reaches `hydrate_from_store`, which classifies it as an
/// unreconciled queued row, drops it, and logs an error pointing at
/// `stale_run_recovery_failures_total` — a sweep that never touched the row,
/// because `mark_stale_runs_failed` filters `status IN ('queued','running')`
/// and a garbage status is not selected. The run vanishes from history and the
/// operator is sent to the wrong counter.
///
/// Left uncounted deliberately (#1246 counts foreign keys), but do not cite
/// this function as an example of a benign enum fallback.
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
    let lifecycle_revision: i64 = row.get(8)?;
    let terminal_reason_str: Option<String> = row.get(9)?;

    let retry_count: i64 = row.get(10)?;
    let last_error: Option<String> = row.get(11)?;
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

    let lifecycle_revision = u64::try_from(lifecycle_revision).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Integer, Box::new(e))
    })?;
    Ok(Job::from_persisted(
        JobId(id_uuid),
        AgentId(agent_uuid),
        prompt,
        schedule,
        str_to_job_status(&status_str),
        created_at.with_timezone(&chrono::Utc),
        next_run_at,
        last_run_at,
        lifecycle_revision,
        terminal_reason_str
            .as_deref()
            .and_then(str_to_job_terminal_reason),
        u32::try_from(retry_count).unwrap_or(u32::MAX),
        last_error,
    ))
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
    // Per-agent debug-mode toggle (issue #1003). Stored as INTEGER
    // (0 / 1); NULL is impossible because the column is `NOT NULL
    // DEFAULT 0` on both fresh schemas and ALTER-TABLE-migrated rows.
    // Any non-zero integer is treated as `true` to match the
    // SQLite-boolean idiom used by `is_default` above.
    let debug_mode: bool = row.get::<_, Option<i64>>(16)?.unwrap_or(0) != 0;

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
        debug_mode,
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
///   12: parent_run_id, 13: created_at, 14: resolved_config (#837),
///   15: lifecycle_revision, 16: terminal_reason
///
/// Takes `store` solely to record the two foreign-key degradations below on
/// `GET /operations/metrics` (#1246). It must not touch `store.conn`: every
/// caller is inside a `query_map`/`query_row` while holding the connection
/// lock, which is non-reentrant.
fn parse_run_row(store: &SqliteStore, row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
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
    // a future schema change drops or renames the column (which the
    // versioned migration runner rejects, but a manually altered database
    // can still produce), the strict `?` would propagate
    // `InvalidColumnIndex` / `InvalidColumnName` and poison every row
    // hydration; `.ok().flatten()` collapses both "missing column" and
    // "NULL cell" into `resolved_config: None` so triage degrades
    // gracefully — the run still hydrates with no snapshot. Do NOT
    // tighten this to `row.get(14)?` without auditing those failure
    // paths.
    let resolved_config_str: Option<String> = row.get(14).ok().flatten();
    let lifecycle_revision: i64 = row.get(15).unwrap_or_default();
    let terminal_reason: Option<String> = row.get(16).ok().flatten();

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
    // #1246: these two are *foreign keys*, not defaulted columns, so their
    // fallback is not the benign field-level degradation the rest of this
    // parser does. `job_id -> None` is not "the default job", it is "no job",
    // which is a claim about the world: `GET /runs` reports `job_id: null`
    // and `derive_trigger` labels the run "user" instead of "scheduled".
    //
    // It is *not* a cancellation hazard, despite how it reads.
    // `cancel_runs_for_job` only considers `Queued`/`Running` runs, and
    // `hydrate_from_store` refuses to project `Queued`/`Running` rows into the
    // live map at all (#1236/#1239) — so every run reaching live state through
    // this parser is terminal by construction, and live runs enter via
    // `insert_persisted_run` without round-tripping through here.
    //
    // Dropping the row is still the worse option, on the boot cost alone:
    // `mark_stale_runs_failed` collects with `collect::<Result<_, _>>()` and
    // `Gateway::new` propagates that with `?` (`alms-gateway/src/gateway.rs`),
    // so a row-level error here is an unbootable daemon. Hence: keep the row,
    // degrade the column, and make it visible. See `field_degradation` for the
    // full argument.
    let job_id = job_id_str
        .and_then(|s| match uuid::Uuid::parse_str(&s) {
            Ok(id) => Some(id),
            Err(e) => {
                store.record_degraded_field(
                    DegradedField::RunsJobId,
                    format_args!(
                        "run {run_id_str}: unparseable job_id {s:?}: {e}; the run is no longer \
                         attributable to its job, so it reports job_id: null and is labelled a \
                         user-triggered run instead of a scheduled one"
                    ),
                );
                None
            }
        })
        .map(alms_core::job::JobId);
    let parent_run_id = parent_run_id_str
        .and_then(|s| match uuid::Uuid::parse_str(&s) {
            Ok(id) => Some(id),
            Err(e) => {
                store.record_degraded_field(
                    DegradedField::RunsParentRunId,
                    format_args!(
                        "run {run_id_str}: unparseable parent_run_id {s:?}: {e}; run reads as \
                         top-level instead of as a subagent run"
                    ),
                );
                None
            }
        })
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

    let lifecycle_revision = u64::try_from(lifecycle_revision).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(15, rusqlite::types::Type::Integer, Box::new(e))
    })?;
    Ok(Run::from_persisted(
        RunId(run_id_uuid),
        SessionId(session_id_uuid),
        AgentId(agent_id_uuid),
        str_to_run_status(&status_str),
        input,
        response,
        error,
        usage,
        created_at,
        started_at,
        ended_at,
        job_id,
        parent_run_id,
        resolved_config,
        lifecycle_revision,
        terminal_reason,
    ))
}
