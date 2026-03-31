# ALMS Docs Index

This is the entrypoint to ALMS documentation.

## Product vision

- `product-vision-core.md` — the definitive product vision: agent teams that collaborate on projects
- `ux-principles.md` — product UX principles (Run Timeline, artifacts, diff-first)

## Core design

- `architecture.md` — system architecture (multi-agent hierarchy, components, dependency graph)
- `tech-stack.md` — recommended stack + rationale (Rust, SQLite, WASM, SSE)
- `api.md` — HTTP/SSE API contract
- `security-model.md` — capabilities, approvals, guardrails
- `events-and-audit.md` — event streams, approvals, audit invariants

## Agent runtime

- `agent-runtime-design.md` — config, context builder, workspace subsystems
- `agent-ux-requirements.md` — Alper's UX requirements (config, context, workspace, tokens)
- `autonomous-subagents-design.md` — recursive spawning, progress reporting, cost budgets
- `persistent-agents-cli-design.md` — named agents, CLI management, registry

## Communication

- `communication-architecture.md` — hybrid communication model (structured + NL)
- `layer2-peer-messaging-design.md` — peer-to-peer DM design and lifecycle
- `system-prompts.md` — prompt file inventory and assembly order

## Tools & sandbox

- `tool-sandbox-abi.md` — WASM tool ABI specification (v0)
- `wasm-sandbox-vs-openclaw-skills.md` — ALMS WASM sandbox vs OpenClaw skill ecosystem

## Planning & future

- `workflow-layer.md` — WorkItems/ChangeSets/PR reviews as first-class workflow layer
- `testing-strategy.md` — deterministic time, mock LLM, golden tests
- `proposal.md` — consolidated direction + findings

## Archived

Outdated reviews, superseded design docs, and completed investigation reports
are in `_archive/`. They are preserved for historical reference.

---

*Last updated 2026-03-31.*
