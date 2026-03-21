# ALMS Docs Index

This is the entrypoint to ALMS documentation.

## Product vision

- `product-vision-core.md` — the definitive product vision: agent teams that collaborate on projects
- `ux-principles.md` — product UX principles (Run Timeline, artifacts, diff-first)
- `ux-drift-analysis.md` — gap analysis: vision vs current implementation

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

## Tools & sandbox

- `tool-sandbox-abi.md` — WASM tool ABI specification (v0)
- `wasm-sandbox-vs-openclaw-skills.md` — ALMS WASM sandbox vs OpenClaw skill ecosystem

## Planning & structure

- `mvp-plan.md` — original MVP execution plan
- `mvp-module-crate-structure.md` — crate graph stability during MVP
- `workflow-layer.md` — WorkItems/ChangeSets/PR reviews as first-class workflow layer
- `testing-strategy.md` — deterministic time, mock LLM, golden tests
- `TASKS.md` — current task list / ownership

## Reviews & analysis

- `product-review-2026-03-20.md` — comprehensive product review (6.5/10 rating)
- `openclaw-competitive-analysis-2026-03-15.md` — OpenClaw competitive snapshot
- `zeki-review-2026-02-12.md` — fresh-eyes assessment + critical path
- `proposal.md` — consolidated direction + findings
- `mesut-verdict-2026-02-10.md` — earlier repo review

---

*Last updated 2026-03-21.*
