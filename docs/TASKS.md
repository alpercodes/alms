# ALMS Tasks / TODO (triaged)

This is a running task list so agents (Mesut/Atlas/Mustafa) can coordinate.

**Note:** Previously-blocking wiring tasks were completed and removed from this list to keep it actionable.

## P0 — MVP foundation (end-to-end + safety)

1) Atomic snapshot persistence + rotation + corruption fallback
- Implement the requirements in `docs/session-storage.md` (temp write + fsync + rename + fsync dir, keep last N, checksum/version).
- Add tests for restart survival + corrupted snapshot fallback.
- **Owners:** Atlas

2) Implement Tool Sandbox ABI v0 in code ✅
- Implemented host↔WASM contract per `docs/tool-sandbox-abi.md`.
- Instance-per-call enforced; size limits + allocator + tests added.
- **Owners:** Atlas, Mustafa

3) Capability enforcement + audit trail (minimal)
- Ensure every tool invocation goes through a single policy gate and produces an audit event.
- Implement minimal audit event schema end-to-end.
- **Owners:** Atlas

4) SSE-first streaming endpoint (minimal) ✅
- Provide `POST /agent/run/stream` using SSE with correlation IDs (`session_id`, `run_id`, `tool_invocation_id`).
- Keep WebSocket as optional later.
- Add golden tests for event sequencing using a mocked LLM.
- **Status:** MVP implemented. Golden tests in `crates/alms-gateway/tests/sse_golden_tests.rs`. 
- **Note:** Current endpoint is `POST /agent/run/stream` but should align with `POST /runs` + `GET /runs/{id}/events` per docs/api.md post-MVP.
- **Owners:** Mustafa

5) Deterministic test harness for scheduler + timeouts
- Apply `docs/testing-strategy.md`: tokio paused time, mock LLM adapter, in-memory SQLite if/when introduced.
- Add at least one scheduler test: schedule → advance time → job run recorded.
- **Owners:** Atlas

6) Docs: API contract for MVP ✅
- Implemented in `docs/api.md`.
- **Owners:** Mesut

7) Docs: Event model + audit log schema ✅
- Implemented in `docs/events-and-audit.md`.
- **Owners:** Mesut

8) Docs: Approval UX spec (minimal) ✅
- Implemented in `docs/approvals-ux.md`.
- **Owners:** Mesut

## P1 — Stability & cleanup

9) Remove dead code / fix warnings / tighten interfaces ✅
- Removed unused imports across coordinator, gateway, runs, sse modules.
- Removed unused ToolContext struct (53 lines) from sandbox.
- Removed unused ChannelAdapter struct (16 lines) from channel.
- Total: 77 lines of dead code removed.
- **Owners:** Mustafa

10) Decide and document “MVP module vs crate” structure ✅
- Decision captured in `docs/mvp-structure.md` and linked from `docs/mvp-plan.md`.
- **Owners:** Atlas

11) Docs: Update tech-stack vs MVP-plan alignment notes ✅
- Added an explicit alignment note to `docs/mvp-plan.md`.
- **Owners:** Mesut

## P1.5 — Tool system consolidation

15) Unify tool registry interface + add real parameter schemas
- The runtime `ToolRegistry` is already a thin wrapper around the sandbox `ToolRegistry` (good), but there are loose ends:
  a) **Parameter schemas are empty stubs.** `runtime::ToolRegistry::to_definitions()` emits `{"type":"object","properties":{},"required":[]}` for every tool. The LLM has no idea what arguments tools accept. Each `Tool` impl should expose its own JSON Schema via a `fn parameters(&self) -> Value` method on the `Tool` trait, and `to_definitions()` should use it.
  b) **Capability model duplication.** `alms-core::Capability` is an enum, but `alms-coordinator::SubagentRequest.capabilities` uses `Vec<String>`. Pick one representation and use it everywhere. Recommendation: enum in core, with `From<&str>` / `Display` for serialization boundaries.
  c) **The runtime wrapper may be unnecessary long-term.** If the only value is error mapping and LLM definition conversion, consider moving those into the sandbox crate directly (e.g. a `ToolRegistry::to_llm_definitions()` method) and having the runtime use `SandboxRegistry` directly. Less indirection.
- **Owners:** Zeki (approach), Atlas/Mustafa (implementation)

## P2 — Product / docs

12) Make onboarding/docs non-drifting
- Once MVP path stabilizes, add a dedicated docs/architecture/UX “designer” agent (docs-only).
- **Owners:** Atlas

13) Observability ✅
- Structured tracing with `#[instrument]` spans across coordinator, gateway, runtime.
- Per-session run IDs propagated through subagent requests.
- Subagent lifecycle events: spawned, started, completed, cancelled, timeout.
- Tool execution logging with duration metrics.
- Tracing targets: `coordinator::*`, `subagent::*`, `agent::*`, `agent::tool::*`.
- **Owners:** Mustafa

14) Docs: Developer onboarding ✅
- Implemented in `docs/dev-onboarding.md`.
- **Owners:** Mesut

---

See also:
- `docs/proposal.md`
- `docs/tech-stack.md`
- `docs/security-model.md`
- `docs/session-storage.md`
- `docs/tool-sandbox-abi.md`
- `docs/testing-strategy.md`
- `docs/mvp-plan.md`
