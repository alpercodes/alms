# ALMS Docs Index

This is the entrypoint to ALMS documentation.

## Start here (MVP)

1) **MVP plan** — what we’re building *now*
- `mvp-plan.md`

2) **API contract** — how clients/UI/SDK talk to ALMS
- `api.md`

3) **Events & audit** — the behavioral spine (streams, approvals, invariants)
- `events-and-audit.md`

4) **Security model** — capabilities, approvals, guardrails
- `security-model.md`

5) **Capability model** — single source of truth for permissions + scopes
- `capability-model.md`

6) **Policy reasons** — stable reason codes for policy_decision + approvals
- `policy-reasons.md`

7) **Approvals UX** — what the user sees and how approvals behave
- `approvals-ux.md`

## Architecture (target state)

- `tech-stack.md` — recommended stack + rationale (target architecture)
- `architecture.md` — system architecture overview (coordinator/multi-agent)

## Storage & tooling specs

- `session-storage.md` — MVP decision: snapshot storage requirements + migration triggers
- `tool-sandbox-abi.md` — WASM tool ABI (instance-per-call MVP)

## Testing & developer workflow

- `testing-strategy.md` — test layers, deterministic time, golden tests
- `dev-onboarding.md` — how to build/run/test, how to add tools/channels
- `TASKS.md` — current task list / ownership
- `proposal.md` — consolidated proposal + key findings (historical but useful)

## Notes

- If `tech-stack.md` and `mvp-plan.md` appear to conflict: follow `mvp-plan.md` for execution; `tech-stack.md` is target-state.

---

*Maintained by Mesut.*
