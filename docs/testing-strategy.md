# ALMS Testing Strategy

This document defines how ALMS should be tested so we can confidently ship an agent loop system that is correct under concurrency, safe around tools, and stable over time.

Goals:
- Make concurrency + scheduling behavior **deterministic and testable**
- Make LLM behavior **mockable**
- Make storage (SQLite) **repeatable** and fast for tests
- Make “tool access” **auditable** and verifiable (policy enforcement)

---

## 1) Testing layers

### 1.1 Unit tests (fast, deterministic)
Focus: pure logic and invariants.

Targets:
- capability checks (allow/deny/approval-required)
- scope matching (paths, domains, argv allow/deny)
- tool argument validation
- event encoding/decoding
- cron parsing + next-run computation

Rules:
- no network
- no real time
- no filesystem side effects (use temp dirs if necessary)

### 1.2 Component tests (in-process integration)
Focus: components working together with mocks.

Targets:
- runtime tool-call loop with mocked LLM adapter
- session persistence to SQLite (in-memory or temp-file)
- scheduler running a job and recording job_run + audit events
- tool execution path using a “fake tool” that returns deterministic output

### 1.3 Browser and end-to-end tests
Focus: user-visible state convergence and, where a real daemon is involved,
the complete HTTP/SSE path.

Targets:
- start daemon
- create session
- run agent
- stream response events (SSE)
- run a cronjob
- validate audit log entries

The current Playwright suite is a deterministic browser suite backed by route
fixtures. It covers dashboard boot, agent-switch isolation, replay-gap
convergence, and delayed optimistic-send failure. It does not start the Rust
daemon. Gateway integration and SSE golden tests cover the backend in process;
a small real-daemon HTTP/SSE suite remains a useful future addition and must
not be implied by the current Playwright label.

---

## 2) Deterministic time (critical)

ALMS has scheduling, timeouts, retries, and backoff. Tests must not sleep in real time.

Recommended approach:
- Use `tokio::time::pause()` and `tokio::time::advance()` in tests.
- Introduce an internal `Clock` abstraction for places where wall-clock is used.

Example (conceptual):
- `trait Clock { fn now(&self) -> Instant/Timestamp }`
- prod: `SystemClock`
- tests: `MockClock`

Even if you don’t implement a clock trait on day 1, **use tokio time control** for scheduler/tool timeouts.

---

## 3) Mockable LLM adapter (non-negotiable)

Do not bind core logic directly to reqwest/OpenRouter.

Define a narrow interface:
- `complete(messages, tools, params) -> Completion`
- `stream(messages, tools, params) -> Stream<Delta>`

Test strategies:
- “scripted model”: pre-programmed responses (including tool_calls)
- “echo model”: returns the user input
- “chaos model”: injects malformed tool args to ensure validation is robust

Tests to include:
- tool call emitted → tool executed → tool result fed back → final assistant message
- invalid JSON args in tool_call → graceful error path
- multiple tool calls in one turn

---

## 4) SQLite test harness

### Recommended
- Use in-memory SQLite for most tests (`:memory:`) OR
- Use temp-file DB for tests that require multiple connections

Always:
- run migrations at test setup
- wrap each test in a transaction and rollback if feasible

Things to test:
- session creation is idempotent for same context
- message append ordering
- audit log append-only semantics
- job + job_run recording

---

## 5) Tool execution tests (policy + audit)

Tools are the danger zone. Tests must prove:
- capability checks are enforced
- outputs are truncated/sanitized
- audit log entries are emitted even on failure/timeout

Suggested test tools:
- `FakeTool` that returns deterministic JSON
- `SlowTool` that times out
- `LargeOutputTool` that exceeds output limit
- `ShellTool` in “dry-run mode” for deterministic behavior

Key assertions:
- denied tool call never executes
- approved tool call executes and is logged
- failure paths still write audit entries with error status

---

## 6) Scheduler tests

Test with paused time:
- schedule a job for T+N
- advance time
- verify job run started
- verify job_run record + audit events
- verify retries/backoff logic deterministically

Also test:
- concurrency limits (jobs don’t exceed max parallelism)
- cancellation (disable job stops future runs)

---

## 7) Streaming tests (SSE-first)

If SSE is used:
- test event framing + reconnect token/last-event-id handling
- test correlation IDs (`session_id`, `run_id`, `tool_invocation_id`, `job_run_id`)

Golden tests:
- given deterministic scripted model/tool outputs, the exact event sequence matches a snapshot.

---

## 8) CI pipeline

GitHub CI runs three parallel jobs:

1. **Frontend:** install the pinned Node/npm toolchain with `npm ci`, run the
   high-severity dependency audit, typecheck, lint, format-check, run Vitest,
   reproduce the committed Vite bundle, and run the Chromium Playwright suite.
2. **Rust:** `cargo fmt --all -- --check`, Clippy for all targets/features with
   warnings denied, `cargo test --all`, and `cargo build --release`.
3. **Security audit:** check `Cargo.lock` against the RustSec advisory database.

`make ci` reproduces the frontend type/lint/format/unit/build checks plus the
Rust job. The dependency audit and Playwright suite remain explicit GitHub
frontend-job gates; Playwright is available locally through
`make frontend-test-e2e`.

---

## 9) Change acceptance criteria

Every behavioral fix must add a deterministic regression at the narrowest
authoritative boundary. Concurrency fixes pin ordering with barriers, paused
time, or injected failures instead of probabilistic sleeps. Persistence fixes
cover both the failure transaction and restart-visible state. Frontend state
fixes cover reducer behavior and the relevant browser convergence flow.

Mechanical decomposition changes must keep the full CI matrix green, preserve
the normal crate-dependency edge set, and introduce no new lifecycle or
persistence bypass.

---

*Authored by Mesut (2026-02-10); CI and coverage status updated 2026-08-01.*
