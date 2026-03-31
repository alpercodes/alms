# MVP Module vs Crate Structure (Decision)

**Status:** Revised for MVP

## Goal
Ship an end‑to‑end daemon **without destabilizing the codebase** or creating new dependency cycles. Keep the graph small *in practice*, even if crates remain.

## Decision (MVP)
**Do not introduce new crates or a large restructure during MVP.**
Instead:
- Keep existing crates, but **treat them as thin facades** around internal modules.
- Avoid adding new cross‑crate dependencies.
- Use `alms-core` as the only shared dependency between “verticals” (gateway/runtime/session/sandbox/channel).

### Dependency policy (MVP)
Make the dependency graph explicit and keep it acyclic:
- Allowed: `alms-gateway` → `alms-runtime`, `alms-session`, `alms-sandbox`, `alms-core`
- Allowed: `alms-runtime` → `alms-session`, `alms-sandbox`, `alms-core`
- Allowed: `alms-session` → `alms-core`
- Allowed: `alms-sandbox` → `alms-core`
- Disallowed: any reverse edges (e.g., `alms-core` depending on other crates)
- Disallowed: introducing new crate cycles

### Practical boundaries (MVP)
- `alms-core` — shared types/protocol/errors (stable)
- `alms-gateway` — HTTP server + request handling
- `alms-runtime` — agent loop + LLM client
- `alms-session` — session storage
- `alms-sandbox` — tool execution (single tool registry)
- `alms-cli` — thin entrypoint

This keeps the current structure **stable**, while enforcing a rule: **no new crate‑to‑crate cycles and no new crates until MVP is done.**

## Why (change from prior decision)
- The repo already has multiple crates; merging into a new `almsd` crate now adds churn and risk.
- MVP risks are wiring and correctness, not modular purity.
- We can defer the “almsd” consolidation until we have a stable end‑to‑end path.

## Post‑MVP Options
Once the MVP is stable:
1) **Consolidate into `almsd`** for simplicity, *or*
2) **Extract clean crate boundaries** for long‑term scaling.

Either is viable; choose based on team size and release cadence.

---
*Date: 2026-02-11*
