# ALMS Tasks / TODO (triaged)

This is the running task list for ALMS. Keep it short, current, and merge-friendly.

## Status snapshot
- **Docs spine is in place** (`docs/index.md`, `api.md`, `events-and-audit.md`, `security-model.md`, `capability-model.md`, `approvals-ux.md`, `policy-reasons.md`, `artifacts.md`, plus Zeki's review).
- Core engineering work is now about **making runs/approvals/audit/event persistence real and coherent**.
- **2026-02-14:** Tool parameter schemas implemented, OpenAI API format fixed, LlmMessage.content nullability fixed, multiple compilation fixes applied, GatewayConfig env loading fixed, CLI health command functional. See `docs/agent-ux-requirements.md` for new UX requirements from Alper.

---

## P0 — Make it run (unblock reality)

1) Build environment: make the project compile reliably on the VPS
- Current risk (per Zeki): 4GB RAM / no swap → OOM during wasmtime/cranelift builds.
- Options: add swap, use a beefier build machine, or dev-toggle wasmtime.
- **Owners:** Mustafa (infra), Atlas

2) Real LLM end-to-end smoke
- Prove: `POST /runs` → real OpenRouter call → streamed tokens back.
- Capture a repeatable smoke script in repo (`scripts/smoke.sh`) or Makefile target.
- **Owners:** Atlas, Mustafa

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

7) Deterministic test harness for scheduler + timeouts
- Use paused tokio time; at least one scheduler test: schedule → advance time → job run recorded.
- **Owners:** Atlas

---

## P2 — Converge on the Run/Event/Approval model (ALMS identity)

8) Canonical Run API (introduce without breaking MVP compatibility)
- Implement `POST /runs` + `GET /runs/{run_id}/events` as canonical.
- Keep `/agent/run` + `/agent/run/stream` as compatibility aliases (deprecated).
- Ensure event invariants in `docs/events-and-audit.md` hold.
- **Owners:** Atlas, Mustafa

9) Approvals end-to-end (guarded posture)
- Implement `approval_required` → pause → `approval_resolved` → continue.
- Minimal `/approvals` endpoints (list pending, resolve approve/deny).
- Guarantee `full_control` posture never emits approvals.
- **Owners:** Atlas, Mustafa

10) Event persistence stance (required if approvals ship)
- Decide and implement: best-effort streaming vs persisted per-run event log.
- Recommendation: persist per-run events if approvals exist (reconnect/replay).
- **Owners:** Atlas

11) Audit surfacing (minimal)
- Add minimal query path for audit per session/run (even if in-memory for MVP).
- Redaction/truncation rules aligned with `docs/security-model.md`.
- **Owners:** Atlas

12) Tool parameter schemas (tool-call reliability) ✅
- `fn parameters(&self) -> Value` added to `Tool` trait with default empty schema.
- Real JSON Schemas implemented for echo, math, http_get builtins.
- Runtime `to_definitions()` wired to use `tool.parameters()`.
- OpenAI API format fixed: `{"type":"function","function":{...}}`.
- **Owners:** Zeki (approach), Atlas/Mustafa (implementation)

---

## P3 — Persistence upgrade (post-MVP foundation, but should be planned now)

13) SQLite storage layer (sessions/messages/audit)
- Replace (or complement) JSON snapshots with SQLite as source of truth.
- Migrations in-repo.
- **Owners:** Atlas

---

## P4 — Stability / quality

14) CI basics
- `cargo fmt`, `cargo clippy`, `cargo test` (including golden tests) in CI.
- **Owners:** Mustafa

15) Documentation drift checks
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
- `docs/zeki-review-2026-02-12.md`

UX requirements:
- `docs/agent-ux-requirements.md`

Execution plan:
- `docs/mvp-plan.md`
- `docs/mvp-module-crate-structure.md`
- `docs/testing-strategy.md`
