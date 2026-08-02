# ALMS Tech Stack Decision

ALMS is not “an app”. It’s closer to a **local-first agent operating system**:
- a long-running daemon that manages **agents + sessions + tools + scheduling**
- safe(ish) delegated access to the host (terminal/files/network)
- a Telegram channel adapter (other channels are target work) and an embedded browser UI
- auditability and policy enforcement

This decision record contains both the implemented baseline and explicit target requirements.
Sections labeled **Implemented** or **Current** describe repository reality. Sections labeled
**Target**, **Recommended**, **Roadmap**, or **Non-negotiable** are design direction and must
not be read as already shipped. The retained stack keeps the hard parts (state, isolation,
concurrency) boring and correct while allowing rapid browser-UI iteration.

> **North star:** A person can create agents, give them bounded autonomy, schedule work (cron), delegate to sub-agents, and safely grant host access — in a cohesive, well-designed system.

**See also:**
- `docs/security-model.md` (capabilities, approvals, audit)
- `docs/proposal.md` (historical 2026-02-10 proposal; not current repository status)

---

## 1) Recommendation in one line

**Rust core daemon + SQLite state + native tool registry + HTTP/SSE API + Preact/TypeScript UI**.

### Why this is the best fit
- **Correctness under concurrency** is core to an Agent Loop Management System (timeouts, cancellation, queues, subagent lifecycles).
- **Isolation and policy enforcement** are first-class requirements once terminal/files access is available; a unified capability-grant model is still target work.
- **Durable state** must not rely on brittle file locking.
- **UX iteration speed** matters a lot, but belongs outside the core daemon.

---

## 2) Architecture overview (what ALMS is)

### 2.1 Implemented components

1) **ALMS daemon**
- Coordinator (multi-agent orchestration)
- Runtime (LLM I/O + tool-call loops)
- Scheduler (cron/jobs)
- Tool host (policy-controlled tools; unified capabilities are future work)
- SQLite session/state store
- Telegram channel adapter; additional channels are future work
- HTTP/SSE gateway

2) **Implemented clients**
- Embedded browser UI
- Rust CLI

A desktop shell and public SDK are future distribution choices, not current components.

### 2.2 Current lifecycle and target semantics
**Main agent (implemented)**
- persistent and user-facing
- plans and delegates one level through `invoke_agent`

**Subagents (implemented unless noted)**
- ephemeral one-shot or named with persistent history
- inherit the configured sandbox/tool policy; per-invocation capability grants are future work
- return a final result; structured progress reporting and recursive spawning are future work

**Tools (implemented)**
- registered per runtime and governed by posture, filesystem/shell policy, resource limits, and audit paths
- a single capability-grant model across tools and jobs is a target, not current enforcement

**Cancellation (implemented, best effort)**
- cancelling a parent run cascades to attached subagents and active tool execution

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
- HTTP: **axum** + **tower**
- Streaming: **SSE** for active event delivery; `/ws` is a no-op compatibility endpoint, not a bidirectional control transport
- Observability: **tracing** + `tracing-subscriber`
- Config: **config** + TOML + environment overlays

### Implemented internal layout
- `alms-core` — shared IDs, lifecycle types, errors, and configuration
- `alms-session` — SQLite persistence and session state
- `alms-sandbox` — native tool execution and filesystem/shell policy
- `alms-tools` — agent-facing coordination and recall tools
- `alms-runtime` — provider adapters, prompts, context, and agent loop
- `alms-coordinator` — subagent lifecycle and peer MessageBus
- `alms-channel` — user-facing channel adapters
- `alms-gateway` — HTTP/SSE transport and process wiring
- `alms-cli` — command-line entrypoint

> Keep it **one process** early. Split into microservices only when proven necessary.

### Current implementation

The repository implements this as one daemon with an acyclic nine-crate
workspace. SQLite is authoritative for durable state, SSE is the primary event
transport, and production tools use one native registry path. Phase 8 changes
module ownership inside these boundaries; it does not change the stack.

---

## 4) Durable state: SQLite first

### Why SQLite
OpenClaw-style JSON + file locks tends to fail under concurrency (races, corruption, TOCTOU). SQLite gives:
- transactions
- crash safety
- indexing
- straightforward backups
- a solid base for audit logs

### Rust DB library

ALMS uses bundled **rusqlite**, with persistence ownership contained in
`alms-session` and explicit transactional boundaries for multi-record writes.

### Data model (implemented)
The SQLite store currently owns these tables:
- `sessions`, `messages`, `agents`, and `runs`
- `run_tool_calls`
- `jobs`
- `audit_events`
- `context_summaries` and `session_summaries`
- `schema_migrations`

There is no `capability_grants` or `job_runs` table today. A future unified
capability model may add grants; job executions currently use the existing job,
run, session, and audit records.

### Event log mindset
Even if you don’t fully event-source on day 1:
- treat tool calls, job runs, and session turns as **append-only events**
- build snapshots/views as needed

### Migrations
- treat DB schema as code: migrations in-repo, applied by the daemon at startup.

---

## 5) Scheduler / cron (in-core)

Cron is not a bolt-on; it’s the core autonomy feature.

### Implemented scheduler behavior
- schedules persist in SQLite
- bounded channels provide backpressure
- cancellation, shutdown, and restart recovery are covered by lifecycle tests
- job output is routed through run/session records and notification delivery

### Target additions
- configurable retry policy
- a unified per-job capability scope and job:<id> principal model
- approval policy for creating or modifying high-impact schedules

---

## 6) Tools & host access (policy-controlled; capabilities are target work)

### Current rule
ALMS exposes a native `shell` tool, so safety comes from guarded posture,
compiled allow/deny rules, destructive-command rejection, workspace boundaries,
timeouts, output caps, and audit records. A unified capability grant checked by
every tool invocation is not implemented yet.

### Two planes model (current and target)

**Plane A — Host-privileged tools** (dangerous, high value)
- shell execution
- filesystem read/write
- network requests
- git
- process management

These are implemented natively in Rust. Current enforcement includes tool-specific
policy, posture approval, filesystem boundaries, shell allow/deny rules, resource
limits, and audit recording. Uniform capability checks remain target work.

**Plane B — Plugin tools** *(future direction; not currently implemented)*
- third-party extensions
- deterministic compute
- transformations

A plugin substrate (for example WASM) is not currently in the codebase; earlier
wasmtime-based scaffolding was removed because no code called it. Any future
plugin host must enforce the same policy for privileged host calls.

### Implemented tool-execution guardrails
For `shell` and filesystem tools:
- `bash -c` command strings with a persistent working directory
- compiled shell allow/deny rules plus a hard destructive-command denylist
- per-invocation timeout and bounded output with spill files for large results
- workspace-root restrictions and read-before-write checks
- guarded-posture approval for risky operations
- invocation records in the audit/run stores

### Isolation roadmap
- MVP: controlled `std::process::Command` with strict limits
- Better: `bubblewrap`/`nsjail`/containers for tool processes
- Best: microVMs (Firecracker) for high-risk tools

---

## 7) Networking posture (default-deny recommended)

Network is a common exfiltration vector.

Recommended target defaults:
- outbound network deny-by-default, with explicit endpoint/domain grants
- SSRF protections for localhost and metadata ranges
- capability and audit enforcement shared with other host tools

Current state: `http_get` is a native registered tool, but the unified capability
grant model and network allowlist/SSRF layer remain future work.

---

## 8) API: HTTP + streaming (SSE)

### Why
- stable boundary between daemon, browser UI, CLI, and future clients
- language-agnostic clients

### Implemented surface

The gateway exposes resources for sessions, runs, agents, jobs, approvals,
settings, audit, workspaces, and operational metrics. Session/run event
streams use SSE with replay cursors, a gateway epoch, retained-floor signaling,
and authoritative reconciliation. [`api.md`](api.md) is the endpoint and wire
contract; this decision record intentionally does not duplicate its route list.

### Streaming events
The implemented wire contract uses events such as `token_delta`, `reasoning_delta`,
`tool_start`, `tool_end`, `subagent_started`, `subagent_activity`,
`subagent_completed`, `job_completed`, and terminal run events. Exact names and
payloads are defined in [`api.md`](api.md) and the SSE golden tests.

---

## 9) Browser UI stack (TypeScript)

### Why TS here
- rapid iteration
- great ecosystem for UI
- a clean base for a future SDK if one is needed

### Implemented choice

- Preact with strict TypeScript and `@preact/signals`
- Vite for deterministic production builds
- Zod at HTTP/SSE boundaries
- normalized entity state with revision/cursor guards
- Vitest and Testing Library for unit/component coverage
- Playwright for browser convergence flows
- generated assets committed and embedded in the Rust binary

A desktop shell or public SDK should be selected only when there is a concrete
distribution requirement; neither is part of the current stack.

---

## 10) LLM providers

### Implemented strategy

One internal client contract dispatches to native Anthropic and Gemini
adapters plus a configurable OpenAI-compatible adapter. Provider entries cover
OpenAI, OpenRouter, local OpenAI-compatible servers, and other compatible
vendors without adding a new backend service.

Requirements:
- consistent tool-call schema
- consistent streaming semantics
- usage accounting

---

## 11) Hybrid: when it makes sense (and when it doesn’t)

### Implemented hybrid
- **Rust daemon and CLI** for correctness- and security-sensitive behavior
- **TypeScript** for the embedded browser UI

A desktop shell or public SDK can reuse the HTTP/SSE boundary when a concrete
distribution requirement exists. Replacing the working Rust CLI with another
language is not a current recommendation.

### Hybrid that is usually a mistake (early)
Splitting core backend logic across languages (e.g., Go services + Rust sandbox) before the single-process architecture is stable:
- doubles your models (capabilities/tools/messages)
- increases drift
- makes security policy enforcement harder

---

## 12) Non-negotiables and their status

1) **SQLite/real DB for state** — implemented
2) **single capability model** — target; not yet implemented
3) **one production tool registry** — implemented
4) **auditable tool execution** — implemented for current native paths; keep complete as tools expand
5) **backpressure + bounded queues** — implemented on stabilized admission/trigger paths; preserve this invariant

---

## 13) Established decisions

The daemon/CLI/UI shape, acyclic crate graph, native tool path, SQLite storage
and migrations, scheduled jobs, bounded admission, lifecycle state machines,
SSE recovery, and typed frontend boundary are implemented. New infrastructure
should be introduced only for an observed operating requirement, not as a
replacement for these stabilized foundations.

---

## Appendix: Crate-boundary rule

Keep shared protocol and lifecycle types in `alms-core`, persistence in
`alms-session`, native execution in `alms-sandbox`, agent-facing coordination
tools in `alms-tools`, model/context execution in `alms-runtime`, orchestration
in `alms-coordinator`, transport in `alms-gateway`, and the CLI thin. Rust's
workspace resolver rejects cycles; review additionally guards against new
upward dependencies that would blur these ownership boundaries.

---

## 14) Resource footprint: ALMS vs Node.js alternatives

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

*Authored by Mesut (2026-02-10). Updated on 2026-08-01 to distinguish implemented state from target requirements.*
*§14 added by Tesla (2026-03-15).*
