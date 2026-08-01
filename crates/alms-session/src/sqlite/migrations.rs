//! Ordered, transactional SQLite schema migrations.
//!
//! Version 1 adopts and normalizes every schema shape accepted by the former
//! best-effort startup path. Later versions add lifecycle metadata and
//! normalize durable job terminal/retry state.

use super::{V1_BASELINE_INDEXES, V1_BASELINE_SCHEMA};
use alms_core::{AlmsError, AlmsResult};
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior, params};
use std::time::{Duration, Instant};

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

const CONNECTION_PRAGMAS: &str = "
PRAGMA foreign_keys=ON;
";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_RETRY_DELAY: Duration = Duration::from_millis(20);

const MIGRATION_TABLE: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    INTEGER PRIMARY KEY CHECK (version > 0),
    name       TEXT NOT NULL UNIQUE,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
";

#[derive(Clone, Copy)]
struct Migration {
    version: u32,
    name: &'static str,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "normalize_legacy_schema",
        apply: normalize_legacy_schema,
    },
    Migration {
        version: 2,
        name: "add_lifecycle_metadata",
        apply: add_lifecycle_metadata,
    },
    Migration {
        version: 3,
        name: "normalize_job_terminal_states",
        apply: normalize_job_terminal_states,
    },
];

pub(super) fn configure_and_apply(conn: &mut Connection) -> AlmsResult<()> {
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| runtime_error("configure busy timeout", error))?;
    conn.execute_batch(CONNECTION_PRAGMAS)
        .map_err(|error| runtime_error("configure SQLite connection", error))?;
    enable_wal(conn)?;
    apply_migrations(conn, MIGRATIONS)
}

fn enable_wal(conn: &Connection) -> AlmsResult<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0)) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory") => {
                return Ok(());
            }
            Ok(mode) => {
                return Err(AlmsError::Runtime(format!(
                    "SQLite refused WAL journal mode and selected {mode}"
                )));
            }
            Err(error) if is_lock_error(&error) && Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(WAL_RETRY_DELAY.min(remaining));
            }
            Err(error) => return Err(runtime_error("enable WAL journal mode", error)),
        }
    }
}

fn is_lock_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub(super) fn read_schema_version(conn: &Connection) -> AlmsResult<u32> {
    let (count, maximum): (u32, u32) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| runtime_error("read schema version", error))?;

    if count != maximum {
        return Err(AlmsError::Runtime(format!(
            "SQLite schema migration history is not contiguous: {count} row(s), maximum version {maximum}"
        )));
    }
    Ok(maximum)
}

fn apply_migrations(conn: &mut Connection, migrations: &[Migration]) -> AlmsResult<()> {
    validate_migration_list(migrations)?;

    {
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| runtime_error("begin migration bootstrap", error))?;
        transaction
            .execute_batch(MIGRATION_TABLE)
            .map_err(|error| runtime_error("create schema_migrations", error))?;
        transaction
            .commit()
            .map_err(|error| runtime_error("commit migration bootstrap", error))?;
    }

    let current = read_schema_version(conn)?;
    let target = migrations.last().map_or(0, |migration| migration.version);
    if current > target {
        return Err(AlmsError::Runtime(format!(
            "SQLite schema version {current} is newer than this binary supports ({target}); refusing to open"
        )));
    }

    for migration in migrations
        .iter()
        .copied()
        .filter(|migration| migration.version > current)
    {
        // IMMEDIATE obtains SQLite's single-writer reservation before we
        // re-read the version. Concurrent daemon startups therefore either
        // observe this step as already committed or apply it once; they never
        // race two ALTER sequences from the same stale version snapshot.
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| migration_error(migration, "begin transaction", error))?;
        let observed = read_schema_version(&transaction)?;
        if observed > target {
            return Err(AlmsError::Runtime(format!(
                "SQLite schema version {observed} is newer than this binary supports ({target}); refusing to open"
            )));
        }
        if observed >= migration.version {
            transaction
                .commit()
                .map_err(|error| migration_error(migration, "commit no-op", error))?;
            continue;
        }
        if observed + 1 != migration.version {
            return Err(AlmsError::Runtime(format!(
                "SQLite migration {} ({}) expected schema version {}, found {observed}",
                migration.version,
                migration.name,
                migration.version - 1
            )));
        }
        (migration.apply)(&transaction)
            .map_err(|error| migration_error(migration, "apply", error))?;
        transaction
            .execute(
                "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
                params![migration.version, migration.name],
            )
            .map_err(|error| migration_error(migration, "record version", error))?;
        transaction
            .commit()
            .map_err(|error| migration_error(migration, "commit", error))?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "Applied SQLite schema migration"
        );
    }

    Ok(())
}

fn validate_migration_list(migrations: &[Migration]) -> AlmsResult<()> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if migration.version != expected {
            return Err(AlmsError::Runtime(format!(
                "SQLite migration definitions are not contiguous: expected version {expected}, found {}",
                migration.version
            )));
        }
    }
    if migrations.last().map(|migration| migration.version) != Some(CURRENT_SCHEMA_VERSION) {
        return Err(AlmsError::Runtime(format!(
            "SQLite CURRENT_SCHEMA_VERSION is {CURRENT_SCHEMA_VERSION}, but the ordered migration list ends at {}",
            migrations.last().map_or(0, |migration| migration.version)
        )));
    }
    Ok(())
}

fn normalize_legacy_schema(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch(V1_BASELINE_SCHEMA)?;

    add_column_if_missing(transaction, "agents", "provider", "TEXT")?;
    add_column_if_missing(transaction, "agents", "telegram_token", "TEXT")?;
    add_column_if_missing(transaction, "agents", "thinking_budget_tokens", "INTEGER")?;
    add_column_if_missing(transaction, "agents", "reasoning_effort", "TEXT")?;
    add_column_if_missing(transaction, "agents", "gemini_thinking_budget", "INTEGER")?;
    add_column_if_missing(transaction, "agents", "summary_provider", "TEXT")?;
    add_column_if_missing(transaction, "agents", "summary_model", "TEXT")?;
    add_column_if_missing(transaction, "agents", "worktree_mode", "TEXT")?;
    add_column_if_missing(
        transaction,
        "agents",
        "debug_mode",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    add_column_if_missing(transaction, "messages", "seq", "INTEGER NOT NULL DEFAULT 0")?;
    transaction.execute("UPDATE messages SET seq = rowid WHERE seq = 0", [])?;

    add_column_if_missing(transaction, "run_tool_calls", "from_agent", "TEXT")?;
    add_column_if_missing(transaction, "run_tool_calls", "session_id", "TEXT")?;
    add_column_if_missing(transaction, "session_summaries", "source_label", "TEXT")?;
    add_column_if_missing(transaction, "runs", "parent_run_id", "TEXT")?;
    add_column_if_missing(transaction, "runs", "resolved_config", "TEXT")?;

    transaction.execute_batch(V1_BASELINE_INDEXES)?;
    Ok(())
}

fn add_lifecycle_metadata(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    add_column_if_missing(
        transaction,
        "runs",
        "lifecycle_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(transaction, "runs", "terminal_reason", "TEXT")?;
    add_column_if_missing(
        transaction,
        "jobs",
        "lifecycle_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(transaction, "jobs", "terminal_reason", "TEXT")?;
    Ok(())
}

fn normalize_job_terminal_states(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    add_column_if_missing(
        transaction,
        "jobs",
        "retry_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(transaction, "jobs", "last_error", "TEXT")?;
    transaction.execute(
        "UPDATE jobs SET status = 'completed' \
         WHERE status = 'cancelled' AND terminal_reason IN ('completed', 'deadline_reached')",
        [],
    )?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> rusqlite::Result<()> {
    if column_exists(conn, table, column)? {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
    ))
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn runtime_error(context: &str, error: rusqlite::Error) -> AlmsError {
    AlmsError::Runtime(format!("SQLite {context}: {error}"))
}

fn migration_error(migration: Migration, stage: &str, error: rusqlite::Error) -> AlmsError {
    AlmsError::Runtime(format!(
        "SQLite migration {} ({}) failed to {stage}: {error}",
        migration.version, migration.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration_one(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
        transaction.execute_batch("CREATE TABLE marker (value TEXT NOT NULL);")
    }

    fn failing_migration_two(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
        transaction.execute_batch(
            "ALTER TABLE marker ADD COLUMN phase_two TEXT;
             INSERT INTO table_that_does_not_exist VALUES (1);",
        )
    }

    fn successful_migration_two(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
        transaction.execute_batch("ALTER TABLE marker ADD COLUMN phase_two TEXT;")
    }

    fn successful_migration_three(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
        transaction.execute_batch("CREATE TABLE phase_three (value TEXT);")
    }

    #[test]
    fn failed_step_rolls_back_and_can_be_retried() {
        let mut conn = Connection::open_in_memory().unwrap();
        let failed = [
            Migration {
                version: 1,
                name: "create_marker",
                apply: migration_one,
            },
            Migration {
                version: 2,
                name: "fail_after_alter",
                apply: failing_migration_two,
            },
            Migration {
                version: 3,
                name: "add_phase_three",
                apply: successful_migration_three,
            },
        ];

        let error = apply_migrations(&mut conn, &failed).unwrap_err();
        assert!(error.to_string().contains("migration 2"));
        assert_eq!(read_schema_version(&conn).unwrap(), 1);
        assert!(!column_exists(&conn, "marker", "phase_two").unwrap());

        let repaired = [
            failed[0],
            Migration {
                version: 2,
                name: "add_phase_two",
                apply: successful_migration_two,
            },
            Migration {
                version: 3,
                name: "add_phase_three",
                apply: successful_migration_three,
            },
        ];
        apply_migrations(&mut conn, &repaired).unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 3);
        assert!(column_exists(&conn, "marker", "phase_two").unwrap());
    }

    #[test]
    fn migration_definitions_must_be_contiguous() {
        let mut conn = Connection::open_in_memory().unwrap();
        let invalid = [Migration {
            version: 2,
            name: "starts_too_late",
            apply: migration_one,
        }];
        let error = apply_migrations(&mut conn, &invalid).unwrap_err();
        assert!(error.to_string().contains("expected version 1"));
    }

    #[test]
    fn future_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_TABLE).unwrap();
        for version in 1..=CURRENT_SCHEMA_VERSION + 1 {
            conn.execute(
                "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
                params![version, format!("version_{version}")],
            )
            .unwrap();
        }

        let error = apply_migrations(&mut conn, MIGRATIONS).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("newer than this binary supports")
        );
    }

    #[test]
    fn non_contiguous_history_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_TABLE).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (2, 'gap')",
            [],
        )
        .unwrap();

        let error = apply_migrations(&mut conn, MIGRATIONS).unwrap_err();
        assert!(error.to_string().contains("not contiguous"));
    }
}
