# ALMS Tech Stack (proposed, updated)

ALMS is not “an app”. It’s closer to a **local-first agent operating system**:
- a long-running daemon that manages **agents + sessions + tools + scheduling**
- safe(ish) delegated access to the host (terminal/files/network)
- channels (Telegram/WhatsApp/etc.) and a UI
- auditability and policy enforcement

This document proposes a stack that makes the hard parts (state, isolation, concurrency) **boring and correct**, and keeps rapid iteration where it matters (UI/SDK).

> **North star:** A person can create agents, give them bounded autonomy, schedule work (cron), delegate to sub-agents, and safely grant host access — in a cohesive, well-designed system.

**See also:**
- `docs/security-model.md` (capabilities, approvals, audit)
- `docs/proposal.md` (consolidated findings + repo reality check)

---

## 1) Recommendation in one line

**Rust core daemon + SQLite state + WASM tool sandbox + HTTP + streaming (SSE/WS) API + TypeScript UI/SDK**.

### Why this is the best fit
- **Correctness under concurrency** is core to an Agent Loop Management System (timeouts, cancellation, queues, subagent lifecycles).
- **Isolation & capability enforcement** is a first-class requirement once you provide terminal/files access.
- **Durable state** must not rely on brittle file locking.
- **UX iteration speed** matters a lot, but belongs outside the core daemon.

---

## 2) Architecture overview (what ALMS is)

### 2.1 Components (conceptual)

1) **almsd (daemon)** *(naming TBD; current repo uses “gateway” in places)*
- Coordinator (multi-agent orchestration)
- Runtime (LLM I/O + tool-call loops)
- Scheduler (cron/jobs)
- Tool host (capability-gated tools)
- Session store (durable)
- Channels (Telegram/WhatsApp/etc.)
- API server (HTTP + streaming)

2) **Clients**
- Web UI / Desktop UI
- SDKs
- CLI

### 2.2 Explicit lifecycle semantics (must be designed, not implied)

**Main Agent**
- persistent, user-facing
- plans and delegates

**Subagents**
- ephemeral, timeboxed
- run with *scoped* capabilities
- report progress + final result

**Tools**
- per-invocation
- always capability-checked
- always auditable

**Cancellation rules**
- cancel parent session/run ⇒ cascades to subagents ⇒ cancels tool invocations (best-effort)

---

## 3) Core daemon stack (Rust)

### Language: Rust
Rust is the best fit for the daemon because ALMS’s hardest problems are:
- concurrency & cancellation correctness
- resource limits
- safe tool execution boundaries
- predictable latency (no GC pauses)

### Runtime & web stack
- Async runtime: **tokio**
- HTTP: **axum** + **tower**/**tower-http**
- Streaming: **SSE** for token deltas (simple) + WebSocket for bidirectional control if needed
- Observability: **tracing** + `tracing-subscriber`
- Config: `config` or `figment`

### Suggested internal module layout
- `coordinator/` — spawn/kill, routing, progress, aggregation
- `runtime/` — model adapters, prompts, tool-call loop
- `scheduler/` — cron parsing, job runner, job persistence
- `tools/` — capability model, tool registry, execution policies
- `storage/` — DB layer (SQLite), migrations, event log
- `channels/` — Telegram/WhatsApp/etc.
- `api/` — HTTP endpoints, SSE/WS

> Keep it **one process** early. Split into microservices only when proven necessary.

### Repo reality check (important)
The current repository has early implementation scaffolding, but also wiring issues (e.g. gateway/CLI startup inconsistencies, duplicated tool systems, capability model duplication). Treat this doc as the desired direction.

---

## 4) Durable state: SQLite first

### Why SQLite
OpenClaw-style JSON + file locks tends to fail under concurrency (races, corruption, TOCTOU). SQLite gives:
- transactions
- crash safety
- indexing
- straightforward backups
- a solid base for audit logs

### Rust DB libraries
- **sqlx** (async, compile-time checked queries) OR
- `rusqlite` (sync) + a single DB task/queue

### Data model (proposed)
At minimum:
- `sessions` (id, context_id, created_at, last_activity, status)
- `messages` (id, session_id, role, content_json, ts)
- `agents` (id, type, config_json)
- `runs` (id, session_id, agent_id, started_at, ended_at, status)
- `tool_invocations` (id, run_id, tool_name, params_json, result_json, status, ts, duration)
- `capability_grants` (principal, capability, scope, expires_at)
- `jobs` (id, schedule, payload, enabled)
- `job_runs` (id, job_id, started_at, ended_at, status, logs)
- `audit_log` (append-only: what happened, who requested, what was executed)

### Event log mindset
Even if you don’t fully event-source on day 1:
- treat tool calls, job runs, and session turns as **append-only events**
- build snapshots/views as needed

### Migrations
- treat DB schema as code: migrations in-repo, applied by the daemon at startup.

---

## 5) Scheduler / cron (in-core)

Cron is not a bolt-on; it’s the core autonomy feature.

### Requirements
- persistent schedules (stored in SQLite)
- job runner with concurrency limits + backpressure
- cancellation & timeouts
- retries (configurable)
- per-job capability scope
- job output delivery (to channels, to session logs)

### Safety posture
- creating/modifying cronjobs should usually require approval
- jobs run as principal `job:<id>` with scoped capabilities

---

## 6) Tools & host access (capability-gated)

### Key rule
**Never give “raw shell” by default.** Provide host tools that are *policy-controlled*.

### Two planes model (avoid confusion)

**Plane A — Host-privileged tools** (dangerous, high value)
- shell exec
- filesystem read/write
- network requests
- git
- process management

These are implemented natively (Rust) but must still:
- pass capability checks
- be auditable
- have strict resource limits
- optionally require human approval

**Plane B — Plugin tools (WASM)**
- third-party extensions
- deterministic compute
- transformations

WASM helps isolate plugin code, but the host must enforce capabilities for any host calls.

### Tool execution guardrails (minimum viable)
For tools like `shell_exec`:
- argv array (avoid `bash -lc` by default)
- per-invocation timeout
- stdout/stderr size caps
- cwd restrictions / workspace jail
- environment scrubbing
- allowlist/denylist patterns (optional)
- optional “require approval” mode
- all invocations recorded to `audit_log`

### Isolation roadmap
- MVP: controlled `std::process::Command` with strict limits
- Better: `bubblewrap`/`nsjail`/containers for tool processes
- Best: microVMs (Firecracker) for high-risk tools

---

## 7) WASM sandbox stack

### Why WASM
- portable plugin format
- controllable CPU/memory (fuel + limits)
- isolates third-party code

### Recommended
- WASM runtime: **wasmtime**
- Define a stable Tool ABI:
  - param passing
  - memory allocation protocol
  - result encoding
  - error semantics

> Until the ABI is stable, treat sandbox as **prototype** and keep plugin execution behind a strict ABI gate.

---

## 8) Networking posture (default-deny recommended)

Network is a common exfiltration vector.

Recommended defaults:
- outbound network **deny-by-default**
- allowlist only:
  - LLM endpoints (OpenAI/OpenRouter/etc.)
  - user-approved domains
- `http_get` (and similar) is capability-gated and audited
- SSRF protections: block localhost/metadata IP ranges unless explicitly allowed

---

## 9) API: HTTP + streaming (SSE/WS)

### Why
- stable boundary between daemon and UI/SDKs
- language-agnostic clients

### Endpoints (suggested)
- `POST /sessions` (create)
- `GET /sessions/:id`
- `POST /agent/run` (non-stream)
- `POST /agent/run/stream` (SSE)
- `POST /tools/execute`
- `POST /jobs` (create)
- `POST /jobs/:id/run` (manual trigger)
- `GET /jobs/:id/runs`

### Streaming events
Treat everything as an event stream with correlation IDs:
- `session_id`, `run_id`, `job_run_id`, `tool_invocation_id`
- events: token delta, tool_start, tool_end, subagent_start, subagent_progress, subagent_end, job_start/end

---

## 10) UI/SDK stack (TypeScript)

### Why TS here
- rapid iteration
- great ecosystem for UI
- easy SDK distribution

### Recommendation
- Web UI first (fastest iteration + simplest distribution)
- Desktop later if needed:
  - Tauri if you want tight native integration
  - Electron if you value speed-to-ship over footprint

SDK:
- TypeScript SDK first; Python later

---

## 11) LLM providers

### Strategy
Implement one internal “OpenAI-style” interface; then add adapters:
- OpenAI
- OpenRouter
- Anthropic
- local (ollama/vLLM) later

Requirements:
- consistent tool-call schema
- consistent streaming semantics
- usage accounting

---

## 12) Hybrid: when it makes sense (and when it doesn’t)

### The hybrid that *does* make sense
- **Rust daemon** (everything correctness/security-sensitive)
- **TypeScript** for UI + SDK
- optionally: a separate CLI in TS/Go if it accelerates developer UX

This works because the boundary is a clean API.

### Hybrid that is usually a mistake (early)
Splitting core backend logic across languages (e.g., Go services + Rust sandbox) before the single-process architecture is stable:
- doubles your models (capabilities/tools/messages)
- increases drift
- makes security policy enforcement harder

---

## 13) Non-negotiables (if ALMS should beat OpenClaw)

1) **SQLite/real DB for state** (avoid file-lock races)
2) **single capability model** (no parallel enums vs strings)
3) **one tool registry** (avoid runtime vs sandbox duplication)
4) **auditable tool execution** (every exec is recorded)
5) **backpressure + bounded queues** (avoid unbounded memory growth)

---

## 14) Immediate next steps (practical)

1) Decide final shape: `almsd` + `alms` CLI + UI
2) Make the gateway startup story coherent (single entrypoint)
3) Break crate cycles (protocol/types in `alms-core`)
4) Pick the single tool execution path (prefer sandbox-owned registry)
5) Add SQLite storage layer + migrations
6) Add cron/job tables + runner loop
7) Add capability grants + approval workflow design

---

## Appendix: Suggested crate boundaries (to avoid cycles)

A clean split that tends to work:
- `alms-core`: types + protocol structs (no tokio, minimal deps)
- `alms-storage`: sqlite + migrations
- `alms-tools`: capability model + tool registry + execution host
- `alms-runtime`: LLM adapters + tool-call loop (depends on tools)
- `alms-coordinator`: multi-agent orchestration (depends on runtime + storage)
- `alms-channel`: adapters
- `alms-gateway`: API server + channel wiring
- `alms-cli`: thin wrapper

---

## 15) Resource footprint: ALMS vs Node.js alternatives

### The claim

A Rust single-binary daemon should be dramatically lighter than a Node.js equivalent (OpenClaw, etc.):
- **Idle memory**: ~5–15 MB (Rust/tokio/axum) vs ~50–80 MB (Node.js/V8 baseline)
- **Under load**: No GC pauses, no V8 heap bloat — memory grows predictably with actual data, not runtime overhead
- **Startup time**: Sub-second cold start vs multi-second Node.js module loading
- **Disk footprint**: Single static binary (~10–20 MB stripped) vs node_modules tree

For the target use case — a personal agent daemon running 24/7 on a cheap VPS — this is the difference between a $5/mo box and a $20/mo one. That's a real product differentiator, not just a benchmark win.

### We have to prove it

These numbers are estimates. Before we can put them on a landing page or in a README, we need actual measurements. The methodology:

1. **Idle baseline**: Start ALMS daemon, let it settle, measure RSS. Do the same with OpenClaw.
2. **Under load**: Run N concurrent agent sessions with tool calls, measure peak RSS, p50/p99 response latency.
3. **Startup**: Time from process start to first successful `/health` response.
4. **Disk**: Compare release binary size vs OpenClaw's `node_modules` + runtime.

Benchmarks should be run on the same machine (the VPS at minimum — 4 GB RAM, realistic target hardware) and documented with reproducible scripts.

### Target

ALMS should be able to run comfortably on hardware where OpenClaw struggles or is impractical. If we can't demonstrate a meaningful resource advantage, the "Rust for efficiency" argument is just marketing. The goal is to prove it with numbers.

---

*Authored by Mesut (2026-02-10). Updated based on repo findings + proposal (same date).*
*§15 added by Tesla (2026-03-15).*