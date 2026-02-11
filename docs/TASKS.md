# ALMS Tasks / TODO (triaged)

This is a running task list so agents (Mesut/Atlas/Mustafa) can coordinate.

**Note:** Previously-blocking wiring tasks were completed and removed from this list to keep it actionable.

## P0 — MVP foundation (end-to-end + safety)

1) Atomic snapshot persistence + rotation + corruption fallback
- Implement the requirements in `docs/session-storage.md` (temp write + fsync + rename + fsync dir, keep last N, checksum/version).
- Add tests for restart survival + corrupted snapshot fallback.
- **Owners:** Atlas

2) Implement Tool Sandbox ABI v0 in code
- Implement the host↔WASM contract described in `docs/tool-sandbox-abi.md`.
- Explicit MVP posture: **instance-per-call**.
- Enforce max input/output byte limits.
- Add a minimal sample WASM tool + golden tests.
- **Owners:** Atlas, Mustafa

3) Capability enforcement + audit trail (minimal)
- Ensure every tool invocation goes through a single policy gate and produces an audit event.
- Implement minimal audit event schema end-to-end.
- **Owners:** Atlas

4) SSE-first streaming endpoint (minimal)
- Provide `POST /agent/run/stream` using SSE with correlation IDs (`session_id`, `run_id`, `tool_invocation_id`).
- Keep WebSocket as optional later.
- Add golden tests for event sequencing using a mocked LLM.
- **Owners:** Mustafa

5) Deterministic test harness for scheduler + timeouts
- Apply `docs/testing-strategy.md`: tokio paused time, mock LLM adapter, in-memory SQLite if/when introduced.
- Add at least one scheduler test: schedule → advance time → job run recorded.
- **Owners:** Atlas

6) Docs: API contract for MVP
- Write `docs/api.md` describing the MVP HTTP API:
  - endpoints, request/response JSON, error format
  - SSE event types and payloads
  - correlation IDs and retry/reconnect behavior
- **Owners:** Mesut

7) Docs: Event model + audit log schema
- Write `docs/events-and-audit.md`:
  - canonical event types (tool_start/end, token_delta, job_run_start/end, etc.)
  - minimal audit record fields aligned with `security-model.md`
- **Owners:** Mesut

8) Docs: Approval UX spec (minimal)
- Write `docs/approvals-ux.md`:
  - what is shown to the user
  - allow-once vs allow-for-session vs rule
  - how approvals map to capabilities/scopes
- **Owners:** Mesut

## P1 — Stability & cleanup

9) Remove dead code / fix warnings / tighten interfaces
- Unused imports, unused fields, ensure public APIs are minimal and coherent.
- Keep the runtime/tool interface single-path (no duplicate registries).
- **Owners:** Mustafa

10) Decide and document “MVP module vs crate” structure ✅
- Decision captured in `docs/mvp-structure.md` and linked from `docs/mvp-plan.md`.
- **Owners:** Atlas

11) Docs: Update tech-stack vs MVP-plan alignment notes
- Ensure `docs/mvp-plan.md` and `docs/tech-stack.md` don’t contradict each other.
- Add a short note in `docs/mvp-plan.md` if needed: “tech-stack is target state; MVP plan is execution plan”.
- **Owners:** Mesut

## P2 — Product / docs

12) Make onboarding/docs non-drifting
- Once MVP path stabilizes, add a dedicated docs/architecture/UX “designer” agent (docs-only).
- **Owners:** Atlas

13) Observability
- Structured tracing, per-session run IDs, subagent lifecycle events.
- **Owners:** Mustafa

14) Docs: Developer onboarding
- Write `docs/dev-onboarding.md`:
  - how to run daemon
  - how to run tests
  - how to add a tool
  - how to add a channel adapter
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
