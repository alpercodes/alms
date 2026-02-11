# ALMS MVP Plan (end-to-end, resist over-engineering)

This document captures a pragmatic MVP plan based on:
- Mustafa’s feedback
- Mesut’s repo review findings (wiring issues, duplication, crate cycles)

The core idea: **prove the foundation works** with a single daemon running end-to-end before expanding scope.

---

## 0) Guiding principle

**Ship a single working daemon + SQLite + one agent type end-to-end.**

That milestone proves the architecture is real. Everything else is detail.

---

## 1) MVP scope (what “done” means)

### Must-have (end-to-end)
1) **Daemon starts and stays up**
2) **HTTP API works**
   - `/health`
   - `POST /agent/run` (non-stream)
   - `POST /agent/run/stream` (SSE streaming) *(recommended)*
3) **SQLite-backed sessions**
   - create session by context
   - append messages
   - load history
4) **Single agent loop**
   - one agent type that can respond
   - tool-call loop works for at least one tool
5) **Tool execution (minimal)**
   - `echo` tool
   - optionally `shell_exec` in workspace-only mode behind policy
6) **Cron/job runner**
   - schedule one job
   - persist job + job_run
   - execute job with scoped capabilities
7) **Audit log**
   - tool invocations logged
   - job runs logged

### Explicitly NOT in MVP
- multiple channels (Telegram/WhatsApp) beyond one minimal adapter
- subagent orchestration beyond a stub
- plugin marketplace / signed bundles
- microVM isolation
- complex UI

---

## 2) Streaming choice: pick one

MVP recommendation: **SSE**.
- simpler than WebSockets
- proxy-friendly
- easy reconnect semantics

Use WebSockets only if you genuinely need bidirectional interactive control.

---

## 3) Crate/module strategy: start small, keep seams

See: `docs/mvp-structure.md` for the formal decision.

Pushback accepted: **avoid premature splitting into many crates**.

MVP target: 3–4 crates total, e.g.:
- `alms-core` (types/protocol)
- `almsd` (daemon: gateway+runtime+storage+scheduler+tools as modules)
- `alms-cli` (thin)
- optional `alms-channel` (only if it doesn’t complicate wiring)

Important: keep internal modules with clean boundaries so later extraction is easy.

---

## 4) Incremental migration path from current repo state

The repo currently has “wiring issues” (startup inconsistencies, wrong constructor usage, duplicated tool systems, capability drift). The plan is to converge safely:

### Step 1 — Single coherent startup path
- One entrypoint starts:
  - HTTP server
  - message loop (if any)
  - runtime + storage
- Remove/avoid duplicate “gateway start” patterns.

### Step 2 — SQLite session store
- Replace in-memory history as source of truth
- Keep an in-memory cache if needed, but DB is authoritative

### Step 3 — One tool registry + one capability model
- Decide which registry is canonical (recommended: sandbox-owned registry)
- Ensure all tool calls go through the same policy checks + audit

### Step 4 — One agent loop end-to-end
- LLM adapter mocked in tests; real provider behind config
- Ensure tool-call loop works

### Step 5 — Scheduler (cron) minimal
- Persist jobs
- Execute due jobs
- Record job_runs

### Step 6 — Add subagents later
- Only after the single-agent pipeline is stable

---

## 5) Testing-first guardrails (to avoid regressions)

- Deterministic time (tokio pause/advance)
- Mockable LLM adapter
- In-memory SQLite test harness
- Golden tests for SSE event sequences

Reference: `docs/testing-strategy.md`

---

## 6) MVP acceptance tests (definition of “foundation works”)

The foundation works when:
- a user can call `POST /agent/run` and get a response
- a user can call `POST /agent/run/stream` and receive a coherent SSE stream
- a tool call is executed and audited
- a cron job runs and is audited
- sessions and job history survive a daemon restart

---

*Authored by Mesut (2026-02-10), incorporating Mustafa’s feedback.*
