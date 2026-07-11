use alms_session::sqlite::{CURRENT_SCHEMA_VERSION, SqliteStore};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

const CHECKPOINT_SCHEMA: &str = include_str!("fixtures/v0_2_4_schema.sql");

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|name| name.unwrap() == column)
}

fn migration_history(conn: &Connection) -> Vec<(u32, String)> {
    let mut statement = conn
        .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn fresh_install_reaches_current_schema() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("fresh.db");
    let store = SqliteStore::open(&path).unwrap();

    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(store);

    let conn = Connection::open(path).unwrap();
    assert_eq!(
        migration_history(&conn),
        vec![
            (1, "normalize_legacy_schema".to_string()),
            (2, "add_lifecycle_metadata".to_string()),
        ]
    );
    for (table, column) in [
        ("runs", "lifecycle_revision"),
        ("runs", "terminal_reason"),
        ("jobs", "lifecycle_revision"),
        ("jobs", "terminal_reason"),
    ] {
        assert!(column_exists(&conn, table, column), "{table}.{column}");
    }
}

#[test]
fn checkpoint_database_upgrades_without_data_loss() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("checkpoint.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(CHECKPOINT_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO agents
         (id, name, description, is_default, created_at, last_active, worktree_mode, debug_mode)
         VALUES (
             '11111111-1111-4111-8111-111111111111',
             'checkpoint-agent',
             'migrated agent',
             1,
             ?1,
             ?1,
             'off',
             0
         )",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions
         (id, agent_id, context_id, created_at, last_activity, status)
         VALUES (
             '22222222-2222-4222-8222-222222222222',
             '11111111-1111-4111-8111-111111111111',
             'web',
             ?1,
             ?1,
             'active'
         )",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (id, session_id, role, content, timestamp, seq)
         VALUES (
             'message-1',
             '22222222-2222-4222-8222-222222222222',
             'user',
             'preserve me',
             ?1,
             7
         )",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs
         (run_id, session_id, agent_id, input, status, created_at)
         VALUES (
             '44444444-4444-4444-8444-444444444444',
             '22222222-2222-4222-8222-222222222222',
             '11111111-1111-4111-8111-111111111111',
             'work',
             'completed',
             ?1
         )",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs
         (id, agent_id, prompt, schedule, status, created_at)
         VALUES (
             '55555555-5555-4555-8555-555555555555',
             '11111111-1111-4111-8111-111111111111',
             'work',
             '{\"type\":\"once\",\"run_at\":\"2026-07-11T00:00:00Z\"}',
             'pending',
             ?1
         )",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    drop(conn);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

    // Exercise production parsers and writers against the migrated
    // checkpoint database. These explicit-column paths must stay independent
    // of each table's physical column order.
    let sessions = store.load_all_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].context_id, "web");

    let agents = store.list_agents().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "checkpoint-agent");

    let runs = store.load_runs_by_session(sessions[0].id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].input, "work");

    let jobs = store.load_all_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].prompt, "work");

    store.save_run(&runs[0]).unwrap();
    assert_eq!(
        store.load_run(runs[0].run_id).unwrap().unwrap().input,
        "work"
    );
    store.save_job(&jobs[0]).unwrap();
    assert_eq!(
        store.load_job_by_id(jobs[0].id).unwrap().unwrap().prompt,
        "work"
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT content FROM messages WHERE id = 'message-1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "preserve me"
    );
    assert_eq!(
        conn.query_row(
            "SELECT seq FROM messages WHERE id = 'message-1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        7
    );
    let run_metadata: (i64, Option<String>) = conn
        .query_row(
            "SELECT lifecycle_revision, terminal_reason FROM runs
             WHERE run_id = '44444444-4444-4444-8444-444444444444'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(run_metadata, (0, None));
    let job_metadata: (i64, Option<String>) = conn
        .query_row(
            "SELECT lifecycle_revision, terminal_reason FROM jobs
             WHERE id = '55555555-5555-4555-8555-555555555555'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(job_metadata, (0, None));
}

#[test]
fn repeated_startup_is_idempotent() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("restart.db");

    for _ in 0..3 {
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    }

    let conn = Connection::open(path).unwrap();
    assert_eq!(migration_history(&conn).len(), 2);
}

#[test]
fn concurrent_startup_applies_each_version_once() {
    let directory = tempdir().unwrap();
    for round in 0..5 {
        let path = directory.path().join(format!("concurrent-{round}.db"));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let store = SqliteStore::open(path).unwrap();
                    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let conn = Connection::open(path).unwrap();
        assert_eq!(migration_history(&conn).len(), 2);
    }
}

#[test]
fn checkpoint_style_writes_remain_valid_after_upgrade() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("rollback-compatible.db");
    let store = SqliteStore::open(&path).unwrap();
    drop(store);

    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO runs
         (run_id, session_id, agent_id, input, status, created_at)
         VALUES ('old-run', 'session', 'agent', 'input', 'queued', ?1)",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO jobs
         (id, agent_id, prompt, schedule, status, created_at)
         VALUES ('old-job', 'agent', 'prompt', '{}', 'pending', ?1)",
        ["2026-07-10T00:00:00Z"],
    )
    .unwrap();

    let run: (String, i64, Option<String>) = conn
        .query_row(
            "SELECT status, lifecycle_revision, terminal_reason
             FROM runs WHERE run_id = 'old-run'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(run, ("queued".to_string(), 0, None));

    let job: (String, i64, Option<String>) = conn
        .query_row(
            "SELECT status, lifecycle_revision, terminal_reason
             FROM jobs WHERE id = 'old-job'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(job, ("pending".to_string(), 0, None));
}

#[test]
fn legacy_unversioned_columns_are_normalized() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("legacy.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, context_id TEXT NOT NULL,
            created_at TEXT NOT NULL, last_activity TEXT NOT NULL, status TEXT NOT NULL
        );
        CREATE TABLE messages (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL,
            content TEXT NOT NULL, timestamp TEXT NOT NULL, metadata TEXT
        );
        CREATE TABLE agents (
            id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '', model TEXT, system_prompt TEXT,
            posture TEXT, is_default INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, last_active TEXT NOT NULL
        );
        CREATE TABLE runs (
            run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent_id TEXT NOT NULL,
            input TEXT NOT NULL DEFAULT '', response TEXT, error TEXT,
            status TEXT NOT NULL DEFAULT 'queued', started_at TEXT, ended_at TEXT,
            prompt_tokens INTEGER, completion_tokens INTEGER, job_id TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE run_tool_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL,
            seq INTEGER NOT NULL, role TEXT NOT NULL, tool_name TEXT,
            tool_id TEXT, params TEXT, result TEXT, timestamp TEXT NOT NULL
        );
        INSERT INTO messages
            (id, session_id, role, content, timestamp)
            VALUES ('legacy-message', 'session', 'user', 'legacy', '2026-07-10T00:00:00Z');
        ",
    )
    .unwrap();
    drop(conn);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(store);

    let conn = Connection::open(path).unwrap();
    for (table, column) in [
        ("agents", "provider"),
        ("agents", "debug_mode"),
        ("messages", "seq"),
        ("runs", "parent_run_id"),
        ("runs", "resolved_config"),
        ("run_tool_calls", "session_id"),
        ("run_tool_calls", "from_agent"),
    ] {
        assert!(column_exists(&conn, table, column), "{table}.{column}");
    }
    assert_eq!(
        conn.query_row(
            "SELECT seq FROM messages WHERE id = 'legacy-message'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[test]
fn migration_history_records_are_queryable_for_operators() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("operator-history.db");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(store);

    let conn = Connection::open(path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT version, name, applied_at
             FROM schema_migrations
             ORDER BY version",
        )
        .unwrap();
    let rows: Vec<(u32, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert_eq!(rows.len(), CURRENT_SCHEMA_VERSION as usize);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "normalize_legacy_schema");
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[1].1, "add_lifecycle_metadata");
    assert!(rows.iter().all(|(_, _, applied_at)| !applied_at.is_empty()));
}
