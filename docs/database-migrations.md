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

**The newer-than-supported refusal is the one sanctioned fatal reconciliation
site in ALMS.** Everywhere else — the stale-run sweep, the job scheduler
bootstrap, every loader in `alms-session` — a row the daemon cannot repair or
parse is *quarantined* and the daemon boots anyway, because those sites behave
correctly while believing the row is absent. This guard is the exception
precisely because that is not true here: a completion record you cannot read is
indistinguishable from "not yet run", so absence would re-execute work that
already happened. That is also exactly the hazard the rollback section below
describes. See
[Reconciliation policy: absence must be a safe belief](architecture.md#reconciliation-policy-absence-must-be-a-safe-belief)
for the full rule, the test to apply at a new site, and the reasons **not** to
add an `--ignore-schema-version` escape hatch.

The other fail-closed conditions above are a different class: they mean the
schema itself cannot be established, so no row is interpretable at all. They
are upstream of the reconciliation rule, not exceptions to it.

WAL is now a startup requirement for file-backed databases. Some network
filesystems and restricted container volumes do not support SQLite WAL locking;
those deployments previously fell back silently to rollback-journal mode but
now fail with `SQLite refused WAL journal mode`. Move the database to a
WAL-capable local volume before upgrading. In-memory stores continue to use
SQLite's `memory` journal mode.

## Current versions

| Schema | Contents | Current binary | Checkpoint binary |
| --- | --- | --- | --- |
| 0 | Unversioned legacy database | Upgrades to 3 | Historical shapes only |
| 1 | Normalized `v0.2.4` schema and migration history | Upgrades to 3 | Read/write compatible |
| 2 | Lifecycle revision/reason columns on `runs` and `jobs` | Upgrades to 3 | Structurally compatible |
| 3 | Durable job retry state and distinct terminal outcomes | Current | Not semantically compatible |
| >3 | Future schema | Refuses to open | Not guaranteed |

Schema 3 adds durable retry state:

| Table | Column | Type/default | Meaning |
| --- | --- | --- | --- |
| `jobs` | `retry_count` | `INTEGER NOT NULL DEFAULT 0` | Persisted bounded dispatch attempts |
| `jobs` | `last_error` | nullable `TEXT` | Most recent dispatch failure |

The migration also normalizes legacy one-shot rows that encoded completion as
`status = 'cancelled'` with terminal reason `completed` or `deadline_reached`
to the distinct `completed` status. New writes may persist `completed` and
`failed`, so an older binary must not open a schema-3 database.

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

> ### ⚠️ Rolling back the binary without restoring the backup RE-EXECUTES COMPLETED JOBS
>
> This is a **fail-open** hazard, not a compatibility inconvenience. Nothing
> in the checkpoint binary stops it. If you redeploy
> `v0.2.4-pre-stabilization` and leave a schema-3 database in place, then
> roughly **one second after startup, every finished one-shot job and every
> retry-exhausted job fires again** — real agent prompts, real LLM spend, real
> side effects.
>
> The chain, verified against the checkpoint source:
>
> 1. The checkpoint has **no `sqlite/migrations.rs`**. The newer-schema
>    refusal guard arrived in #1224, *after* the checkpoint, so it opens a
>    schema-3 database without complaint.
> 2. Its `str_to_job_status` maps only `active` and `cancelled`. Everything
>    else — including the new `completed` and `failed` — falls through to
>    **`Pending`**.
> 3. Its `load_all_jobs` filter is `WHERE status != 'cancelled'`, so those
>    rows load.
> 4. Its `bootstrap_scheduler` skips only `Cancelled`, and `compute_next_fire`
>    clamps a past-due one-shot to one second out.
>
> **Restoring the pre-migration backup is mandatory, not advisory.** If no
> usable backup exists, do not roll back the binary — stay on the current
> release and open an issue instead.
>
> Leaving the schema-3 database in place fails *open* rather than closed
> because the checkpoint's own load filter excludes only `cancelled`, and
> making migration v3 write a status the checkpoint excludes would mean
> re-encoding completion as `cancelled` — exactly the ambiguity schema 3
> exists to remove. The hazard is therefore documented rather than designed
> away.

Schema 3 is not semantically compatible with the pre-stabilization checkpoint:
the checkpoint does not understand the `completed` and `failed` job states.
Rolling back code therefore also requires restoring the database backup taken
before the migration.

1. Stop every process that can access the database.
2. Preserve the schema-3 database separately for diagnosis.
3. Restore the verified pre-migration backup as `.alms/alms.db`.
4. Ensure no WAL/SHM files from schema 3 remain beside the restored file.
5. Run `PRAGMA integrity_check;`.
6. Start `v0.2.4-pre-stabilization` and verify core reads.

Step 3 is the step that prevents the re-execution described above. Do not skip
it, and do not start the checkpoint binary between steps 1 and 3.

If you must inspect a schema-3 database with the checkpoint binary and accept
no job execution at all, cancel every job first so the checkpoint's
`status != 'cancelled'` filter excludes them:

~~~sql
UPDATE jobs SET status = 'cancelled' WHERE status IN ('completed', 'failed');
~~~

Run that against a **copy**, never the preserved schema-3 original — it
destroys the completed/failed distinction schema 3 introduced.

Never edit or delete `schema_migrations` to force an older binary to open a
newer database. Restore a compatible snapshot instead.

## Adding a migration

1. Append exactly one entry to the ordered migration list.
2. Increment `CURRENT_SCHEMA_VERSION` to the same number.
3. State the supported rollback window explicitly; compatibility is not
   assumed for this private, undeployed project.
4. Add a frozen upgrade fixture and a rollback-on-failure regression.
5. Update the version matrix and backup/rollback notes.
6. Test fresh install, baseline upgrade, repeated startup, concurrent startup,
   failed-step retry, and the full workspace.

Do not modify a migration that may already have been used. Add a new ordered
step.
