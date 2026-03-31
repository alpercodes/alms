# Bug Review — 2026-03-08

## Critical

- **[alms-gateway/src/runs.rs:108-128]** **Runtime event forwarder channel drop race.** `runtime_tx` is moved into the runtime but the spawned forwarder task holds `runtime_rx`. When `execute_run` completes, `runtime_tx` drops, signalling EOF to the forwarder — but if the forwarder is slow to drain buffered events, they can be lost before being forwarded to SSE. Fix: `drop(runtime_tx)` explicitly *after* awaiting the runtime, not when it falls out of scope mid-function.

- **[alms-session/src/store.rs:72]** **Double-init race in `load_snapshot`.** Uses `loaded.swap(true, Ordering::SeqCst)` to guard one-time loading, but two concurrent callers can both see `false`, both swap to `true`, then both execute the file read and overwrite each other's results. Fix: use a `OnceLock` or `Mutex<Option<()>>` to serialize the one-time init.

- **[alms-session/src/lib.rs:129-134]** **Race in `append_message` updating `last_activity`.** After appending the message, the code iterates `self.sessions.iter_mut()` to find the session and update `last_activity`. Between the append and the update, a concurrent modification could intervene. Fix: perform the `last_activity` update in the same `entry()` operation as the append, or look up by `session_id` directly (it's already on the message) via `self.sessions.get_mut(&session_id)`.

- **[alms-gateway/src/approvals.rs:168]** **Wrong fallback `SessionId` in `resolve_approval`.** When looking up `session_id` from `run_id`, if the run is not found it falls back to `SessionId::new()` — a fresh random UUID. The resulting SSE event is logged with an incorrect session_id that doesn't exist. Fix: return a 404 / log a warning and skip the event rather than fabricating a session ID.

## Medium

- **[alms-gateway/src/jobs.rs:72-78]** **Silent job scheduling failure on create.** If `cron_utils::compute_next_fire()` returns `None` during `create_job`, the job is persisted and returned as 201 Created but is never registered with the scheduler and will never fire. The user has no indication. Fix: return 422 if `compute_next_fire` returns `None`, or at minimum log a clear error and surface it in `GET /jobs/{id}` via a `status: "unschedulable"` field.

- **[alms-gateway/src/sse.rs:205-206, 217-218]** **Lossy SSE serialization.** `json_data()` swallows serialization errors by returning `Event::default().data("{}")`. The client receives an empty event with no indication anything went wrong. Fix: log the error at `error!` level so it's at least visible in server logs.

- **[alms-gateway/src/runs.rs:96-99, 153-156]** **Stale run object updates.** `get_run(run_id)` returns a cloned copy. `mark_running()` / `mark_completed()` mutate the clone, then `update_run()` re-inserts it. Any concurrent write between `get_run` and `update_run` is silently overwritten (last writer wins). For MVP single-run-per-session this is unlikely to bite, but it's a correctness hole. Fix: use a `DashMap::entry().and_modify()` pattern to do the mutation atomically.

## Low / Nitpick

- **[alms-runtime/src/agent.rs:283-285]** Error message "No response from LLM" fires when `response.choices` is empty, which is a valid JSON response — the message is slightly misleading. Should say "LLM returned empty choices array".

- **[alms-gateway/src/server.rs:219]** `events.truncate(limit)` with default `limit=100` is correct and matches docs. No issue, just noting it was checked.

## Clean (no issues found)

- `alms-session/src/job_store.rs` — DashMap + write-through, cancel/record_run logic all correct
- `alms-runtime/src/scheduler.rs` — binary heap + tokio::time, deterministic test coverage
- `alms-gateway/src/cron_utils.rs` — clean cron parsing with fallbacks
- `alms-gateway/src/approvals.rs` — DashMap + oneshot channel flow (except the fallback SessionId noted above)
- `alms-core/src/run.rs` — Run state machine and transitions look correct
- `alms-core/src/job.rs` — Job types clean
