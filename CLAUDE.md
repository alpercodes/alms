# ALMS — Claude Code Instructions

## What is this project?

ALMS (Agent Loop Management System) is a Rust-based multi-agent coordination platform. A single daemon exposes an HTTP/SSE API, runs agent loops against LLM providers, executes native tools with per-tool sandboxing (project-root path canonicalization, shell permissions, Landlock on Linux — see [`docs/security-model.md` § 4.4](docs/security-model.md#44-filesystem-sandboxing-implemented) for the platform asymmetry between Linux and Windows/macOS), and manages sessions with snapshot persistence.

## Build & Run

```bash
# Requires Rust nightly (auto-installed from rust-toolchain.toml)
# On Windows, cargo is at ~/.cargo/bin/cargo — use `export PATH="$HOME/.cargo/bin:$PATH"` if needed
# Frontend requires the exact Node/npm versions in .node-version and package.json
npm ci
cargo build --release
cargo run --bin alms -- gateway --bind 127.0.0.1:8080

# Run all CI checks locally
make ci          # fmt-check + clippy + test + build-release
make test        # cargo test --all
make test-golden # SSE golden tests only
make clippy      # cargo clippy -- -D warnings
npm run ui:check # TypeScript + ESLint + Prettier + both test runners
npm run ui:build # Rebuild committed rust-embed assets
npm run ui:test:e2e
```

**Two frontend test runners, two trees.** `npm run ui:test` runs both halves:
`ui:test:unit` (Vitest over `frontend/`) and `ui:test:behavior` (Node's
`node:test` over `crates/alms-gateway/tests/ui/*.test.mjs`, which is where the
tests for `crates/alms-gateway/static/ui/` live). The behaviour suites also run
under `cargo test -p alms-gateway` via `tests/ui_behavior.rs` — one `#[test]`
per suite, kept in step with the directory by the
`every_ui_test_file_has_a_cargo_test` guard, so a suite cannot be picked up by
one runner and silently missed by the other (issue #7; before it, three suites
ran under neither).

## Project Structure

```
crates/
  alms-core/         # Core types (IDs, Capability, AuditEvent, Channel trait, errors)
                     #   config.rs — unified AlmsConfig (layered: defaults → TOML → env vars)
                     #   registry.rs — AgentRecord, CreateAgentRequest, validate_agent_name
  alms-gateway/      # Axum HTTP server, SSE streaming, run lifecycle, event log
  alms-runtime/      # Agent loop, LLM client, tool registry, context builder, workspace
                     #   agent/mod.rs — AgentRuntime struct, public API, run orchestration
                     #   agent/loop_impl.rs — agent_loop(), stream_llm_call(), execute_tool_call()
                     #   agent/context.rs — build_context(), episodic summaries, summarization
                     #   agent/dm.rs — DM-specific helpers, conflict detection
                     #   agent/types.rs — Posture, AgentConfig, SystemPrompts, RunOutput
                     #   context/mod.rs — ContextBuilder orchestrator (token-budgeted context window)
                     #   context/normalize.rs — canonical message-shape pipeline (group/strip/normalize)
                     #   context/strategies.rs — token-budgeted history selection (full/truncate/sliding)
                     #   context/perspective.rs — DM-perspective role mapping + reasoning-message filter
                     #   context/rebuild.rs — persisted Message → LlmMessage reconstruction (incl. spill repair)
                     #   context/error_markers.rs — error-marker classification + legacy run-boundary dedup
                     #   workspace.rs — AgentWorkspace (personality/goals/memories/user files)
                     #   episodic.rs — Cross-session episodic memory (summary generation + formatting)
                     #   workspace_tool.rs — WorkspaceWriteTool (exception: stays in runtime)
  alms-tools/        # Tool implementations extracted from alms-runtime
                     #   8 agent tools (send_message, invoke_agent, read_session, etc.)
                     #   SubagentDispatcher, MessageSender traits
                     #   EventForwarder trait for type-erased runtime event forwarding
  alms-coordinator/  # Multi-agent orchestration — pure hierarchy, real AgentRuntime loops
  alms-session/      # Session management, SQLite persistence, episodic summary storage
                     #   sqlite/session_summaries.rs — per-session episodic summary persistence
                     #   sqlite/migrations.rs — ordered transactional schema migrations
  alms-sandbox/      # Builtin native tools (echo, math, http_get, shell, fs_*, etc.) + tool registry
  alms-channel/      # Channel adapters (Telegram polling implemented)
  alms-cli/          # CLI entrypoint (clap) — gateway, health, agent/session/run/job management
frontend/            # Strict TypeScript contracts, bridge, Vitest, Playwright
crates/alms-gateway/static/ui/       # Editable browser UI source
crates/alms-gateway/static/ui-dist/  # Committed deterministic Vite output embedded by Rust
docs/                # Design docs — api.md, architecture.md, security-model.md, etc.
                     #   agent-runtime-design.md — detailed design for config/context/workspace
                     #   agent-ux-requirements.md — Alper's UX requirements
                     #   system-prompts.md — prompt file inventory and assembly order
                     #   _archive/ — archived/superseded docs
                     #   research/ — competitive analysis and tech-stack decisions
```

### Dependency graph

```
alms-cli → alms-gateway → alms-runtime      → alms-core
                        → alms-tools        → alms-core
                                            → alms-session
                                            → alms-sandbox
                        → alms-coordinator  → alms-core
                                            → alms-session
                                            → alms-runtime
                                            → alms-tools
                        → alms-channel      → alms-core
                        → alms-session      → alms-core
           alms-runtime → alms-sandbox      → alms-core
                        → alms-session
         → alms-session
```

## Code Conventions

- **Formatting**: `cargo fmt` — enforced in CI, zero tolerance
- **Linting**: `cargo clippy -- -D warnings` — all warnings are errors
- **Edition**: Rust 2024 (nightly)
- **Error handling**: `thiserror` for library errors, `anyhow` in CLI/binary code
- **Logging**: `tracing` with structured fields and `#[instrument]` macros
- **Concurrency**: `tokio` async runtime, `DashMap` for concurrent maps, `parking_lot` for locks
- **IDs**: Newtype wrappers (`AgentId`, `SessionId`, `RunId`, etc.) — never raw strings
- **Tests**: `#[cfg(test)] mod tests` in each file. Golden tests for SSE in `alms-gateway/tests/`
- **Frontend**: new code is strict TypeScript; all API/SSE JSON crosses the mandatory validated bridge
- **Frontend assets**: run `npm run ui:build` and commit `static/ui-dist/`; CI rejects generated drift

## Git Workflow

- Feature branches: `fix/<name>` or `feature/<name>`
- **PRs target `develop`** — always use branches + PRs, never commit directly. `main` is for release merges only.
- Run `make ci` before pushing

## Current State

**Version:** `v0.2.3` released and stable on `main` (tag `v0.2.3`); **v0.2.4 in progress** on `develop`. See [`CHANGELOG.md`](CHANGELOG.md) for per-release notes and operator-facing default changes (model/provider defaults, Anthropic thinking budget, agent-loop hard caps). The former task ledger is preserved in `docs/_archive/TASKS.md` for history; it is not the current priority list.

**Roughly what works** — the agent runtime (tool loop, token-budgeted context builder, workspace files, episodic memory), the HTTP/SSE gateway + web UI, SQLite persistence, multi-provider LLM support (OpenAI/OpenRouter, Anthropic, Gemini — with reasoning/thinking and prompt/context caching), multi-agent coordination (subagents via `invoke_agent`, peer-to-peer DMs via `send_message`), per-tool sandboxing + shell permissions, the agent registry + CLI, cron/scheduled jobs, and the approval workflow. The **Project Structure** map above and the design docs under `docs/` are the source of truth for how each piece works.

**Not yet real:** recursive subagent spawning (subagents can't spawn sub-subagents), progress reporting, and `invoke_agent` result truncation (full responses are still stored in parent context). See `docs/autonomous-subagents-design.md`.

See also `docs/agent-runtime-design.md` (runtime design) and `docs/agent-ux-requirements.md` (UX requirements).

> **Maintaining this section:** keep it a short, stable snapshot. Per-PR detail belongs in `CHANGELOG.md`, the PR description, and git history — **not here**. This file is loaded into context every session, so each paragraph added is a recurring token cost.
