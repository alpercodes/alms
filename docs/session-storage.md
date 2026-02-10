# Session Storage Strategy (Decision)

**Status:** Approved for MVP

## Decision
For MVP, ALMS will use **in-memory session state with periodic JSON snapshots** (via `MemoryStore`), with a future migration path to **append-only log + snapshots** or **SQLite** once concurrency and durability requirements are clear.

## Rationale
- **Speed to ship:** Snapshot storage is simple and matches current code.
- **Low operational burden:** No external services required for MVP.
- **Clear upgrade path:** Append-only log or SQLite can replace snapshot persistence without changing external APIs.

## MVP Requirements
- Sessions and message history must survive process restarts via snapshot file.
- Snapshot location must be configurable.
- Snapshot writes should be atomic (write temp + rename) once we optimize.

## Deferred Decisions
- Append-only log vs SQLite vs external DB.
- Compaction strategy for large histories.
- Multi-node consistency.

## Next Steps (Post-MVP)
1. Implement atomic snapshot writes and rotation.
2. Add benchmarks for session history size vs read/write latency.
3. Choose long-term backend based on observed usage.

---
*Date: 2026-02-10*
