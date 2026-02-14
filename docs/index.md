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

6) **Policy reasons** — stable reason codes for policy decisions + approvals
- `policy-reasons.md`

7) **Approvals UX** — what the user sees and how approvals behave
- `approvals-ux.md`

8) **Artifacts** — large outputs, binaries, redaction, retention
- `artifacts.md`

## Architecture (target state)

- `tech-stack.md` — recommended stack + rationale (target architecture)
- `architecture.md` — system architecture overview (coordinator/multi-agent)
- `workflow-layer.md` — WorkItems/ChangeSets/PR reviews as first-class workflow layer
- `ux-principles.md` — product UX principles (Run Timeline, artifacts, diff-first)

## Testing & developer workflow

- `testing-strategy.md` — deterministic time, mock LLM, golden tests
- `mvp-module-crate-structure.md` — keep crate graph stable during MVP
- `dev-onboarding.md` — build/run/test, add tools/channels
- `TASKS.md` — current task list / ownership

## Reviews / context

- `zeki-review-2026-02-12.md` — fresh-eyes assessment + critical path
- `proposal.md` — consolidated direction + findings
- `mesut-verdict-2026-02-10.md` — earlier repo review

## Notes

- If `tech-stack.md` and `mvp-plan.md` appear to conflict: follow `mvp-plan.md` for execution; `tech-stack.md` is target-state.

---

*Maintained by Mesut.*
