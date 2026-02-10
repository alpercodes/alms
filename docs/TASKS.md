# ALMS Tasks / TODO (triaged)

This is a running task list so agents (Mesut/Atlas/Mustafa) can coordinate.

## P0 — Blocking (make it build + run end-to-end)

1) Break the **Cargo dependency cycle**
- Problem: `alms-runtime` <-> `alms-coordinator` cycle.
- Fix: move shared message types + traits into `alms-core` or a new `alms-protocol` crate; make dependencies one-way.

2) Fix `alms-gateway` compile/run wiring
- `crates/alms-cli/src/main.rs` calls `alms_gateway::serve(&bind)` but `server::serve` currently requires `(bind, gateway: Gateway)`.
- `crates/alms-gateway/src/server.rs` `run_agent` calls `AgentRuntime::new(..., SessionManager)` where `LlmClient` is required.
- Pick an ownership model:
  - HTTP server owns gateway and spawns `Gateway::run()` in background; OR
  - Gateway owns the HTTP server.
- Ensure one coherent startup path.

3) Ensure the canonical repo is a **git repo**
- `</srv/alms` currently has no `.git` directory.
- Decide canonical upstream and how branches/PRs are done.

## P1 — Architecture correctness / avoid OpenClaw pitfalls

4) Unify capability + tool model
- Capabilities: currently both `alms-core::Capability` (enum) and `Vec<String>` capabilities in coordinator requests.
- Tools: there are currently two parallel tool registries (`alms-runtime::tools` vs `alms-sandbox::registry`). Pick one and wire runtime → sandbox if sandboxing is the goal.

5) Session storage strategy
- MVP: in-memory + snapshot.
- Decide append-only log + snapshots vs sqlite vs something else; align with the OpenClaw session-issues research.

6) Tool sandbox ABI
- Define stable ABI: how params/results are passed, allocation strategy, bounds checking.

## P2 — Product/UX

7) Make onboarding/docs non-drifting
- Once MVP path stabilizes, add a dedicated docs/architecture/UX “designer” agent (docs-only).

8) Observability
- Structured tracing, per-session run IDs, subagent lifecycle events.

---

See also: `docs/mesut-verdict-2026-02-10.md`
