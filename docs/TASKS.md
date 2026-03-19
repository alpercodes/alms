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
- **2026-03-11:** SSE multi-subscriber (#44): `event_senders` changed from single sender to `Vec<Sender>` per run. Dead senders pruned via `retain()` on every broadcast. Register-before-replay eliminates snapshot→live gap; dedup filter on `max_replay_id`. 4 new tests (170 total).
- **2026-03-11:** Token-by-token SSE streaming (#27): `agent_loop` now uses `complete_stream` (SSE) instead of buffered `complete`. Token deltas emitted via `RuntimeEvent::TokenDelta` as chunks arrive. Proper SSE line-buffer parser handles TCP chunk boundaries. Tool call deltas accumulated incrementally. Falls back to buffered on streaming failure. Mock produces word-level chunks. 5 new tests (175 total).
- **2026-03-08:** Token usage logging (#16) implemented: `prompt_tokens` + `completion_tokens` accumulated per run, surfaced in `run_finished` SSE and `GET /runs/{id}`. All pre-existing clippy warnings fixed — `make ci` now passes cleanly across all crates.
- **2026-03-11:** Persistent named agents & CLI system design doc written (`docs/persistent-agents-cli-design.md`). Tasks #46–#54 formulated covering: agent registry, auto-migration, HTTP API, per-agent config, CLI commands (agent/session/run/job/dashboard), UI agent switching.
- **2026-03-12:** Agent registry (#46): `agents` table + `AgentRecord` in alms-core + `validate_agent_name` + CRUD on SqliteStore (`create_agent`, load/list/update/delete, `set_default_agent`, `touch_agent`). 9 store tests + 8 validation tests.
- **2026-03-12:** Agent auto-migration (#47): `migrate_sidecar_agent()` in Gateway::new() auto-registers sidecar agent ID into `agents` table on first boot. Idempotent, non-fatal. `SessionManager::store()` accessor added. 4 migration tests. Review fixes: `save_agent` renamed to `create_agent` (INSERT-only semantics); `set_default_agent` now errors on nonexistent ID.
- **2026-03-12:** CLI agent commands (#50): `alms agent {list, create, show, delete, set-default, config}`. Direct SQLite access, `--json` flag, name validation, delete guards. 11 tests.
- **2026-03-12:** Per-agent config overrides (#49): `execute_run()` merges per-agent model/system_prompt/posture from AgentRecord. Three-layer precedence: per-run > per-agent > server default. `touch_agent()` updates `last_active` after every run.
- **2026-03-12:** Agent HTTP API (#48): `/agents` CRUD endpoints (list, create, get, update, delete, set-default). `UpdateAgentRequest` type. `GET /settings` includes agents array. Path params accept UUID or name slug. 6 handler tests.
- **2026-03-12:** CLI session commands (#51): `alms session {list, show, delete}`. `list --agent NAME` filters by agent. `show` displays session details + message count + agent name. Direct SQLite access. `list_sessions()` + `load_session_by_id()` + `load_sessions_by_agent()` + `message_count()` on SqliteStore. 8 CLI tests + 4 store tests.
- **2026-03-12:** CLI run and job commands (#52): `alms run {create, list, show}` via HTTP API; `alms job {list, show}` via SQLite, `alms job {create, cancel}` via HTTP API. `--url` / `ALMS_GATEWAY_URL` for gateway address. Auth token forwarding. Schedule parsing ("once:"/"cron:"). `load_job_by_id()` + `load_all_jobs_unfiltered()`. 12 new tests (31 CLI total).
- **2026-03-12:** CLI dashboard + polish (#53): `alms dashboard` opens web UI via `open` crate. `alms completions <shell>` generates shell completions via `clap_complete`. `alms health --json` for machine-readable output. `--json` flag now consistent across all commands. 2 new tests (33 CLI total).
- **2026-03-12:** UI agent selector & management (#54): Agent selector dropdown in header, session sidebar (filtered by active agent), agent management section in settings modal (create/delete/set-default with bootstrap status badges), agents loaded from server API instead of localStorage.
- **2026-03-12:** Named subagent sessions (#69): `invoke_agent` gains `name` param for persistent subagent sessions. UUID v5 deterministic identity from parent session + name. 8 new tests.
- **2026-03-12:** Autonomous subagents design doc written (`docs/autonomous-subagents-design.md`). Tasks #70–#74 formulated covering: subagent workspaces, recursive spawning, auto-inject completion, progress reporting, SSE events.
- **2026-03-12:** `read_subagent_session` tool (#81): on-demand read of named subagent conversation history. Derives deterministic session ID, returns messages + summary. 8 tests.
- **2026-03-12:** Subagent workspaces + registry lookup (#70): `system_prompt` removed from `invoke_agent` tool. Named subagents looked up in agent registry for config. Workspace attached at `{workspace_dir}/{name}/`. `Coordinator.with_workspace_dir()` wired in gateway.
- **2026-03-12:** Fix #55 (CRITICAL): LLM streaming hang — two bugs in `llm_client.rs` SSE parser. (1) `[DONE]` sentinel didn't terminate the stream; `parse_sse_event` returned `None` which hit `continue` → fell through to `bytes.next().await`, hanging if server doesn't close connection (HTTP/2, OpenRouter proxy). Fix: tri-state `SseParseResult` enum (Chunk/Done/Skip); `Done` terminates the unfold immediately. (2) No per-chunk read timeout; `reqwest::Client::timeout` only covers initial `send()`, not body reads. Fix: `tokio::time::timeout(60s)` on each `bytes.next().await`. 1 new test.
- **2026-03-12:** Agent create workspace + CLI awareness (#84): CLI `alms agent create` and HTTP `POST /agents` now create workspace directory with empty identity files (personality.md, goals.md, memories.md, user.md). CLI outputs workspace path. Default system prompt tells agents they can run `alms --help` via shell_exec to discover CLI commands.
- **2026-03-13:** Fix #56 (CRITICAL): Telegram shutdown — `AtomicBool`/`AtomicI64` replaced with `Arc<Atomic*>` so `stop()` propagates to polling task. Also fixes #59 (offset desync).
- **2026-03-13:** Fix #57 (CRITICAL): Serial message processing — `handle_message().await` replaced with `tokio::spawn` per message. `SessionQueue<K>` added (`session_queue.rs`): per-session FIFO work queue backed by `DashMap<SessionId, UnboundedSender>`. Routes Telegram messages, HTTP `POST /runs`, and scheduler fire loop through the queue. 5 new tests (61 gateway total). `Gateway::run()` refactored to delegate to `run_until_shutdown()`.
- **2026-03-13:** Codex codebase review (`docs/codex-review-1303.md`): 6 findings — workspace identity split (#98), failed runs lose history (#99), default-agent not live (#100), config partially wired (#101), SSE subscription leak (#102), event log durability comments (#103), query-string auth scope (#104). Tasks added as P16.
- **2026-03-14:** Cancel button for in-progress runs (#91): `CancellationToken` per run, 4 cancellation checkpoints (loop top, LLM streaming, tool execution, approval wait), `POST /runs/{run_id}/cancel` endpoint, `run_cancelled` SSE event, UI stop button. PR #65.
- **2026-03-14:** Fixes #98 ✅, #99 ✅, #100 ✅ all merged. Channel tests (#62) already complete (29 tests exist). #91 ✅ merged.
- **2026-03-14:** Fix #111: context token limit + assembly fixes. No ordering bugs found — root causes were `max_input_tokens` too low (32k→128k), token estimation too optimistic (chars/4→chars/3), and silent history load failure (warn→error). PR #72.
- **2026-03-14:** Fix #108: agent shell_exec cwd defaults to workspace directory. Added `default_cwd` field to `ShellExecTool`, re-registered on workspace attach. Sandbox security model preserved.
- **2026-03-14:** Fix #107: new agent session unresponsive until page reload. `createAgentFromPanel()` now calls `switchAgent()` after creating agent, enabling input and loading session.

---

## P0 — Make it run (unblock reality)

1) Build environment: make the project compile reliably on the VPS
- Current risk (per Zeki): 4GB RAM / no swap → OOM during wasmtime/cranelift builds.
- Options: add swap, use a beefier build machine, or dev-toggle wasmtime.
- **Owners:** Mustafa (infra), Atlas

2) Real LLM end-to-end smoke ✅
- `scripts/smoke.sh` — tests health, sessions, /runs, SSE events, tool execution.
- `make smoke` (mock LLM) and `make smoke-real` (real LLM via OpenRouter).
- Covers: health check, session creation, canonical run API, status polling, SSE replay.
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
- Legacy `/agent/run` + `/agent/run/stream` removed (all callers migrated to canonical API).
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

27) ~~True token-by-token SSE streaming~~ ✅ DONE (2026-03-11)
- `agent_loop` now calls `complete_stream` (OpenAI SSE streaming) instead of buffered `complete`.
- `RuntimeEvent::TokenDelta` emitted per chunk; forwarded to SSE via `forward_runtime_events`.
- SSE line-buffer parser: handles TCP chunk boundaries via `futures::stream::unfold` accumulator.
- Streaming tool calls: `ToolCallDelta` accumulated incrementally by `index` across chunks.
- `stream_options: { include_usage: true }` requests usage in the final streaming chunk.
- Fallback: if streaming fails, falls back to buffered `complete()` with a warning.
- Mock mode: produces word-level chunks for realistic streaming behavior in dev/test.
- 5 new tests: SSE event parsing (content, [DONE], tool_call_delta, usage), mock multi-chunk stream.
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

44) ~~SSE single-subscriber limitation~~ ✅ DONE (2026-03-11)
- `event_senders` changed from `DashMap<RunId, Sender>` to `DashMap<RunId, Vec<Sender>>` — multiple tabs/clients receive events simultaneously.
- Dead senders pruned automatically via `retain()` on every `send_event()` broadcast.
- Race fix: `stream_run_events()` now registers the live channel BEFORE snapshotting the event log. Overlap deduplication via `max_replay_id` filter on the live stream.
- 4 new tests: multi_subscriber_broadcast, dead_subscriber_pruned, remove_senders_cleans_all, dedup_filters_replayed_events.
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
- Parallel tool execution: agent_loop uses `join_all` for concurrent tool calls in full-control posture; guarded posture runs them sequentially (one approval at a time).
- **Owners:** Atlas

110) Replace `fs_write` with search-and-replace edit tool
- Current `fs_write` overwrites the entire file, which is error-prone for small edits and wastes tokens sending full file contents.
- Should support targeted edits via old_string → new_string replacement (like Claude Code's Edit tool), so agents can make surgical changes without rewriting the whole file.
- Keep `fs_write` for full-file creates/overwrites; add a new `fs_edit` tool for partial edits.
- **Owners:** Atlas

---

## P7 — Multi-agent (Coordinator)

69) Named subagent sessions (persistent invoke_agent) ✅
- `invoke_agent` gains optional `name` parameter. When provided, subagent derives deterministic `(agent_id, context_id)` from parent session + name using UUID v5 (`AgentId::deterministic()`). `SessionManager::get_or_create` returns the same session across invocations — conversation history preserved.
- Empty name treated as None (ephemeral). `ALMS_NAMESPACE` constant for UUID v5 derivation.
- `SubagentDispatcher` trait gains `subagent_name: Option<String>` parameter on `dispatch` and `dispatch_background`.
- 8 new tests: deterministic ID properties, persistent session message count, ephemeral independence, name parameter threading.
- **Owners:** Atlas

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

## P10 — Persistent Named Agents & CLI System

> Design doc: `docs/persistent-agents-cli-design.md`
> Goal: multiple named, persistent agents per deployment + full CLI for managing agents, sessions, runs, jobs.
> Logical implementation order: 46 → 47 → 48 → 49 → 50 → 51 → 52 → 53 → 54.

46) Agent registry — data model + SQLite persistence ✅
- New `agents` table in `SqliteStore` SCHEMA: `id` (UUID PK), `name` (unique slug), `description`, `model`/`system_prompt`/`posture` (nullable overrides), `is_default` flag, `created_at`, `last_active`.
- `AgentRecord` struct in `alms-core/src/registry.rs`. `CreateAgentRequest` + `validate_agent_name()`.
- CRUD methods on `SqliteStore`: `create_agent`, `load_agent_by_id`, `load_agent_by_name`, `get_default_agent`, `list_agents`, `update_agent`, `delete_agent`, `set_default_agent` (errors on nonexistent ID), `touch_agent`.
- 9 store tests + 8 name validation tests (179+ total).
- **Owners:** Atlas

47) Agent auto-migration (single-agent → multi-agent) ✅
- `migrate_sidecar_agent()` in `Gateway::new()`: if `agents` table empty → auto-create `{ name: "default", id: <sidecar-uuid>, is_default: true }`.
- Idempotent (skips if agents table non-empty), non-fatal (all errors are `warn!` only).
- `resolve_default_agent_id()` still resolves the UUID; migration just registers it in SQLite.
- `SessionManager::store()` accessor added for downstream crate access.
- 4 migration tests (183+ total).
- **Owners:** Atlas

48) Agent HTTP API (`/agents` CRUD) ✅
- New `agents.rs` module in `alms-gateway` with 6 handler functions.
- Routes: `GET /agents`, `POST /agents`, `GET /agents/{id_or_name}`, `PUT /agents/{id_or_name}`, `DELETE /agents/{id_or_name}`, `POST /agents/{id_or_name}/default`.
- Path parameter accepts UUID or name slug (try UUID parse first, then name lookup).
- `GET /settings` expanded to include `agents` array: `[{ name, id, is_default, model }]`.
- `UpdateAgentRequest` in `alms-core/src/registry.rs` (flat optionals; empty string = clear override).
- Cannot delete default agent (409 Conflict). Name validation via `validate_agent_name()`.
- Coexists with existing `/agents/{agent_id}/workspace` routes (Axum 0.8 disambiguates by static segment).
- 6 unit tests for resolve_agent (by UUID, by name, not-found, preference) + validation.
- **Owners:** Atlas

49) Per-agent config overrides in run execution ✅
- `execute_run()` looks up agent's `AgentRecord` from SQLite and merges per-agent `model`/`system_prompt`/`posture` with server defaults.
- Three-layer precedence: per-run override > per-agent override > server default.
- `touch_agent(agent_id)` called after every run completion to update `last_active`.
- Agent lookup is fail-safe — errors absorbed, falls back to server defaults.
- **Owners:** Atlas

50) CLI — agent management commands ✅
- `alms agent {list, create, show, delete, set-default, config}` — 6 subcommands via clap.
- All commands open `./data/alms.db` directly via `SqliteStore` — no running gateway needed.
- Name validation via `validate_agent_name()`. Duplicate name detection. Default-agent delete guard with `--force` override.
- `--json` global flag for machine-readable output. Default: human-readable table/detail view.
- `config` subcommand: partial updates, empty string clears overrides.
- 11 unit tests covering resolve, CRUD roundtrip, duplicate/invalid name, set-default, delete guards, config update/clear.
- **Owners:** Atlas

51) CLI — session management commands ✅
- `alms session {list, show, delete}` — 3 subcommands via clap.
- `list --agent NAME` filters by agent (resolves by UUID or name slug).
- `show` displays session details (ID, agent, context, status, message count, timestamps) + agent name lookup.
- `delete` verifies session exists before deleting, cascades to messages/audit/summaries.
- Direct SQLite access (no gateway needed). `--json` flag for machine-readable output.
- New SqliteStore methods: `list_sessions()`, `load_session_by_id()`, `load_sessions_by_agent()`, `message_count()`.
- 8 CLI tests + 4 store tests (19 CLI total, 35 session total).
- **Owners:** Atlas

52) CLI — run and job commands ✅
- `alms run {create, list, show}` — all via HTTP API (runs are in-memory only, no SQLite table).
- `run create --session ID --input "text" [--model M] [--max-tokens N] [--posture P]` — calls POST /runs.
- `alms job {list, show}` — direct SQLite access (no gateway needed).
- `alms job {create, cancel}` — via HTTP API (gateway must be running for scheduler registration).
- `--url` / `ALMS_GATEWAY_URL` env var for gateway address. Auth token forwarded from `ALMS_AUTH_TOKEN`.
- `job list --agent NAME` filters by agent. `parse_schedule()` for "once:" and "cron:" formats.
- HTTP helpers: `api_get`, `api_post`, `api_delete` with auth + connection error handling.
- New SqliteStore methods: `load_job_by_id()`, `load_all_jobs_unfiltered()`.
- `Serialize` added to `CreateRunRequest`, `Deserialize` added to response types.
- 12 new tests (31 CLI total).
- **Owners:** Atlas

53) CLI — dashboard + polish ✅
- `alms dashboard [--url URL]` — opens web UI in system browser via `open` crate. `ALMS_GATEWAY_URL` env var support.
- `alms completions <shell>` — generates shell completions (bash/zsh/fish/elvish/powershell) via `clap_complete`.
- `alms health --json` — machine-readable health output. JSON error objects on failure with exit code 1.
- `--json` flag now consistent across all list/show commands (Health, Session, Agent, Run, Job).
- 2 new tests (33 CLI total).
- **Owners:** Atlas

54) UI — agent selector and management ✅
- Agent selector dropdown in the header (next to posture badge).
- Switching agents filters sessions and shows that agent's workspace.
- Agent management section in settings drawer: create/delete/configure agents.
- Each agent shows workspace bootstrap status (`needs_bootstrap()`).
- Session sidebar replaces old agent sidebar — shows sessions for active agent with new-session button.
- Agents loaded from server (`GET /settings` agents array) instead of localStorage.
- **Owners:** Atlas

---

## P11 — Telegram Adapter Rework

> Findings from end-to-end Telegram workflow review (2026-03-12).
> The adapter works for basic demo but has 4 critical correctness bugs and several reliability gaps.
> Logical implementation order: 56 → 57 → 58 → 59 → 60 → 61 → 62 → 63.

56) Fix Telegram shutdown — stop signal never reaches polling task ✅
- `receive_updates()` cloned `TelegramChannel` with independent `AtomicBool`/`AtomicI64`. `stop()` on original never reached polling task.
- Fixed: replaced with `Arc<AtomicBool>`/`Arc<AtomicI64>` shared between original and clone.
- **Owners:** Atlas

57) Fix serial message processing — head-of-line blocking ✅
- `handle_message().await` blocked the `tokio::select!` loop. Fixed by spawning each message as a `tokio::spawn` task with `Arc`-wrapped dependencies. Per-session FIFO ordering added via `SessionQueue` — messages to the same session execute sequentially, different sessions concurrently. Same queue used for HTTP `POST /runs` and scheduler fire loop.
- **Owners:** Atlas

58) Fix polling latency — remove unnecessary interval ticker (CRITICAL)
- `interval(5s)` wraps a 30s long-poll `getUpdates`. After the HTTP call returns, the loop waits an extra 5 seconds before re-polling. Messages sit undelivered during the gap.
- Fix: loop directly on `get_updates()` — the 30s Telegram timeout IS the wait mechanism. Add short sleep (1-5s with backoff) on error only.
- **Owners:** Atlas

59) Fix update offset desync — shared state between original and clone ✅
- Cloned `AtomicI64` for `last_update_id` was disconnected from the original. Fixed by #56's `Arc<AtomicI64>` change.
- **Owners:** Atlas

60) Handle Telegram 4096-character message limit
- `sendMessage` rejects text >4096 chars. LLM responses (especially with tool output) regularly exceed this. User gets no reply; error is only logged.
- Fix: split long responses at sentence/paragraph boundaries into multiple messages.
- **Owners:** Atlas

61) Fix HTML parse_mode breaking LLM output
- `send_message()` always sets `parse_mode: "HTML"`. LLM responses containing `<`, `>`, `&` are rejected as malformed HTML.
- Fix: either escape the text for HTML, use no parse_mode (plain text), or use MarkdownV2 with proper escaping.
- **Owners:** Atlas

62) Add alms-channel tests ✅
- Added 29 tests across 3 files: `telegram/mod.rs` (convert_update, api_url, polling offset), `telegram/types.rs` (builder + serde), `alms-core/channel.rs` (Command::parse, IncomingMessage). Total: 37 tests in alms-channel.
- **Owners:** Atlas

63) Persist Telegram update offset to sidecar file ✅
- If the process crashes after processing an update but before the next `getUpdates` with incremented offset, Telegram redelivers the update → duplicate reply.
- Fix: persist `last_update_id` to sidecar file (`./data/telegram_offset`) after processing each batch; restore on startup.
- Done in PR #186 (atomic write on Unix, direct write on Windows, negative-value clamping, 6 persistence tests).
- **Owners:** Atlas, Larry

106) Telegram message loop ignores per-agent config overrides
- The Telegram message handler in `gateway.rs` creates `AgentRuntime` with the server-default `agent_config` and `llm` client. It does not look up per-agent overrides (model, system_prompt, posture) from the agent registry.
- The HTTP run path (`runs.rs` `execute_run()`) correctly loads per-agent config via `apply_overrides()` and swaps the LLM model via `llm.with_model()`. The Telegram path bypasses this entirely.
- Effect: switching the default agent correctly changes workspace/sessions for Telegram, but the runtime still uses the server-default model and system prompt — not the per-agent overrides.
- Fix: extract the per-agent override logic from `execute_run()` into a shared helper. Call it in the Telegram message handler before creating the `AgentRuntime`.
- **Evidence:** `gateway.rs:279-282` vs `runs.rs:214-245`
- **Owners:** Atlas

105) Per-agent Telegram chat routing
- Currently all Telegram messages go to whichever agent is set as "default". There's no way to route specific chats to specific agents.
- Fix: allow configuring a Telegram chat ID → agent mapping in the agent registry or config. Each registered agent can optionally be bound to one or more Telegram chat IDs. Messages from those chats route to that agent; unmatched chats fall back to the default agent.
- This enables multi-agent setups where e.g. a "support" agent handles one group chat and a "dev" agent handles a private chat, each with their own workspace and personality.
- **Owners:** Atlas

---

## P12 — Agent Loop UX (dead air elimination + crash safety)

> Findings from agent loop review (2026-03-12).
> The SSE streaming works correctly, but there are significant dead-air gaps where the user sees nothing.
> Logical implementation order: 64 → 65 → 66 → 67 → 68.

64) Emit `status` SSE events during dead-air phases
- New `RuntimeEvent::Status { phase: String }` emitted at key moments so the UI can show what the agent is doing.
- Phases: `"building_context"` (before context build), `"summarizing"` (during sliding-summary LLM call), `"calling_llm"` (before each LLM call), `"executing_tools"` (before join_all).
- New `SseEventData::status(run_id, phase)` → SSE event type `status`.
- UI: show phase text in the chat area or status bar (e.g., "Thinking…", "Summarizing context…", "Running tools…").
- **Owners:** Atlas

65) Incremental session message persistence (crash safety)
- Currently, user message + assistant response are appended to session history only AFTER the entire agent loop finishes (`agent.rs:215-216`). Server crash mid-run = lost conversation.
- Fix: append user message to session BEFORE starting the agent loop. Append assistant message immediately after the loop returns. Consider appending tool-call/tool-result messages incrementally during the loop.
- **Owners:** Atlas

66) Partial response recovery on mid-loop errors
- If the agent loop fails on iteration 3 of 10, all accumulated tool results and partial text from iterations 1-2 are discarded. The user sees only `run_error`.
- Fix: if `agent_loop` fails but has accumulated content or completed tool calls, return partial output alongside the error (or emit a `token_delta` with the partial content before the error event).
- **Owners:** Atlas

67) UI: listen for `run_started` + show typing indicator
- `run_started` SSE event is emitted but the UI has no listener for it. Between send and first `token_delta`, the chat area is blank.
- Fix: on `run_started`, show a typing/thinking indicator in the chat area (pulsing dots or "Agent is thinking…"). Remove it when the first `token_delta` or `tool_start` arrives.
- **Owners:** Atlas

68) Style max-iterations as a warning, not normal text
- When `max_iterations` is reached, the agent returns `"[Max iterations reached]"` as a normal response. The user sees it as an ordinary agent message with no visual distinction.
- Fix: emit a `run_error` or a dedicated `run_warning` event instead, so the UI can style it appropriately.
- **Owners:** Atlas

---

## P13 — Autonomous Subagents

> Design doc: `docs/autonomous-subagents-design.md`
> Goal: subagents that behave like autonomous colleagues — own workspace, registry-based config, context isolation, recursive spawning.

### Phase 1 — Complete the autonomous flow

70) ~~Subagent workspaces + registry lookup~~ ✅ DONE
- `system_prompt` removed from `invoke_agent` tool parameters.
- Named subagents looked up in agent registry for config (system_prompt, model, posture).
- Workspace attached at `{workspace_dir}/{name}/` via `AgentWorkspace::with_dir()`.
- Model override applied via `llm.with_model()`. Posture from agent record respected.
- Ephemeral (unnamed) subagents: default config, no workspace.
- **Owners:** Atlas

81) ~~`read_subagent_session` tool~~ ✅ DONE
- Tool: `read_subagent_session(name, last_n?, summary_only?)`.
- Derives deterministic session ID, reads from `SessionManager`. 8 tests.
- **Owners:** Atlas

84) ~~`alms agent create` creates workspace directory~~ ✅ DONE
- When an agent is created via CLI or API, create `{workspace_dir}/{name}/` so parent can write workspace files (personality.md, goals.md, etc.) before the first invocation.
- **Owners:** Atlas

82) Truncate `invoke_agent` tool_results in parent session
- When `invoke_agent` returns, store a short summary in the parent's tool_result (not the full subagent response).
- Full response stays in the subagent's own session, accessible via `read_subagent_session`.
- Short responses (< 500 tokens) pass through unsummarized.
- **Owners:** Atlas

83) System prompt addition for subagent context model
- When subagent tools are registered, add instructions to parent's system prompt explaining the context model: invoke_agent returns summaries, read_subagent_session for full details.
- **Owners:** Atlas

75) Validate `name` in invoke_agent against `validate_agent_name()` rules
- Prevent invalid names from silently creating broken sessions.
- **Owners:** Atlas

### Phase 2 — Autonomous polish

71) Recursive subagent spawning — subagents can spawn sub-subagents
- Wire `invoke_agent` and `get_task_result` tools into subagent runtimes in `run_agent_loop`.
- Add `max_depth: u32` to `SubagentRequest` (default 3). Decrement on spawn, reject at 0.
- Requires `Arc<Coordinator>` threaded into `run_subagent`.
- **Owners:** Atlas

72) Auto-inject completed background results
- At top of each `agent_loop` iteration, check `Coordinator` for pending completed background tasks.
- Inject results as system messages before the LLM call — parent learns of completion without polling.
- `get_task_result` tool remains as explicit fallback.
- **Owners:** Atlas

77) ~~Guard concurrent invocations of same named subagent~~ **DONE (PR #179)**
- `DashSet` tracks active names; `spawn_subagent` rejects if already running. RAII drop guard ensures cleanup on panic.
- **Owners:** Atlas

### Phase 3 — Advanced orchestration

73) `report_progress` tool for intermediate status updates
- Named subagents can call `report_progress(status, progress_pct?)` during their loop.
- Progress stored on `SubagentHandle`. Emit `RuntimeEvent::SubagentProgress` for UI.
- **Owners:** Atlas

74) SubagentProgress SSE event type
- New SSE event type forwarded from subagent → parent run stream → UI.
- UI shows inline subagent status (e.g., "reviewer: analyzing file 3/7").
- **Owners:** Atlas

78) Task decomposition — parent LLM emits plans, coordinator breaks into subtasks
- **Owners:** Atlas

79) Subagent clarification requests — subagent can ask parent questions mid-loop
- **Owners:** Atlas

80) Cost budget per subagent tree — token budget enforcement across hierarchy
- **Owners:** Atlas

---

## P14 — Critical bugs (data integrity, security, races)

> These are the highest-priority open issues from the GitHub tracker.

85) Fix `delete_agent` orphaning sessions and jobs (GitHub #11)
- `delete_agent()` removes the agent row but leaves sessions and jobs pointing at a nonexistent agent ID.
- Fix: add ON DELETE CASCADE foreign keys, or explicitly delete dependent rows in a transaction before removing the agent.
- **Owners:** Atlas

86) Harden `shell_exec` sandbox escape (GitHub #32)
- `shell_exec` cwd is restricted, but the executed command itself can access files outside the sandbox root.
- Fix: use Landlock (Linux) or a restricted OS user to enforce true filesystem isolation for spawned processes.
- **Owners:** Atlas, Mustafa

87) ~~Fix race condition with concurrent named subagent invocations (GitHub #37)~~ **DONE (PR #179)**
- Concurrent same-name invocations rejected via `DashSet` guard with RAII cleanup.
- **Owners:** Atlas

88) ~~Fix SSE stream timeout returning partial content as complete (GitHub #30)~~ **DONE (PR #175)**
- Stream timeout now yields `Err` instead of `None`, discarding partial content. Agent loop falls back to buffered mode automatically.
- **Owners:** Atlas

---

## P15 — Web UI Polish

> The web UI is functional but needs significant UX work to be usable for real workflows.
> These tasks address the most pressing gaps identified during manual testing (2026-03-13).

89) Expandable tool calls in chat
- Tool call rows in the chat are truncated to a single line. Long tool names, params, or results are cut off with no way to see the full content.
- Fix: make tool rows expandable — click to reveal full params and result JSON. Consider a collapsible card layout similar to how Claude Code renders tool calls (summary line with expand toggle, syntax-highlighted detail on expand).
- **Owners:** Atlas

90) Full audit log viewer
- The Audit panel shows a compact list of recent events but offers no way to view the full event payload, filter by type, or paginate through history.
- Fix: make each audit row expandable to show full JSON payload. Add filter controls (by event type, time range). Support loading older events beyond the initial batch.
- **Owners:** Atlas

91) Cancel button for in-progress runs ✅
- Per-run `CancellationToken` with 4 checkpoints (iteration boundary, LLM streaming, tool execution, approval wait). `POST /runs/{run_id}/cancel` endpoint, `run_cancelled` SSE event, UI Stop button. PR #65.
- **Owners:** Atlas

92) General UI improvements
- Overall polish pass on the web UI: better spacing, responsive layout, keyboard shortcuts, loading states, error toasts, empty states, mobile-friendliness.
- Specific known gaps: no visual feedback when saving agent model, no confirmation on destructive actions beyond browser confirm(), chat scroll behavior on long responses, no way to copy agent/session/run IDs.
- **Owners:** Atlas

~~93) Duplicate of #98 — removed.~~

94) Tool calls not persisted in session history
- Tool call messages (tool_call + tool_result) do not appear to be saved in session history. When reloading a session, only user and assistant text messages are visible — tool interactions are lost.
- Investigate: check `agent.rs` session append logic — are tool_call/tool_result `LlmMessage` entries being stored, or only the final text response?
- **Owners:** Atlas

95) Run loop issues with subagent invocations
- When the agent invokes subagents via `invoke_agent`, there are issues with the run loop (exact symptoms TBD — may include hangs, duplicate events, or incorrect event routing).
- Investigate: check `invoke_agent` tool execution path, `Coordinator::run_subagent`, and how subagent events are forwarded to the parent SSE stream.
- **Owners:** Atlas

97) Dead code audit — thorough review for unused types, modules, and functions
- The codebase has accumulated scaffolding and speculative abstractions that are never used (e.g. the now-removed `Capability`, `AgentRole`, `SubagentType` enums).
- Do a systematic pass across all crates: check for unused pub types, unused functions, dead modules, stale imports, and orphaned test helpers. `cargo clippy` catches private dead code but not unused `pub` items.
- **Owners:** Atlas

96) Empty speech bubbles in chat
- An empty agent speech bubble sometimes appears in the chat with no text content.
- Likely cause: `getAgentBody()` creates a new bubble on `token_delta` but the LLM response starts with tool calls (no text), or a bubble is created after a tool but no subsequent text follows.
- Fix: suppress empty bubbles — don't create an agent bubble until there's actual text content, or remove empty bubbles on `run_finished`.
- **Owners:** Atlas

---

## P16 — Codex Review Findings (2026-03-13)

> Findings from the 2026-03-13 codebase review (`docs/codex-review-1303.md`).
> These address structural correctness issues that cause confusing runtime behavior.

98) HIGH: Workspace identity split — agent files created under name, read under UUID ✅
- Standardized on name-based workspace paths. `AgentWorkspace::new()` accepts name instead of UUID. One-time migration `migrate_workspace_dirs()` renames UUID dirs to name dirs on startup. PR #60.
- **Owners:** Atlas

99) HIGH: Failed runs lose user message and leave no session trace ✅
- User message now appended to session history *before* entering the agent loop. On failure, a synthetic error marker message is appended. PR #61.
- **Owners:** Atlas

100) HIGH: Changing the default agent does not affect live gateway behavior ✅
- `default_agent_id` in AppState changed to `Arc<ArcSwap<AgentId>>`. Set-default handler updates both SQLite and live cell. Channel adapter reads live value. PR #64.
- **Owners:** Atlas

101) MEDIUM: Unified config partially wired — session config and bind address ignored
- `AlmsConfig.session` is defined and loaded but `gateway.rs:67-82` hard-resets to `SessionConfig::default()`.
- `server.bind` / `ALMS_BIND` is loaded into config but the CLI startup path ignores it unless `--bind` is passed manually.
- This undermines the "single source of truth" config model — some sections look real but have no effect.
- Fix: plumb `config.session` into `GatewayConfig`. Make CLI `--bind` default to the loaded config value instead of hardcoded `127.0.0.1:8080`. Remove any config sections that aren't actually used.
- **Evidence:** `config.rs:18-24`, `gateway.rs:67-82`, `main.rs:29-32`, `main.rs:129-152`
- **Owners:** Atlas

102) MEDIUM: SSE subscription leak — subscribe to nonexistent/finished runs without error ✅
- `runs.rs:598-605` always registers a live sender before checking run state. Subscribing to a nonexistent or already-finished run gets an open SSE stream instead of a 404.
- Post-completion subscriptions leave stale `event_senders` entries that are never pruned (no future events to trigger cleanup).
- Fix: check run existence and status *before* registering a sender. Return 404 for nonexistent runs, return the historical event log for finished runs (then close), only register a live sender for active runs. Add periodic or disconnect-aware cleanup for orphaned entries.
- **Evidence:** `runs.rs:598-605`, `server.rs:93-99`, `server.rs:150-164`
- **Owners:** Atlas

103) LOW: Event log comments overstate durability
- `event_log.rs:1` says "durable SSE event storage" and `server.rs:44-45` says "persistent event log for reconnect-after-restart support", but storage is `Arc<RwLock<Vec<LoggedEvent>>>` — purely in-memory.
- Fix (short-term): downgrade comments to "in-memory replay during current process lifetime". Fix (long-term): persist replayable events to SQLite for actual cross-restart replay.
- **Evidence:** `event_log.rs:1`, `event_log.rs:21-23`, `event_log.rs:57-64`
- **Owners:** Atlas

104) LOW: Query-string auth token on all routes leaks credentials
- `auth.rs:19-20` and `auth.rs:40-45` allow `?token=...` query auth on every protected route, not just SSE.
- Bearer tokens in URLs leak into server logs, browser history, and HTTP Referer headers.
- Fix: restrict `?token=` query auth to SSE endpoints only (where `Authorization` headers aren't available from `EventSource`). All other routes require the `Authorization: Bearer` header.
- **Evidence:** `auth.rs:19-20`, `auth.rs:40-45`
- **Owners:** Atlas

---

## P-URGENT — User-reported bugs

107) URGENT: New agent session unresponsive until page reload ✅
- `createAgentFromPanel()` called `refreshAgents()` which updated the agent list but never switched to the new agent — input stayed disabled, no session created. Fix: parse the `POST /agents` response to get the new agent ID, then call `switchAgent()` which handles session loading + `enableInput()`.
- **Owners:** Atlas

108) URGENT: Agents start with cwd set to project root instead of workspace ✅
- Added `default_cwd` field to `ShellExecTool` (separate from `sandbox_root` security boundary). When `AgentRuntime.with_workspace()` attaches a workspace, shell_exec is re-registered with the workspace dir as default cwd. Priority: explicit cwd param > default_cwd (workspace) > sandbox_root > inherit from process.
- **Owners:** Atlas

109) URGENT: Cannot schedule messages/jobs from the UI
- The Jobs panel and create-job form exist in the UI, but scheduling does not work in practice. Needs investigation — could be a frontend form submission issue, backend endpoint error, or schedule parsing failure.
- **Owners:** Atlas

111) URGENT: Message ordering / context assembly issues ✅
- **Investigation found no ordering bugs** — context assembly is correctly ordered FIFO throughout (session storage via `rowid`, truncate/sliding-summary both reverse-walk correctly, tool call/result pairs stay adjacent).
- **Root causes**: (1) `max_input_tokens` defaulted to 32k — burns through in 3-4 tool-heavy iterations, causing context loss. Bumped to 128k. (2) Token estimation (`text.len()/4`) underestimated for JSON tool output. Changed to `div_ceil(3)` (~3 chars/token), safer for mixed content. (3) History load failure silently fell back to empty Vec — upgraded from `warn!` to `error!` with session ID for production visibility.
- Note: `max_tokens_per_run` config exists but is never enforced — cumulative token budget across iterations is not checked. Deferred.
- **Owners:** Atlas

112) URGENT: UI blocks input while agent is running — no message queuing
- The UI disables the text input and send button while a run is in progress (`disableInput()`). Users cannot type or queue follow-up messages.
- The backend `SessionQueue` already supports queuing multiple messages per session (FIFO), so the infrastructure exists — the UI just doesn't use it.
- Fix: keep the input enabled during active runs. Submitted messages should be queued as new runs via `POST /runs` and processed after the current run finishes. Show queued messages in the chat as pending.
- **Owners:** Atlas

113) MEDIUM: Clarify relationship between `SessionConfig::max_context_tokens` and `ContextConfig::max_input_tokens`
- Both default to 128000 but serve different purposes: `max_context_tokens` is a session storage limit (max messages/tokens to retain in the session), while `max_input_tokens` is the LLM context window budget (how many tokens to send per request).
- Their relationship is not documented anywhere. Users could easily confuse the two, set one but not the other, or not understand why changing one doesn't affect the other.
- Fix: (1) Add doc comments to both config structs explaining the distinction. (2) Add a note in `alms.toml.example` clarifying which controls what. (3) Consider whether `max_context_tokens` should default to something larger than `max_input_tokens` (it's the storage limit, so it could be higher), or whether one should derive from the other. (4) Validate that `max_context_tokens >= max_input_tokens` in `AlmsConfig::validate()` since it makes no sense to store fewer tokens than you'd try to send.
- **Evidence:** `config.rs:260-261` (`SessionConfig::max_context_tokens`), `config.rs:286` (`ContextConfig::max_input_tokens`)
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

Design:
- `docs/persistent-agents-cli-design.md`
- `docs/autonomous-subagents-design.md`

Execution plan:
- `docs/mvp-plan.md`
- `docs/mvp-module-crate-structure.md`
- `docs/testing-strategy.md`
