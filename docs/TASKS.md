# ALMS Tasks / TODO (triaged)

This is the running task list for ALMS. Keep it short, current, and merge-friendly.

## Status snapshot
- **Docs spine is in place** (`docs/index.md`, `api.md`, `events-and-audit.md`, `security-model.md`, `capability-model.md`, `approvals-ux.md`, `policy-reasons.md`, `artifacts.md`, plus Zeki's review).
- **2026-02-14:** Tool parameter schemas implemented, OpenAI API format fixed, LlmMessage.content nullability fixed, multiple compilation fixes applied, GatewayConfig env loading fixed, CLI health command functional. See `docs/agent-ux-requirements.md` for new UX requirements from Alper.
- **2026-03-07:** Run/event/approval/audit pipeline fully implemented. MVP HTTP API is end-to-end functional with SSE streaming, guarded posture approvals, event replay, and audit log. CI pipeline live on GitHub Actions.
- **2026-03-09:** Review fixes (#24): safe UTF-8 truncation, kill_on_drop on shell_exec, env_clear for secret isolation, path traversal guard, argv/mode validation, fs_list cap, posture/temperature/max_tokens validation, 20 new tool tests, CLAUDE.md updated.
- **2026-03-09:** Sliding-summary context strategy (#22): rolling summary persisted per session in SQLite; LLM summarization call at configurable intervals; ContextBuilder.build() gains existing_summary param; falls back to truncation with warning on failure.
- **2026-03-09:** Background subagents + parallel tool execution (#28, #29): invoke_agent(background=true) fires non-blocking and returns task_id; get_task_result tool polls result; agent_loop uses join_all for concurrent tool calls; Coordinator fully wired with real AgentRuntime loops.
- **2026-03-09:** Extended tools (#23): shell_exec + fs_read/write/list builtins; per-run temperature/max_tokens/posture overrides via API; Settings UI and Audit panel extended; posture badge in header.
- **2026-03-09:** Fix #38 (CRITICAL): session context_id mismatch — `execute_run()` was creating shadow sessions by passing session UUID as context_id. Threaded `session.context_id` through all callers.
- **2026-03-09:** Fix #39 (HIGH): persisted AgentId via sidecar file `./data/agent_id`. Restarts no longer orphan workspace files, sessions, or jobs.
- **2026-03-09:** Fix #40 (HIGH): Telegram double-deserialization — `set_webhook`/`delete_webhook` used `TelegramResponse<bool>` as type param to `post()` which already unwraps the envelope.
- **2026-03-09:** Fix #41 (MEDIUM): scheduler sleep calculation — replaced `iter().find()` with peek-and-drain; filter cancelled jobs before firing.
- **2026-03-09:** Bearer auth (#42): `ALMS_AUTH_TOKEN` env var enables auth middleware on all routes except `/health`. Supports `?token=` query param for SSE.
- **2026-03-09:** Fix #45 (MEDIUM): session `last_activity` and `status` now write-through to SQLite on every `append_message()` and `archive_idle()`. `delete()` cascades to SQLite (messages, audit, summaries, session row). `SessionManager::with_store()` constructor for test injection.
- **2026-03-10:** Graceful shutdown (#43): CancellationToken-based shutdown. 6-phase sequence: stop HTTP → stop scheduler → abort fire loop → stop channel adapters → drain in-flight runs (30s timeout) → flush SQLite WAL. New runs rejected with 503 during shutdown.
- **2026-03-10:** Quick fixes (#31, #32, #33, #34): CreateSessionResponse.created now correct, dead coordinator message bus removed, Span::enter confirmed correct, UI agent ID aligned with server. Coordinator integration tests (#30): 8 tests covering dispatch, background lifecycle, cancel, timeout, poll.
- **2026-03-10:** Symlink bypass hardening (#25): `check_no_traversal()` replaced with `check_sandbox_path()` using `canonicalize()` + prefix check. New config: `tools.sandbox_root` (default "." = safe), `tools.shell_policy` ("sandboxed"/"unrestricted"). 7 new sandbox tests (165 total).
- **2026-03-08:** Token usage logging (#16) implemented: `prompt_tokens` + `completion_tokens` accumulated per run, surfaced in `run_finished` SSE and `GET /runs/{id}`. All pre-existing clippy warnings fixed — `make ci` now passes cleanly across all crates.

---

## P0 — Make it run (unblock reality)

1) Build environment: make the project compile reliably on the VPS
- Current risk (per Zeki): 4GB RAM / no swap → OOM during wasmtime/cranelift builds.
- Options: add swap, use a beefier build machine, or dev-toggle wasmtime.
- **Owners:** Mustafa (infra), Atlas

2) Real LLM end-to-end smoke ✅
- `scripts/smoke.sh` — tests health, sessions, /agent/run, /runs, SSE events, tool execution.
- `make smoke` (mock LLM) and `make smoke-real` (real LLM via OpenRouter).
- Covers: health check, session creation, legacy + canonical run APIs, status polling, SSE replay.
- **Owners:** Zeki (script), Atlas/Mustafa (run on build machine)

---

## P1 — MVP foundation (must work end-to-end)

3) Snapshot persistence: atomic + rotation + checksum + fallback ✅
- Implemented in `crates/alms-session/src/store.rs`.
- **Owners:** Atlas

4) Tool Sandbox ABI v0 in code ✅
- Allocator (`alms_alloc`), size limits, ABI envelope (`abi:0`), tests.
- **Owners:** Atlas, Mustafa

5) Minimal audit trail for tools + policy gate ✅ *(pending merge if PR not yet merged)*
- `feature/atlas-audit` implements minimal audit events + deny unknown tools.
- After merge, ensure audit records include `run_id` when available.
- **Owners:** Atlas

6) SSE streaming for runs ✅
- SSE endpoint exists + golden tests.
- **Owners:** Mustafa

7) Deterministic test harness for scheduler + timeouts ✅
- `alms-runtime/src/scheduler.rs`: `Scheduler` with `schedule_once`, `schedule_recurring`, `cancel`.
- Background runner uses `tokio::time::sleep_until` + `Notify` for instant wake on new jobs.
- 5 deterministic tests via `tokio::time::pause()` + `advance()` — all pass in 0ms.
- Covers: one-shot firing, early non-firing, recurring multi-fire, cancel, multi-job ordering.
- **Owners:** Atlas

---

## P2 — Converge on the Run/Event/Approval model (ALMS identity)

8) Canonical Run API (introduce without breaking MVP compatibility) ✅
- `POST /runs` + `GET /runs/{run_id}` + `GET /runs/{run_id}/events` implemented.
- `/agent/run` + `/agent/run/stream` kept as deprecated compatibility aliases.
- Tool events (`tool_start`, `tool_end`) wired via RuntimeEvent channel.
- **Owners:** Atlas, Mustafa

9) Approvals end-to-end (guarded posture) ✅
- `approval_required` → pause (oneshot channel) → `approval_resolved` → continue.
- `GET /approvals` (list pending, filterable by session_id) + `POST /approvals/{id}` (approve/deny).
- Guarded posture blocks if no event_sender attached (security guarantee).
- `clear_for_run()` cleans up stale approvals on run completion.
- **Owners:** Atlas, Mustafa

10) Event persistence (in-memory event log) ✅
- In-memory per-run event log (`EventLogManager`) persists all events with sequential IDs.
- `GET /runs/{run_id}/events` replays missed events via `Last-Event-ID` header on reconnect.
- **Owners:** Atlas

11) Audit surfacing (minimal) ✅
- `GET /audit?session_id=<uuid>&limit=<n>` returns session audit events.
- In-memory for MVP; audit records include `run_id` on all tool events.
- **Owners:** Atlas

12) Tool parameter schemas (tool-call reliability) ✅
- `fn parameters(&self) -> Value` added to `Tool` trait with default empty schema.
- Real JSON Schemas implemented for echo, math, http_get builtins.
- Runtime `to_definitions()` wired to use `tool.parameters()`.
- OpenAI API format fixed: `{"type":"function","function":{...}}`.
- **Owners:** Zeki (approach), Atlas/Mustafa (implementation)

---

## P3 — Persistence upgrade (post-MVP foundation, but should be planned now)

13) SQLite storage layer (sessions/messages/audit) ✅
- `SqliteStore` in `alms-session/src/sqlite.rs`: sessions, messages, audit_events tables.
- WAL mode + FK enforcement; schema applied on open (idempotent `CREATE TABLE IF NOT EXISTS`).
- `SessionManager::with_sqlite(config, db_path)`: loads all data on startup, write-through on every mutation.
- Gateway reads `ALMS_DB_PATH` env var — set it to enable persistence, omit for in-memory only.
- 7 unit tests covering roundtrip, upsert, ordering, and session isolation.
- **Owners:** Atlas

---

## P4 — Stability / quality

14) CI basics ✅
- `.github/workflows/ci.yml`: fmt-check + clippy + test + build-release on push/PR to main.
- `ALMS_LLM_MOCK=1` set in CI env; `Swatinem/rust-cache` for fast builds.
- Local: `make ci` runs the same checks.
- **Owners:** Mustafa

15) Documentation drift checks
- Keep `docs/api.md`, `docs/events-and-audit.md`, and implementation aligned.
- Add a “docs index” link in README (optional).
- **Owners:** Mesut

30) Coordinator integration tests ✅
- 8 tests added to `alms-coordinator/src/lib.rs` using mock LLM + in-memory SessionManager:
  a) `dispatch_foreground_success` — success path returns mock response text
  b) `dispatch_foreground_with_system_prompt` — custom system prompt works
  c) `dispatch_background_lifecycle` — dispatch_background → poll Running → Completed
  d) `cancel_subagent` — spawn + cancel succeeds
  e) `timeout_produces_failed` — 1ns timeout → Failed or Completed (scheduler-dependent)
  f) `poll_unknown_task_returns_error` — returns Err for random UUID
  g) `list_active_includes_spawned` — spawned task appears in list_active
  h) `cancel_unknown_task_returns_error` — cancelling unknown task returns Err
- **Owners:** Atlas

---

## P5 — Extended MVP: agent UX + cron + richer UI

> Goal: everything needed for the UI to show agent config, cron jobs, token usage, and multi-session switching.
> Logical implementation order: 16 → 17 → 18 → 19 → 20 → 21 → 22.

16) Token usage logging per run ✅
- `TokenUsage { prompt_tokens, completion_tokens }` added to `alms-core::run`.
- `agent_loop` accumulates usage across all LLM calls in a run; returned in `RunOutput`.
- `run_finished` SSE event carries `prompt_tokens` + `completion_tokens`.
- `GET /runs/{id}` response includes `usage: { prompt_tokens, completion_tokens }` via `RunStatusResponse`.
- Pre-existing clippy warnings across alms-channel, alms-coordinator, alms-sandbox fixed as a bonus.
- **Owners:** Atlas

17) `workspace_write` tool ✅
- `WorkspaceWriteTool` in `alms-runtime/src/workspace_tool.rs` implements `alms_sandbox::Tool`.
- Parameters: `{ file: "goals"|"memories", content: string, mode: "write"|"append" }`.
- `personality.md` is rejected — only `goals` and `memories` are agent-writable.
- Registered automatically by `AgentRuntime::with_workspace()`.
- 5 unit tests covering write, append, invalid file, missing content, and schema.
- **Owners:** Atlas

18) Workspace HTTP API ✅
- `GET /agents/{agent_id}/workspace` — returns all workspace file contents as `{ "files": { "personality.md": "...", ... } }`.
- `PUT /agents/{agent_id}/workspace/{file}` — user-facing write; accepts `personality`, `goals`, `memories`; body `{ "content": "..." }`.
- Both return 503 if `ALMS_WORKSPACE_DIR` is not set (opt-in feature).
- Implemented in `alms-gateway/src/workspace.rs`; routes wired in `server.rs`.
- **Owners:** Atlas

19) Jobs HTTP API + SQLite persistence ✅
- `POST /jobs`, `GET /jobs`, `GET /jobs/{id}`, `DELETE /jobs/{id}` implemented.
- `jobs` table in SQLite; `JobStore` with DashMap + write-through; jobs survive restart.
- `cancel()` returns `Option<bool>` to distinguish 404 from 409 Conflict.
- **Owners:** Atlas

20) Scheduler → agent run integration ✅
- Fired job IDs sent via mpsc channel → `fire_job_run` → `execute_run`.
- `Run.job_id` links run back to the triggering job; visible in `GET /runs/{id}`.
- `record_run()` updates `last_run_at`, `status`, `next_run_at` atomically.
- Recurring jobs re-armed with next cron time after each firing.
- `bootstrap_scheduler()` re-registers persisted jobs on startup.
- Cancellation mid-run is detected before re-arm (guard in `fire_job_run`).
- **Owners:** Atlas

21) Extended web UI ✅
- **Multi-session sidebar** — `GET /sessions` (new), list + switch + new session button.
- **Agent config panel** — right drawer: personality/goals/memories textareas with Save; 503 handled gracefully.
- **Cron jobs panel** — right drawer: job list + create form (Once/Recurring), delete/cancel.
- **Token usage** — per-run badge on agent messages; cumulative p/c shown in run history sidebar.
- **Run history** — `GET /runs?session_id=` (new), last 20 runs with status icon + token counts.
- 3-column layout: session sidebar | chat | right panel (toggled via header buttons).
- **Owners:** Atlas

23) Extended tools + full settings UI ✅
- `shell_exec` tool: argv array (no shell injection), cwd, env, timeout, stdout/stderr truncation.
- `fs_read`, `fs_write`, `fs_list` filesystem tools; all registered in default builtin registry.
- `CreateRunRequest` accepts `temperature`, `max_tokens`, `posture` per-run overrides.
- `GET /settings` now returns temperature, max_tokens, posture, context_strategy, enabled_tools.
- UI Settings modal extended: temperature, max_tokens, posture fields; server info block.
- UI header: posture badge (amber when guarded), Audit log button.
- Audit log right panel: shows tool name, allow/deny, timestamp, params for active session.
- Settings button turns amber when any override is active.
- **Owners:** Atlas

22) Context compression (sliding-summary strategy) ✅
- Rolling `ContextSummary { text, messages_covered, updated_at }` persisted per session in SQLite.
- `maybe_summarize()` triggers when uncovered messages minus recent_window >= summary_interval.
- LLM called with extend-or-create prompt (max 512 tokens, temp 0.3); falls back to truncation on failure.
- `ContextBuilder.build()` gains `existing_summary: Option<&str>` param; injects as second system message.
- 2 new context tests + `build_context` test converted to `#[tokio::test]`.
- **Owners:** Atlas

24) Review fixes ✅
- safe_truncate() helper using is_char_boundary — fixes UTF-8 panic on multi-byte output truncation.
- kill_on_drop(true) on shell_exec child — kills spawned process when timeout fires on Linux.
- cmd.env_clear() in shell_exec — daemon API keys/tokens no longer inherited by spawned processes.
- check_no_traversal() guard on fs_read/fs_write/fs_list — rejects .. path components.
- argv element type validation — non-string elements now error instead of silently becoming "".
- fs_write mode validation — unknown mode values now error instead of silently writing.
- fs_list hard cap at 500 entries — prevents LLM context flooding on large directories.
- Posture/temperature/max_tokens validation in execute_run — warns on unknown posture, clamps temperature.
- GET /settings: static tool name list instead of ToolRegistry instantiation per request.
- 20 new tests for safe_truncate, check_no_traversal, all 4 new tools.
- CLAUDE.md updated: current state, personality.md writability, SQLite, known issues.
- **Owners:** Atlas

---

## P6 — Quality of life

25) ~~Symlink bypass hardening for fs tools~~ ✅ DONE (2026-03-10)
- Replaced `check_no_traversal()` with `check_sandbox_path()` using `canonicalize()` + prefix check.
- New config: `tools.sandbox_root` (default ".") and `tools.shell_policy` ("sandboxed"/"unrestricted").
- Safe by default — fs tools sandboxed to cwd, shell_exec cwd restricted to sandbox_root.
- Set `sandbox_root = ""` or `shell_policy = "unrestricted"` for full access.
- Future: Landlock (Linux) or restricted OS user for true shell isolation (see security-model.md §4.4).
- **Owners:** Atlas

26) Context window visibility in UI
- Show current context token count in the chat header or sidebar.
- Show which context strategy is active (truncate / sliding-summary).
- Optional: "Clear context" button that sends a sentinel message to reset the rolling summary.
- **Owners:** Atlas

27) True token-by-token SSE streaming
- agent_loop currently buffers the full LLM response then emits run_finished.
- Implement streaming via the LLM client's streaming API and emit token_delta events per chunk.
- Chat UI already consumes token_delta events — backend just needs to produce them.
- **Owners:** Atlas

37) Add user.md to agent workspace ✅
- Added `user.md` as a fourth workspace file describing the user (name, preferences, background).
- `WorkspaceFile::User` variant added; included in `all()`, `build_system_prompt_prefix()` (as "## About the User"), and `agent_writable()`.
- `workspace_write` tool: `"user"` is a valid file target alongside personality/goals/memories.
- Workspace HTTP API: `GET` returns user.md content; `PUT .../workspace/user` accepted.
- Bootstrap prompt updated to collect user info and save to user.md.
- `needs_bootstrap()` unchanged — still keyed to absence of personality.md.
- 2 new tests: `test_write_and_read_user`, `test_write_user` (tool).
- **Owners:** Atlas

31) Fix agent ID mismatch (UI / server alignment) ✅
- The web UI was generating random UUIDs for new agents via `crypto.randomUUID()` instead of using the server's configured agent ID.
- Fix: `newAgent()` now reads `serverDefaults.agent_id` (from `GET /settings`) and falls back to `crypto.randomUUID()` only if unavailable. Boot sequence already used `serverDefaults.agent_id`.
- **Owners:** Atlas

44) SSE single-subscriber limitation
- `RunManager` stores one `mpsc::Sender` per run. A second browser tab connecting to the same run's event stream replaces the first subscriber's sender, cutting it off.
- There is also a narrow race window in `stream_run_events()` between `events_from()` (snapshot) and `register_sender()` (live channel) — events in that gap are missed. Last-Event-ID reconnect partially mitigates this.
- Fix: store a `Vec` of senders per run and broadcast to all; eliminate the snapshot→register gap by registering first, then replaying.
- **Owners:** Atlas

45) Session last_activity and status not persisted to SQLite ✅
- `append_message()` now writes through `last_activity` to SQLite after `touch()`.
- `archive_idle()` now writes through `status` to SQLite after setting to `Idle`.
- `delete()` cascades to SQLite: deletes messages, audit events, summaries, and session row.
- `SessionManager::with_store()` constructor added for test injection.
- 3 new integration tests: persist_last_activity, persist_idle_status, delete_from_sqlite.
- **Owners:** Atlas

32) Clean up dead coordinator scaffolding ✅
- Removed `AgentMessage` enum, `ProgressUpdate` struct, `message_tx`/`message_rx` fields, `message_sender()`/`process_messages()` methods, and fire-and-forget `message_tx.send()` calls from `run_subagent`.
- The message bus was scaffolding for a peer-mesh design that was rejected; no code consumed the messages.
- **Owners:** Atlas

33) Fix Span::enter() across .await in coordinator ✅
- Audited: the coordinator already uses `.instrument(span)` correctly on all spawned tasks. No `Span::enter()` guards are held across `.await` points. No fix needed.
- **Owners:** Atlas

34) Fix CreateSessionResponse.created always true ✅
- `create_session` now checks `has_session()` before `get_or_create()` and returns the correct `created` value.
- Added `SessionManager::has_session()` method.
- **Owners:** Atlas

28) invoke_agent tool (agent-to-agent delegation) ✅
- `invoke_agent(task, system_prompt?, background?)` spawns a subagent via the Coordinator.
- Foreground (default): blocks and returns `{ response }`. Background: returns `{ task_id }` immediately.
- `get_task_result(task_id)` polls the background task; returns running/completed/failed/cancelled.
- Parallel tool execution: agent_loop uses `join_all` so multiple tool calls run concurrently.
- **Owners:** Atlas

---

## P7 — Multi-agent (Coordinator)

29) Coordinator — wire execute_task to real AgentRuntime ✅
- Real `AgentRuntime::run()` call in `run_agent_loop()`; subagent gets own AgentId, session context, system prompt.
- Coordinator wired into AppState as `Arc<Coordinator>`; `execute_run()` registers both invoke_agent and get_task_result tools.
- Subagent SSE events forwarded into parent run's event stream via mpsc forwarding task.
- `GET /tasks`, `GET /tasks/{id}` HTTP endpoints expose coordinator state.
- Background mode: `dispatch_background()` fires non-blocking; `poll_task()` reads stored `completed_result`.
- **Owners:** Atlas

---

## P8 — Correctness bugs (breaks core promises today)

38) Fix session context_id mismatch in execute_run ✅
- `execute_run()` was calling `runtime.run(session_manager, &session_id.0.to_string(), input)` — passing the session UUID as context_id, which caused `get_or_create()` to create shadow sessions instead of using the original.
- Fix: threaded `session.context_id` through `execute_run()` from all three callers (`create_run`, `stream_run_legacy`, `fire_job_run`). Runtime now finds the correct session.
- **Owners:** Atlas

39) Persist AgentId across restarts ✅
- `Gateway::new()` was generating a fresh `AgentId` on every boot, orphaning workspace files, sessions, and jobs.
- Fix: sidecar file `./data/agent_id` stores the UUID. Precedence: `ALMS_AGENT_ID` env var > sidecar file > generate new. Self-heals on garbage input. `Display`/`FromStr` added to `AgentId`.
- Recovery for existing data: `echo "<uuid>" > ./data/agent_id`
- 5 new tests covering create/load/stability/invalid/config-passthrough.
- **Owners:** Atlas

40) Fix Telegram double-deserialization bug ✅
- `set_webhook()` and `delete_webhook()` were using `T = TelegramResponse<bool>` with `post()`, which already unwraps the envelope — causing double-wrapping that always failed on parse.
- Fix: removed the redundant `TelegramResponse` wrapper; both methods now let `post()` return `bool` directly.
- **Owners:** Atlas

41) Fix scheduler sleep calculation ✅
- `run_loop()` was using `iter().find()` (unordered) to find the next job — could oversleep and delay jobs. `process_due_jobs()` was firing cancelled jobs on the channel.
- Fix: peek-and-drain cancelled entries from heap top in `run_loop()`; filter cancelled jobs before firing in `process_due_jobs()`.
- 3 new tests: cancelled-head-delay, cancelled-not-on-channel, all-cancelled-no-fire.
- **Owners:** Atlas

---

## P9 — Deployment hardening (required before VPS goes public)

42) Bearer token authentication ✅
- Axum middleware in `auth.rs` checks `Authorization: Bearer <token>` against `ALMS_AUTH_TOKEN` env var.
- Also accepts `?token=<token>` query param for SSE EventSource (browser can't set headers).
- `/health` is public; all other routes require auth. If `ALMS_AUTH_TOKEN` is unset, auth is disabled (dev mode, logs warning).
- 7 unit tests covering valid/invalid/missing/malformed/query-param cases.
- **Owners:** Atlas

43) Graceful shutdown ✅
- `CancellationToken` threaded through `AppState`, scheduler, and gateway message loop.
- 6-phase shutdown: stop HTTP (axum `with_graceful_shutdown`) → stop scheduler (`start_with_shutdown` exits cooperatively) → abort fire loop → stop channel adapters (`run_until_shutdown` selects on token, calls `stop()`) → drain in-flight runs (`RunManager` AtomicUsize counter + Notify, 30s timeout) → flush SQLite WAL (`PRAGMA wal_checkpoint(TRUNCATE)`).
- New runs rejected with 503 `SHUTTING_DOWN` during drain.
- Ctrl+C on all platforms, SIGTERM on Unix.
- 7 new tests: drain_immediate, drain_waits, drain_timeout, shutdown_stops_scheduler, flush_wal.
- **Owners:** Atlas

---

## Docs index
Start here:
- `docs/index.md`

Spine:
- `docs/api.md`
- `docs/events-and-audit.md`
- `docs/security-model.md`
- `docs/capability-model.md`
- `docs/approvals-ux.md`
- `docs/policy-reasons.md`
- `docs/artifacts.md`
- `docs/zeki-review-2026-02-12.md`
- `docs/workflow-layer.md`
- `docs/ux-principles.md`

UX requirements:
- `docs/agent-ux-requirements.md`

Execution plan:
- `docs/mvp-plan.md`
- `docs/mvp-module-crate-structure.md`
- `docs/testing-strategy.md`
