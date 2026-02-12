# ALMS Tasks / TODO (triaged)

This is the running task list for ALMS. Keep it short, current, and merge-friendly.

## Status snapshot
- **Docs spine is in place** (`docs/index.md`, `api.md`, `events-and-audit.md`, `security-model.md`, `capability-model.md`, `approvals-ux.md`, `policy-reasons.md`, `artifacts.md`).
- Core engineering work is now about **making runs/approvals/audit/event persistence real and coherent**.

---

## P0 — MVP foundation (must work end-to-end)

1) Snapshot persistence: atomic + rotation + checksum + fallback ✅
- Implemented in `crates/alms-session/src/store.rs`.
- **Owners:** Atlas

2) Tool Sandbox ABI v0 in code ✅
- Allocator (`alms_alloc`), size limits, ABI envelope (`abi:0`), tests.
- **Owners:** Atlas, Mustafa

3) Minimal audit trail for tools + policy gate ✅ *(pending merge if PR not yet merged)*
- `feature/atlas-audit` implements minimal audit events + deny unknown tools.
- After merge, ensure audit records include `run_id` when available.
- **Owners:** Atlas

4) SSE streaming for runs ✅
- SSE endpoint exists + golden tests.
- **Owners:** Mustafa

5) Deterministic test harness for scheduler + timeouts
- Use paused tokio time; at least one scheduler test: schedule → advance time → job run recorded.
- **Owners:** Atlas

---

## P1 — Converge on the Run/Event/Approval model (ALMS identity)

6) Canonical Run API (introduce without breaking MVP compatibility)
- Implement `POST /runs` + `GET /runs/{run_id}/events` as canonical.
- Keep `/agent/run` + `/agent/run/stream` as compatibility aliases (deprecated).
- Ensure event invariants in `docs/events-and-audit.md` hold.
- **Owners:** Atlas, Mustafa

7) Approvals end-to-end (guarded posture)
- Implement `approval_required` → pause → `approval_resolved` → continue.
- Minimal `/approvals` endpoints (list pending, resolve approve/deny).
- Guarantee `full_control` posture never emits approvals.
- **Owners:** Atlas, Mustafa

8) Event persistence stance (required if approvals ship)
- Decide and implement: best-effort streaming vs persisted per-run event log.
- Recommendation: persist per-run events if approvals exist (reconnect/replay).
- **Owners:** Atlas

9) Audit surfacing (minimal)
- Add minimal query path for audit per session/run (even if in-memory for MVP).
- Redaction/truncation rules aligned with `docs/security-model.md`.
- **Owners:** Atlas

10) Tool parameter schemas (tool-call reliability)
- Fix tool calling reliability by providing real JSON Schemas for tool parameters.
- Today, LLM tool definitions may be missing/empty schemas → unreliable tool calls.
- Recommendation:
  - add `parameters() -> JSON Schema` to the tool trait
  - implement schemas for built-ins (echo/math/http_get)
  - ensure runtime uses these schemas when creating LLM tool definitions
- **Owners:** Zeki (approach), Atlas/Mustafa (implementation)

---

## P2 — Stability / quality

10) CI basics
- `cargo fmt`, `cargo clippy`, `cargo test` (including golden tests) in CI.
- **Owners:** Mustafa

11) Documentation drift checks
- Keep `docs/api.md`, `docs/events-and-audit.md`, and implementation aligned.
- Add a “docs index” link in README (optional).
- **Owners:** Mesut

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

Execution plan:
- `docs/mvp-plan.md`
- `docs/mvp-module-crate-structure.md`
- `docs/testing-strategy.md`
