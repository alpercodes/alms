# ALMS — Claude Code Instructions

## What is this project?

ALMS (Agent Loop Management System) is a Rust-based multi-agent coordination platform. A single daemon exposes an HTTP/SSE API, runs agent loops against LLM providers, executes tools in a WASM sandbox, and manages sessions with snapshot persistence.

## Build & Run

```bash
# Requires Rust nightly (auto-installed from rust-toolchain.toml)
# On Windows, cargo is at ~/.cargo/bin/cargo — use `export PATH="$HOME/.cargo/bin:$PATH"` if needed
cargo build --release
cargo run --bin alms -- gateway --bind 127.0.0.1:8080

# Run all CI checks locally
make ci          # fmt-check + clippy + test + build-release
make test        # cargo test --all
make test-golden # SSE golden tests only
make clippy      # cargo clippy -- -D warnings
```

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
                     #   context.rs — ContextBuilder (token-budgeted context window)
                     #   workspace.rs — AgentWorkspace (personality/goals/memories/user files)
                     #   episodic.rs — Cross-session episodic memory (summary generation + formatting)
                     #   workspace_tool.rs — WorkspaceWriteTool (exception: stays in runtime)
  alms-tools/        # Tool implementations extracted from alms-runtime
                     #   8 agent tools (send_message, invoke_agent, read_session, etc.)
                     #   SubagentDispatcher, MessageSender traits
                     #   EventForwarder trait for type-erased runtime event forwarding
  alms-coordinator/  # Multi-agent orchestration — pure hierarchy, real AgentRuntime loops
  alms-session/      # Session store, JSON snapshot persistence (atomic + rotation + checksums)
                     #   sqlite/session_summaries.rs — per-session episodic summary persistence
  alms-sandbox/      # WASM tool sandbox, builtin tools (echo, math, http_get), registry
  alms-channel/      # Channel adapters (Telegram polling implemented)
  alms-cli/          # CLI entrypoint (clap) — gateway, health, agent/session/run/job management
docs/                # Design docs — api.md, architecture.md, security-model.md, etc.
                     #   agent-runtime-design.md — detailed design for config/context/workspace
                     #   agent-ux-requirements.md — Alper's UX requirements
                     #   system-prompts.md — prompt file inventory and assembly order
                     #   _archive/ — archived/superseded docs
                     #   research/ — competitive analysis and tech-stack decisions
```

### Dependency graph (no cycles, 9 crates)

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

## Claude Code Agent Team

Internal agents (under Atlas's control, use git worktree isolation):

| Agent | Role | Worktree | Co-Authored-By | Can Edit Code |
|-------|------|----------|----------------|---------------|
| **Atlas** | Main developer | Main repo | `Atlas <noreply@anthropic.com>` | Yes |
| **Heph** (`heph-dev`) | Feature dev | Isolated | `Heph <noreply@anthropic.com>` | Yes |
| **Tim** (`alms-dev-guardian`) | Code reviewer | Isolated | `Tim <noreply@anthropic.com>` | No (read-only) |
| **Larry** (`larry-bug-fix`) | Bug fixer | Isolated | `Larry <noreply@anthropic.com>` | Yes |

External agents (not under Atlas's control):

| Agent | Role |
|-------|------|
| **Argus** | External — independent agent |
| **Tesla** | External — independent agent |

- All internal agents use `isolation: "worktree"` so they never interfere with each other
- Tim posts reviews as GitHub PR comments with `## Review by Tim (automated)` header
- Larry creates branches, fixes bugs, pushes, creates PRs, and comments on GitHub issues
- Atlas coordinates, plans, implements features, and posts Tim's reviews when Bash is blocked in worktrees

## Git Workflow

- Feature branches: `fix/<name>` or `feature/<name>`
- PRs target `main` — always use branches + PRs, never commit directly to main
- Run `make ci` before pushing

### VPS & Remotes

> **Note**: VPS deployment is not in scope right now. Will be picked up later.

- **VPS**: `root@<vps-host>` (Ubuntu 24.04, 4GB RAM)
- **Canonical repo on VPS**: `</srv/alms` (has `main` checked out)
- **Git remote `atlas`**: points to the VPS canonical repo
- **Pushing to VPS**: The VPS repo has `main` checked out, so direct pushes are refused by default. To push: temporarily set `receive.denyCurrentBranch=updateInstead` on VPS, push, then reset to `refuse`.
- **Agent workspace repos**: `</srv/workspace-atlas/alms`, `</srv/workspace-mustafa/alms`

## Current State (as of 2026-03-28)

**Working**: core types, unified config, session management, agent runtime with tool loop + context builder + workspace integration, HTTP gateway with SSE, Telegram adapter, SQLite persistence (`./data/alms.db`), agent workspace files (personality/goals/memories/user — auto-created on `alms agent create`), bootstrap interview, builtin tools (echo, math, http_get, shell_exec, fs_read, fs_write, fs_list, workspace_write, invoke_agent, read_subagent_session, send_message, list_agents, read_messages, ignore_message, list_my_sessions, read_session), per-run overrides (model, max_tokens, posture), approval workflow (guarded posture), cron/scheduler, scheduled jobs (SQLite-backed), audit log, web UI with agent selector dropdown + session sidebar + dedicated Agents panel + workspace/jobs/audit panels + agent onboarding UI, multi-agent (hierarchy + peer messaging — foreground and background subagents via invoke_agent; peer-to-peer DM via send_message tool + MessageBus + DM sessions with perspective mapping; named subagents with registry-based config + workspace; persistent sessions via UUID v5; on-demand context via read_subagent_session; parallel tool execution via join_all; sliding-summary context compression), bearer auth (`ALMS_AUTH_TOKEN`), graceful shutdown (Ctrl+C / SIGTERM → drain in-flight runs → flush WAL), agent registry (named persistent agents with SQLite CRUD + auto-migration from sidecar `./data/agent_id`), agent HTTP API (`/agents` CRUD — list, create, get, update, delete, set-default; resolves by UUID or name slug), per-agent config overrides (model/posture merged into runs with three-layer precedence: per-run > per-agent > server default), CLI agent management (`alms agent {list, create, show, delete, set-default, config}`), CLI session management (`alms session {list, show, delete}`), CLI run commands (`alms run {create, list, show}` via HTTP API), CLI job commands (`alms job {list, show}` via SQLite, `alms job {create, cancel}` via HTTP API), shell completions (`alms completions <shell>`), browser dashboard (`alms dashboard`), tool call persistence (tool calls and results persisted to session DB and reconstructed into structured LLM messages during context building — full tool execution history survives across runs, parallel tool calls grouped into single assistant message; DM sessions excluded from session-level persistence — tool calls stored in per-run `run_tool_calls` table only, retrievable via `GET /runs/{run_id}/tool-calls`; partial tool call records persisted on error/cancellation for debugging), multi-provider LLM support (OpenAI/OpenRouter + Anthropic Messages API — provider selected via `llm.provider` config or `ALMS_LLM_PROVIDER` env var; Anthropic streaming, tool use, and response format fully mapped), cross-session episodic memory (run summaries generated after each run via heuristic or LLM mode, stored in `session_summaries` SQLite table, injected into context window for cross-session awareness; `list_my_sessions` and `read_session` tools for on-demand session recall; configurable via `run_summary_mode`/`run_summary_budget` with 15% hard cap), DM conversation lifecycle (`ignore_message` and depth-exceeded trigger `end_conversation` with `dm_ended` session markers, depth counter reset, `ConversationEnded` peer notification via dedicated `notifications:{agent}` sessions, `dm_conversation_ended` SSE event; `DEPTH_EXPIRY_SECS` raised to 1800s; see #384 Phases 1-7).

**Not yet real**: recursive subagent spawning (subagents can't yet spawn sub-subagents), progress reporting, invoke_agent result truncation (full responses still stored in parent context). See `docs/autonomous-subagents-design.md`.

See `docs/TASKS.md` for the prioritized task list, `docs/agent-runtime-design.md` for the runtime design, and `docs/agent-ux-requirements.md` for UX requirements.
