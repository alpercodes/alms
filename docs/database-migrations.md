# Database migrations, compatibility, and rollback

ALMS stores durable state in SQLite at `.alms/alms.db`. The daemon applies
ordered migrations before loading any sessions, runs, jobs, or agents.

The source checkpoint immediately before the stabilization program is:

- Tag: `v0.2.4-pre-stabilization`
- Commit: `eadd4b3b664b8e263fd10470ee0b085303deb82c`

A source tag is not a database backup. Back up the database before deploying a
binary that may migrate it.

## Migration contract

Migration history is stored in `schema_migrations`:

| Column | Meaning |
| --- | --- |
| `version` | Positive, contiguous migration number |
| `name` | Stable migration name |
| `applied_at` | SQLite UTC commit timestamp |

Each ordered step runs inside its own `BEGIN IMMEDIATE` transaction. The
schema-version row is inserted in that same transaction after the migration
body succeeds. A failure therefore rolls back both the partial DDL/data changes
and the version record. Earlier committed migrations remain valid, and startup
can safely retry the failed step.

Concurrent startups serialize on SQLite's writer reservation and re-read the
version after acquiring it. A migration is applied once even if two processes
start against the same database.

Startup fails closed when:

- migration history contains a gap;
- the ordered migration list is not contiguous;
- a migration body or version insert fails; or
- the database version is newer than the binary supports; or
- a persistent database cannot enable WAL journal mode.

Do not catch and ignore migration failures. The daemon must not load data from
a schema it cannot prove is current.

WAL is now a startup requirement for file-backed databases. Some network
filesystems and restricted container volumes do not support SQLite WAL locking;
those deployments previously fell back silently to rollback-journal mode but
now fail with `SQLite refused WAL journal mode`. Move the database to a
WAL-capable local volume before upgrading. In-memory stores continue to use
SQLite's `memory` journal mode.

## Current versions

| Schema | Contents | Phase 2 binary | Checkpoint binary |
| --- | --- | --- | --- |
| 0 | Unversioned legacy database accepted by the old best-effort upgrader | Upgrades to 2 | Reads only the historical shapes it already supported |
| 1 | Migration history plus normalized `v0.2.4` tables, columns, indexes, and message sequence backfill | Upgrades to 2 | Read/write compatible |
| 2 | Adds lifecycle metadata columns on `runs` and `jobs` | Current | Read/write compatible because all changes are additive |
| >2 | Future schema | Refuses to open | Not guaranteed |

Schema 2 adds these fields without changing current runtime behavior:

| Table | Column | Type/default | Intended owner |
| --- | --- | --- | --- |
| `runs` | `lifecycle_revision` | `INTEGER NOT NULL DEFAULT 0` | Phase 3 transition API |
| `runs` | `terminal_reason` | nullable `TEXT` | Phase 3 terminal metadata |
| `jobs` | `lifecycle_revision` | `INTEGER NOT NULL DEFAULT 0` | Phase 3 transition API |
| `jobs` | `terminal_reason` | nullable `TEXT` | Phase 3 terminal metadata |

The checkpoint binary uses explicit column lists and can continue reading and
writing schema 2. Its inserts omit the new columns, so SQLite supplies `0` and
`NULL`. Phase 2 deliberately does not persist new job status strings or change
existing enum meanings.

Inspect the installed version with:

~~~sql
SELECT version, name, applied_at
FROM schema_migrations
ORDER BY version;
~~~

## Backup before deployment

Stop the ALMS daemon first when practical. Then use SQLite's online backup
command so WAL contents are included in one consistent snapshot:

~~~powershell
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
sqlite3 .alms/alms.db ".backup '.alms/alms-$stamp.db'"
sqlite3 ".alms/alms-$stamp.db" "PRAGMA integrity_check;"
~~~

The integrity check must return `ok`. Copy the backup to storage outside the
project directory before continuing with a high-risk deployment.

If `sqlite3` is unavailable, stop ALMS completely and copy `alms.db` together
with any `alms.db-wal` and `alms.db-shm` files as one set. Never copy only the
main file while the daemon is running.

## Roll back to the checkpoint binary

Schema 2 is intentionally readable by `v0.2.4-pre-stabilization`, so a binary
rollback from Phase 2 does not require a database restore:

1. Stop the current daemon.
2. Switch to or deploy `v0.2.4-pre-stabilization`.
3. Start the daemon against the existing schema-2 database.
4. Verify session, run, job, and agent reads before accepting traffic.

The checkpoint binary ignores `schema_migrations` and the four additive
lifecycle columns.

For a future schema not listed as checkpoint-compatible, restore the backup:

1. Stop every process that can access the database.
2. Preserve the failed/newer database separately for diagnosis.
3. Restore the verified backup as `.alms/alms.db`.
4. Ensure no WAL/SHM files from the newer database remain beside the restored
   file.
5. Run `PRAGMA integrity_check;` against the restored file.
6. Start the compatible binary and verify the migration history and core data.

Never edit or delete `schema_migrations` to force an older binary to open a
newer database. Restore a compatible snapshot instead.

## Adding a migration

1. Append exactly one entry to the ordered migration list.
2. Increment `CURRENT_SCHEMA_VERSION` to the same number.
3. Keep the change additive throughout the documented rollback window.
4. Use nullable columns or defaults that preserve old-writer behavior.
5. Add a frozen upgrade fixture and a rollback-on-failure regression.
6. Update the compatibility matrix and backup/rollback notes.
7. Test fresh install, baseline upgrade, repeated startup, concurrent startup,
   failed-step retry, and the full workspace.

Do not modify a migration that may already have shipped. Add a new ordered
step.
