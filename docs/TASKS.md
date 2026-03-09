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

30) Coordinator integration tests
- alms-coordinator has 0 tests across 640 lines of concurrent code (DashMap + oneshot channels + spawned tasks + timeout + cancellation). Single biggest quality gap in the codebase.
- Required tests (use `ALMS_LLM_MOCK=1` + in-memory `SessionManager`):
  a) `dispatch` foreground — success path returns response text
  b) `dispatch` foreground — LLM error propagates as `Err`
  c) `dispatch_background` + `poll_task` lifecycle: `Running` while in progress → `Completed` when done
  d) `cancel_subagent` — verify status becomes `Cancelled`, poll returns `PollResult::Cancelled`
  e) Timeout — `SubagentRequest` with very short timeout, verify `Failed` status
  f) `poll_task` on unknown task_id — returns `Err`
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

25) Symlink bypass hardening for fs tools
- check_no_traversal() rejects .. components but a symlink (e.g. ln -s /etc ./safe-link) bypasses it.
- Use std::fs::canonicalize() to resolve the real path, then check it is within an allowed root.
- Requires a configurable workspace root in GatewayConfig / alms.toml.
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

37) Add user.md to agent workspace
- Current workspace files (personality.md, goals.md, memories.md) conflate agent identity with user identity. memories.md is supposed to hold "user preferences" alongside agent learnings — these should be separated.
- Add `user.md` as a fourth workspace file: describes the human the agent is working with (name, working style, preferences, background, communication preferences).
- `personality.md` → describes the *agent* (tone, role, constraints). `user.md` → describes the *user*.
- Changes required:
  a) `AgentWorkspace`: add `user.md` to the file set, read it in `build_system_prompt_prefix()`
  b) `workspace_write` tool: add `"user"` as a valid file target (alongside goals/memories)
  c) Workspace HTTP API: include `user.md` in `GET /agents/{id}/workspace` response and `PUT` handler
  d) Bootstrap prompt: rewrite to fill out both `personality.md` (agent self-description) and `user.md` (user info) — "What should I call you?", "What's your background?", "How do you prefer to communicate?" → user.md
  e) `needs_bootstrap()`: still keyed to absence of `personality.md` — no change needed
- **Owners:** Atlas

31) Fix agent ID mismatch (UI / server alignment)
- The web UI generates its own random UUID for the agent and stores it in localStorage.
- The server has its own AgentId (generated at startup, exposed via `GET /settings` as `agent_id`).
- These are different — workspace files are keyed to the server ID, so the UI is siloed from them.
- Fix: UI boot sequence should read `agent_id` from `GET /settings` and use it as the default agent ID instead of generating one. The fix is already half-done — `GET /settings` returns `agent_id`.
- **Owners:** Atlas

44) SSE single-subscriber limitation
- `RunManager` stores one `mpsc::Sender` per run. A second browser tab connecting to the same run's event stream replaces the first subscriber's sender, cutting it off.
- There is also a narrow race window in `stream_run_events()` between `events_from()` (snapshot) and `register_sender()` (live channel) — events in that gap are missed. Last-Event-ID reconnect partially mitigates this.
- Fix: store a `Vec` of senders per run and broadcast to all; eliminate the snapshot→register gap by registering first, then replaying.
- **Owners:** Atlas

45) Session last_activity and status not persisted to SQLite
- `append_message()` updates `session.last_activity` in memory (via `touch()`) but does not write it to SQLite. `archive_idle()` changes `session.status` only in memory.
- On restart, SQLite reloads stale `last_activity` and `status` even though message history is correct. Sessions that were archived in memory are treated as active again after restart.
- Fix: call `store.save_session(&session)` after `touch()` in `append_message()` and after status changes in `archive_idle()`.
- **Owners:** Atlas

32) Clean up dead coordinator scaffolding
- `AgentMessage` enum, `process_messages()` method, `message_tx`/`message_rx` in Coordinator are unused since the peer-mesh design was rejected.
- Leaving unused concurrent code in place creates confusion (is it intentionally inactive? a bug?).
- Delete these; if peer-to-peer messaging is added later it should be designed fresh.
- **Owners:** Atlas

33) Fix Span::enter() across .await in coordinator
- Any `Span::enter()` guard held across an `.await` point is incorrect per tracing docs — can cause wrong span attribution and memory leaks.
- Audit `run_subagent` in `alms-coordinator/src/lib.rs` for sync span guards held across awaits; replace with `.instrument(span)`.
- **Owners:** Atlas

34) Fix CreateSessionResponse.created always true
- `create_session` uses `get_or_create` semantics but always returns `"created": true`, even for existing sessions. Misleading for clients.
- Either track whether the session was newly created and return the correct value, or remove the field.
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

40) Fix Telegram double-deserialization bug (HIGH)
- `set_webhook()` and `delete_webhook()` call `self.post::<_, TelegramResponse<bool>>(...)`, but `post<B, T>` internally parses the HTTP body as `TelegramResponse<T>`. With T = `TelegramResponse<bool>`, it tries to parse `{"ok":true,"result":true}` as `TelegramResponse<TelegramResponse<bool>>` — which fails because `result: true` is not an object.
- Since `start()` always calls `delete_webhook()` in polling mode (the default), Telegram startup fails with a JSON parse error whenever a bot token is configured.
- Fix: change the type parameter in `set_webhook` and `delete_webhook` to `bool` instead of `TelegramResponse<bool>`.
- **Owners:** Atlas

41) Fix scheduler sleep calculation (MEDIUM)
- `run_loop()` uses `BinaryHeap::iter().find(...)` to find the next job's run_at time. `BinaryHeap::iter()` is unordered — it may skip over the earliest job and return a later one, causing jobs to fire late.
- `process_due_jobs()` correctly uses `peek()`/`pop()` (ordered), so no jobs are lost, only potentially delayed.
- Fix: replace `iter().find(...)` with the heap's `peek()` and filter cancelled IDs from there, or maintain a secondary sorted structure.
- **Owners:** Atlas

---

## P9 — Deployment hardening (required before VPS goes public)

42) Bearer token authentication
- Every HTTP endpoint is currently open to any network client, including `shell_exec` and `fs_write`.
- Add an Axum middleware that checks `Authorization: Bearer <token>` against `ALMS_AUTH_TOKEN` env var.
- Skip auth for `GET /health` only. Return 401 for missing/wrong token.
- ~50 lines in `alms-gateway/src/server.rs`. High security impact, low effort.
- **Owners:** Atlas

43) Graceful shutdown
- SIGTERM currently kills in-flight LLM calls, background subagents, the scheduler loop, and the Telegram polling loop abruptly.
- Use `tokio::signal::ctrl_c()` (and SIGTERM on Unix) + a `CancellationToken` to coordinate shutdown:
  a) Stop accepting new requests
  b) Signal scheduler and channel adapter to stop
  c) Wait for in-flight runs to complete (with a timeout, e.g. 30s)
  d) Flush SQLite WAL
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
