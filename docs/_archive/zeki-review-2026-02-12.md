# Zeki Review — ALMS State of the Project (2026-02-12)

Fresh-eyes review of the entire ALMS codebase + docs. Reviewed from `main` at `0b0806a` (which includes all fix commits up to `feature/mesut-tasks-refresh` at `1019c5c`).

## 1) What exists today

### Code (~6,000 lines Rust, 8 crates)

| Crate | Lines | Verdict | Notes |
|-------|-------|---------|-------|
| alms-core | ~580 | ✅ Solid | Clean ID types, Capability enum, AuditEvent, Channel trait, error types |
| alms-session | ~510 | ✅ MVP-ready | In-memory store + JSON snapshot persistence, get/create/history |
| alms-runtime | ~800 | ✅ Real | Tool-call loop against OpenAI-compatible API, mock mode, audit on every tool call |
| alms-sandbox | ~1,400 | ⚠️ Mixed | Registry (DashMap + Arc + async) is solid, builtins work (echo/math/http_get), WASM execution is prototype-level |
| alms-gateway | ~930 | ✅ Good | Axum HTTP server, SSE with event log + reconnect (Last-Event-ID), run lifecycle, legacy + new endpoints |
| alms-channel | ~500 | ✅ Basic | Telegram polling adapter, command parsing |
| alms-coordinator | ~640 | 🔲 Scaffold | Simulated subagent execution only — not wired to real agent loops |
| alms-cli | ~46 | ✅ Fine | Thin wrapper, correct serve() wiring |

### Docs (~15 documents)

Quality is high across the board. Not aspirational filler — these are specific, opinionated, and implementable:

- **`proposal.md`** — Mesut's consolidated repo review + direction. Excellent single-source summary.
- **`api.md`** — Full REST/SSE API contract with sessions, runs, events, approvals, jobs, reconnect semantics.
- **`security-model.md`** — Threat model, capability system, approval modes, shell sandboxing roadmap, audit requirements.
- **`events-and-audit.md`** — Event taxonomy, state machine (queued→running→waiting_for_approval→succeeded/failed), invariants, audit record schema.
- **`testing-strategy.md`** — Deterministic time (tokio pause), mock LLM, SQLite test harness, golden tests.
- **`architecture.md`** — Hub-and-spoke multi-agent design, message flow, component responsibilities.
- **`tech-stack.md`** — Rust daemon + SQLite + WASM + SSE + TS UI. Well-reasoned.
- **`session-storage.md`** — Snapshot persistence spec (atomic write, rotation, checksums).
- **`tool-sandbox-abi.md`** — WASM ABI contract (allocation, envelopes, size limits).
- **`research/session-issues.md`** — 20-weakness teardown of OpenClaw. Surgical and specific.

### Infrastructure

- Git branch workflow with pre-commit hook blocking direct main commits
- CI: fmt + clippy + test (`.github/workflows/ci.yml`)
- Makefile, CONTRIBUTING.md, nightly toolchain pinned via `rust-toolchain.toml`
- Multiple feature branches with merge-based flow

## 2) What's working

The **single-agent HTTP pipeline** is architecturally complete:

```
HTTP POST /runs → create Run → spawn background task →
  LlmClient.complete() (mock or real) → tool-call loop →
    policy gate (deny unknown tools) → execute via sandbox registry → audit event →
  SSE event stream (run_started → token_delta → run_finished) →
    event log persistence → Last-Event-ID reconnect support
```

Gateway startup is coherent: `serve()` creates Gateway, initializes channels, spawns Telegram polling loop, runs axum. One entrypoint, no confusion.

Previous wiring issues (circular deps, signature mismatches, stale imports) were all fixed by Atlas in a series of commits between `ab8b5b3` and `0b0806a`.

## 3) What's not working / not real

### 3a) Build environment
- **Cannot compile on this machine.** The VPS has 4GB RAM / 2 cores / no swap. Wasmtime (cranelift backend) OOM-kills during compilation. Need either a beefier build machine, cross-compilation, or adding swap.
- **Implication:** I cannot verify whether the code compiles or tests pass. CI should be the source of truth here.

### 3b) Coordinator / multi-agent (the differentiator)
- The coordinator crate simulates subagent work with fake progress/results.
- No real subagent agent loops, no message routing, no result aggregation.
- This is ALMS's main value proposition — "coordination over monolith" — and it's 0% real.

### 3c) WASM sandbox ABI
- Allocator returns hardcoded offsets (no real memory management strategy).
- Timeout is enforced via `tokio::time::timeout` wrapping the call, which is correct, but fuel metering is configured but not wired.
- Result protocol (4-byte length prefix + JSON) works for tests but isn't battle-tested.
- **However:** The native builtins (echo, math, http_get) work fine and don't go through WASM.

### 3d) Tool parameter schemas
- `runtime::ToolRegistry::to_definitions()` emits empty schemas for every tool: `{"type":"object","properties":{},"required":[]}`.
- The LLM knows tool *names* but has no idea what arguments they accept. Tool calls will be unreliable.
- **Task created:** P1.5 #15 on `feature/zeki-tool-consolidation` branch.

### 3e) Persistence
- Atomic snapshot persistence **is implemented** (merged by Atlas): temp file → fsync → rename → dir fsync, 3-rotation backup, SHA256 checksum verification, corruption fallback. Tests cover roundtrip and corrupted-file recovery.
- In-memory + snapshot is the current store. No SQLite yet — that's the next persistence step.
- Sessions should survive daemon restarts via snapshots, though this needs real-world verification.

### 3f) Approval workflow
- Fully documented (events, state machine, UX spec).
- Not implemented in code at all.

### 3g) Cron / scheduler
- Documented in API spec and tech-stack.
- Not implemented.

### 3h) Capability model duplication
- `alms-core::Capability` is an enum (Shell, Read, Write, Http, Search, CodeExecution, Custom).
- `alms-coordinator::SubagentRequest.capabilities` is `Vec<String>`.
- Minor but will drift. Flagged in P1.5 task.

## 4) Honest ratings

| Dimension | Score | Reasoning |
|-----------|-------|-----------|
| Architecture / Design | 8.5/10 | Docs are better than most shipped products. Specific, actionable, opinionated. |
| Code quality | 7/10 | Clean Rust, proper error handling, tracing, tests. Some stubs pretending to be features. |
| Completeness | 4/10 | Single-agent HTTP path ~80% there. Snapshot persistence implemented. Multi-agent 0%. No SQLite, approvals, cron, or real tool schemas. |
| Team execution | 7/10 | Clear roles, proper git workflow, fast iteration. Velocity is the main risk. |
| **Overall** | **~5/10** | Solid foundation, not yet a product. Maybe 20% toward an MVP that could replace OpenClaw. |

## 5) Critical path — what to focus on next

Ordered by "unblocks the most value":

### Tier 1 — Must happen now

**A) Make it build and run end-to-end with a real LLM.**
- Fix the build environment (swap, remote build, or conditionally disable wasmtime for dev builds).
- Connect to a real LLM provider (OpenRouter key is already on the machine).
- Send a message via HTTP, get a real response back. Prove the skeleton works.
- *This is the single most important milestone. Everything else is theoretical until this happens.*

**B) Tool parameter schemas (P1.5 #15).**
- Add `fn parameters(&self) -> Value` to the `Tool` trait.
- Implement real schemas for echo, math, http_get.
- Wire into `to_definitions()`.
- Without this, tool calls are unreliable even with a real LLM.

### Tier 2 — Shortly after

**C) ~~Atomic snapshot persistence.~~ ✅ Done.**
- Already implemented by Atlas: atomic write, rotation, checksums, corruption fallback with tests.
- Next step is verifying it works in real-world daemon restarts.

**D) SQLite storage layer.**
- Replace JSON snapshots with SQLite for sessions, messages, audit log.
- This was called out in every doc as non-negotiable ("don't repeat OpenClaw's file-lock mistakes").
- Use `rusqlite` or `sqlx`. Migrations in-repo.

**E) Capability enforcement end-to-end.**
- Right now: unknown tools are denied, known tools are allowed. That's a flat allowlist, not a capability system.
- Wire the `Capability` enum into actual policy decisions. A tool should declare what capabilities it requires, and the runtime should check grants.

### Tier 3 — MVP completion

**F) Coordinator / subagents (real).**
- This is the project's reason to exist. But it depends on A-E being solid first.
- Start simple: main agent spawns one subagent for a task, gets real result back.

**G) Cron / scheduler.**
- Persistent schedules, job runner, audit trail.
- Depends on SQLite (D).

**H) Basic approval workflow.**
- At minimum: `shell_exec` outside workspace triggers approval. Can be CLI-only for MVP.

### Not now

- Multi-channel (Discord, WhatsApp) — Telegram polling is fine for MVP
- WASM plugin ecosystem — native tools are fine for now
- UI — API-first, UI later
- Microservice split — stay single-process

## 6) Suggestions for the team

- **Atlas + Mustafa:** Focus on Tier 1A. Make the daemon start up, accept an HTTP request, call a real LLM, return a response. Everything else is secondary.
- **Zeki (me):** I'll own the tool parameter schema work (P1.5 #15) and can help with architecture decisions / code review.
- **Mesut:** Docs are in great shape. Hold off on new docs until code catches up. Could help with testing strategy / golden test fixtures.
- **Build environment:** Seriously consider adding 2GB swap to this VPS, or set up a build-only machine. Can't iterate if you can't compile.

## 7) Bottom line

ALMS has the right foundation. The research is real, the design is sound, the code that exists is clean. The team is functioning well with clear roles. The main risk is the gap between documentation and running software — which is large but closable.

Priority zero: **make it run.**

---

*Reviewed by Zeki (2026-02-12). Branch: `feature/zeki-tool-consolidation`.*
