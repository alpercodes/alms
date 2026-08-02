# ALMS Docs Index

This is the entrypoint to ALMS documentation.

## Product vision

- `product-vision-core.md` — the definitive product vision: agent teams that collaborate on projects
- `ux-principles.md` — product UX principles (Run Timeline, artifacts, diff-first)

## Core design

- `architecture.md` — system architecture (multi-agent hierarchy, components, dependency graph)
- `tech-stack.md` — implemented stack decision, current boundaries, and clearly labeled target constraints
- `api.md` — HTTP/SSE API contract
- `security-model.md` — capabilities, approvals, guardrails
- `events-and-audit.md` — event streams, approvals, audit invariants
- `database-migrations.md` — SQLite schema versions, compatibility, backup, and rollback

- `frontend.md` — Vite/TypeScript build, validated runtime boundary, and normalized entity state

## Agent runtime

- `agent-runtime-design.md` — config, context builder, workspace subsystems
- `agent-ux-requirements.md` — Alper's UX requirements (config, context, workspace, tokens)
- `autonomous-subagents-design.md` — historical roadmap with a current-status banner for remaining subagent work
- `persistent-agents-cli-design.md` — named agents, CLI management, registry
- `jobs-await-completion-design.md` — scheduled-job lifecycle: job episodes stay active across triggered DMs + background subagents (#1198) — *APPROVED, phase 1 implemented*

## Communication

- `communication-architecture.md` — hybrid communication model (structured + NL)
- `layer2-peer-messaging-design.md` — peer-to-peer DM design and lifecycle
- `system-prompts.md` — prompt file inventory and assembly order

## Tools & sandbox

- `tool-sandbox-abi.md` — WASM tool ABI specification (v0) — *design-only; WASM substrate removed from the codebase, see doc header*
- `wasm-sandbox-vs-openclaw-skills.md` — WASM sandbox vs OpenClaw skills competitive analysis — *design-only; WASM substrate removed from the codebase, see doc header*

## Planning & future

- `engineering-stabilization-plan.md` — eight-phase concurrency, persistence, recovery, and maintainability program; Phases 1–7 complete and Phase 8 in progress
- `phase8-decomposition-plan.md` — final stabilization scope, ownership boundaries, and validation gates
- `workflow-layer.md` — WorkItems/ChangeSets/PR reviews as first-class workflow layer
- `testing-strategy.md` — deterministic time, mock LLM, Rust/frontend/browser CI, and golden tests
- `proposal.md` — historical 2026-02-10 proposal and repository snapshot; not current implementation status

## Archived

Outdated reviews, superseded design docs, and completed investigation reports
are in `_archive/`. They are preserved for historical reference.

The completed Phase 6 and Phase 7 implementation plans are archived as
`_archive/phase6-normalized-frontend-plan.md` and
`_archive/phase7-durable-operations-plan.md`.

---

*Last updated 2026-08-01.*
