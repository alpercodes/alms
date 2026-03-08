# ALMS Tasks / TODO (triaged)

This is the running task list for ALMS. Keep it short, current, and merge-friendly.

## Status snapshot
- **Docs spine is in place** (`docs/index.md`, `api.md`, `events-and-audit.md`, `security-model.md`, `capability-model.md`, `approvals-ux.md`, `policy-reasons.md`, `artifacts.md`, plus Zeki's review).
- **2026-02-14:** Tool parameter schemas implemented, OpenAI API format fixed, LlmMessage.content nullability fixed, multiple compilation fixes applied, GatewayConfig env loading fixed, CLI health command functional. See `docs/agent-ux-requirements.md` for new UX requirements from Alper.
- **2026-03-07:** Run/event/approval/audit pipeline fully implemented. MVP HTTP API is end-to-end functional with SSE streaming, guarded posture approvals, event replay, and audit log. CI pipeline live on GitHub Actions.
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

22) Context compression (sliding-summary strategy)
- Implement sliding window: keep last N messages + a persistent summary prefix.
- Summary generated by a cheap LLM call when context exceeds threshold.
- Must not silently fall back to truncation — raise an error or log clearly if unavailable.
- Tested with deterministic mock LLM.
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
