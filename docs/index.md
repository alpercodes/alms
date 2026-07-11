# ALMS Docs Index

This is the entrypoint to ALMS documentation.

## Product vision

- `product-vision-core.md` — the definitive product vision: agent teams that collaborate on projects
- `ux-principles.md` — product UX principles (Run Timeline, artifacts, diff-first)

## Core design

- `architecture.md` — system architecture (multi-agent hierarchy, components, dependency graph)
- `tech-stack.md` — recommended stack + rationale (Rust, SQLite, SSE)
- `api.md` — HTTP/SSE API contract
- `security-model.md` — capabilities, approvals, guardrails
- `events-and-audit.md` — event streams, approvals, audit invariants
- `database-migrations.md` — SQLite schema versions, compatibility, backup, and rollback

## Agent runtime

- `agent-runtime-design.md` — config, context builder, workspace subsystems
- `agent-ux-requirements.md` — Alper's UX requirements (config, context, workspace, tokens)
- `autonomous-subagents-design.md` — recursive spawning, progress reporting, cost budgets
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

- `workflow-layer.md` — WorkItems/ChangeSets/PR reviews as first-class workflow layer
- `testing-strategy.md` — deterministic time, mock LLM, golden tests
- `proposal.md` — consolidated direction + findings

## Archived

Outdated reviews, superseded design docs, and completed investigation reports
are in `_archive/`. They are preserved for historical reference.

---

*Last updated 2026-03-31.*
