# ALMS Tasks / TODO (triaged)

This is a running task list so agents (Mesut/Atlas/Mustafa) can coordinate.

## P0 — Blocking (make it build + run end-to-end)

1) Break the **Cargo dependency cycle** ✅
- Fixed: moved MainAgent into coordinator and removed runtime↔coordinator cycle.

2) Fix `alms-gateway` compile/run wiring ✅
- Fixed: `serve(bind)` signature, corrected AgentRuntime construction, HTTP server now owns gateway lifecycle.

3) Ensure the canonical repo is a **git repo** ✅
- Initialized git in `</srv/alms` and committed initial import.

## P1 — Architecture correctness / avoid OpenClaw pitfalls

4) Unify capability + tool model
- ✅ Capabilities unified to `alms-core::Capability` in coordinator.
- Tools: there are currently two parallel tool registries (`alms-runtime::tools` vs `alms-sandbox::registry`). Pick one and wire runtime → sandbox if sandboxing is the goal.

5) Session storage strategy ✅
- Decision captured in `docs/session-storage.md` (MVP: in-memory + snapshots; defer append-only/SQLite choice).

6) Tool sandbox ABI ✅
- MVP spec captured in `docs/tool-sandbox-abi.md`.

## P2 — Product/UX

7) Make onboarding/docs non-drifting
- Once MVP path stabilizes, add a dedicated docs/architecture/UX “designer” agent (docs-only).

8) Observability
- Structured tracing, per-session run IDs, subagent lifecycle events.

---

See also:
- `docs/mesut-verdict-2026-02-10.md`
- `docs/proposal.md`
- `docs/tech-stack.md`
- `docs/security-model.md`
- `docs/testing-strategy.md`
- `docs/mvp-plan.md`
