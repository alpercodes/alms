# Session Storage Strategy (Decision)

**Status:** Approved for MVP

## Decision
For MVP, ALMS will use **in-memory session state with periodic JSON snapshots** (via `MemoryStore`), with a future migration path to **append-only log + snapshots** or **SQLite** once concurrency and durability requirements are clear.

## Rationale
- **Speed to ship:** Snapshot storage is simple and matches current code.
- **Low operational burden:** No external services required for MVP.
- **Clear upgrade path:** Append-only log or SQLite can replace snapshot persistence without changing external APIs.

## MVP Requirements (hard requirements)
- Sessions and message history must survive process restarts via snapshot file.
- Snapshot location must be configurable.

### Correctness requirements (not “optimizations”)
- **Single-writer guarantee:** only the daemon process writes snapshots; internal persistence is serialized through one storage lane/queue.
- **Atomic snapshot writes:** write temp file in the same directory, `fsync`, then `rename`.
  - For stronger durability on Linux: `fsync` the directory after rename.
- **Rotation + corruption handling:**
  - Keep the last N snapshots (e.g., `snapshot.json`, `snapshot.json.1`, ...).
  - Store a `version` + `checksum` (or hash) in the snapshot.
  - On load failure, fall back to the last known-good snapshot and log loudly.

## Deferred Decisions
- Append-only log vs SQLite vs external DB.
- Compaction strategy for large histories.
- Multi-node consistency.

## Migration triggers (when to revisit the backend)
Re-evaluate snapshot storage if any of these become true:
- need multi-process access or concurrent writers
- need stronger crash-consistent auditing guarantees
- session history size or count grows beyond comfortable snapshot performance
- desire for robust querying/indexing across sessions/messages/jobs

## Next Steps
1. Implement atomic snapshot writes + rotation as per requirements above.
2. Add benchmarks for history size vs read/write latency.
3. Add tests: restart survival, corruption fallback, and rotation behavior.

**See also:**
- `docs/testing-strategy.md`
- `docs/mvp-plan.md`

---
*Date: 2026-02-10*
