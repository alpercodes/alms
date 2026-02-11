# ALMS Proposal (Mesut)

This file is an opinionated, consolidated proposal for what ALMS should become, based on:
- the current repository state in `</srv/alms`
- the existing architecture/research docs
- my one-by-one code review + sanity checks

It is intentionally redundant: it’s meant to be a single place other agents can read to understand the direction and the concrete problems to fix.

---

## 1) Product definition (what ALMS is)

ALMS is a **local-first Agent Loop Management System** — effectively an **agent operating system**:
- users create agents with configurable autonomy and tool access
- agents can spawn subagents (specialists) and aggregate results
- agents can schedule work via cronjobs (persistent autonomy)
- tools can include host access (terminal/files/network) under explicit policy
- everything is observable and auditable

**Goal:** go far beyond OpenClaw’s “glimpse”: deliver a fully designed system that is safe-by-default, reliable under concurrency, and pleasant to use.

---

## 2) Tech stack recommendation (canonical direction)

**Rust core daemon + SQLite durable state + WASM tool sandbox + HTTP/WebSocket API + TypeScript UI/SDK**

Rationale:
- ALMS’s hardest problems are concurrency, cancellation, resource limits, isolation, and durable state.
- Rust fits the daemon; SQLite prevents file-lock races; TS fits UI/SDK velocity.

Reference doc:
- `docs/tech-stack.md`

Security model doc:
- `docs/security-model.md`

---

## 3) Non-negotiables (to beat OpenClaw)

1) **Durable state must not be “JSON + file locks”**
   - Use SQLite (or equivalent transactional DB).

2) **Single capability model across the entire system**
   - No parallel `enum Capability` vs `Vec<String>` drift.

3) **One tool registry / one tool-call path**
   - Avoid `alms-runtime` tools duplicating `alms-sandbox` tools.

4) **Auditable execution**
   - Every tool call, job run, and escalation/approval must be recorded append-only.

5) **Backpressure / bounded queues**
   - No unbounded in-memory queues for message lanes, jobs, or tool invocations.

---

## 4) One-by-one findings (repo reality check)

### 4.1 Repo basics
- Canonical workspace reviewed: `</srv/alms`
- Note: This directory currently has **no `.git`** (not a git repository). If this is a mirror/export, decide the upstream repo location and workflow.

### 4.2 Environment sanity check
- On this machine/session, `cargo` was not available (`cargo: command not found`).
- Therefore most findings are from static inspection.

### 4.3 alms-gateway
Files:
- `crates/alms-gateway/src/gateway.rs`
- `crates/alms-gateway/src/server.rs`
- `crates/alms-gateway/src/lib.rs`

Findings:
- Two “gateway” roles are present but not coherently integrated:
  1) Channel router loop (`Gateway::run` polling Telegram)
  2) HTTP API server (`server.rs`)

Likely build blockers:
- CLI calls `alms_gateway::serve(&bind)` but `server::serve` currently requires `(bind, gateway: Gateway)`.
- `server.rs::run_agent` constructs `AgentRuntime::new(..., SessionManager)` where an `LlmClient` is expected.

Proposal:
- Pick one coherent startup ownership model:
  - HTTP server owns `Gateway` and spawns channel loop in background; OR
  - `Gateway` owns the HTTP server.

### 4.4 alms-cli
File:
- `crates/alms-cli/src/main.rs`

Finding:
- Calls `alms_gateway::serve(&bind)` which doesn’t match current `serve` signature.

### 4.5 alms-runtime
Files:
- `crates/alms-runtime/src/agent.rs` (tool-call loop)
- `crates/alms-runtime/src/tools.rs` (native tool registry)
- `crates/alms-runtime/src/main_agent.rs` (orchestration sketch)

Findings:
- Tool-call loop is a reasonable skeleton.
- There is a **separate tool system** in runtime not integrated with `alms-sandbox`.
- `main_agent.rs` depends on coordinator, contributing to the crate boundary tension.

### 4.6 alms-coordinator
File:
- `crates/alms-coordinator/src/lib.rs`

Findings:
- Coordinator/subagent execution is currently a simulation scaffold (fake progress/results).
- Capability model here is `Vec<String>`.

### 4.7 alms-sandbox
Files:
- `crates/alms-sandbox/src/sandbox.rs` etc.

Findings:
- Tool registry (DashMap + Arc) is solid.
- Built-in tools are more complete than runtime’s.
- Sandbox execution is still **prototype-level**:
  - allocation returns ptr=0 (no real allocation strategy)
  - assumes a result memory protocol without enforcing it
  - timeout is checked post-hoc

### 4.8 alms-session
Files:
- `crates/alms-session/src/lib.rs`, `types.rs`, `store.rs`

Findings:
- In-memory session manager + snapshot store is OK for MVP.
- `SessionManager::get(session_id)` currently scans all sessions (inefficient but acceptable early).

### 4.9 Capability & tool duplication (cross-cutting)
- Capabilities: `alms-core::Capability` enum vs coordinator’s `Vec<String>`.
- Tools: runtime tool registry vs sandbox tool registry.

This will drift unless unified early.

---

## 5) Proposed architecture boundary cleanup (to avoid cycles)

A split that typically works:
- `alms-core`: protocol types (minimal deps)
- `alms-storage`: SQLite + migrations
- `alms-tools`: capabilities + tool registry + host execution policy
- `alms-runtime`: LLM adapters + tool-call loop (depends on tools)
- `alms-coordinator`: multi-agent orchestration (depends on runtime + storage)
- `alms-channel`: channel adapters
- `alms-gateway`: API server + channel wiring
- `alms-cli`: wrapper

Key rule: avoid `runtime <-> coordinator` cycles.

---

## 6) Security model (summary)

See `docs/security-model.md` for full details.

Short version:
- every tool invocation is capability-checked and auditable
- risky actions require approval (policy-driven)
- cronjobs run as a principal `job:<id>` with scoped capabilities
- shell tool should use argv arrays, strict timeouts, output caps, and workspace restrictions

---

## 7) Task breakdown (who does what)

See `docs/TASKS.md`.

The key near-term objective is to make ALMS **buildable and runnable end-to-end** with a coherent startup path, while locking in the single capability/tool model direction.

---

## 8) Suggested doc improvements (next edits)

`docs/tech-stack.md` is strong, but should be tightened with:
- explicit agent/subagent lifecycle semantics
- clearer separation of “WASM plugin sandbox” vs “host-privileged tools”
- explicit networking posture (default deny, allowlists)
- observability/event stream model (correlation IDs)
- cross-link to `docs/security-model.md`

---

*Authored by Mesut (2026-02-10).*
