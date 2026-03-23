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
  alms-runtime/      # Agent loop, LLM client (OpenAI-compat), tool execution, audit
                     #   context.rs — ContextBuilder (token-budgeted context window)
                     #   workspace.rs — AgentWorkspace (personality/goals/memories/user files)
  alms-coordinator/  # Multi-agent orchestration — pure hierarchy, real AgentRuntime loops
  alms-session/      # Session store, JSON snapshot persistence (atomic + rotation + checksums)
  alms-sandbox/      # WASM tool sandbox, builtin tools (echo, math, http_get), registry
  alms-channel/      # Channel adapters (Telegram polling implemented)
  alms-cli/          # CLI entrypoint (clap) — gateway, health, agent/session/run/job management
docs/                # Design docs — api.md, architecture.md, security-model.md, etc.
                     #   agent-runtime-design.md — detailed design for config/context/workspace
                     #   agent-ux-requirements.md — Alper's UX requirements
                     #   system-prompts.md — prompt file inventory and assembly order
_quarantine/         # Archived/superseded docs
research/            # Competitive analysis and tech-stack decisions
```

### Dependency graph (no cycles)

```
alms-cli → alms-gateway → alms-runtime    → alms-core
         → alms-session → alms-core
                        → alms-coordinator → alms-core
                                           → alms-session
                                           → alms-runtime
                        → alms-channel    → alms-core
                        → alms-session    → alms-core
           alms-runtime → alms-sandbox    → alms-core
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

## Configuration System

Unified config in `alms-core/src/config.rs` (`AlmsConfig`):
- **Layered precedence**: compiled defaults → `alms.toml` config file → env var overrides
- **Secrets** (API keys, tokens): loaded from `data/secrets.json` (via `alms auth set` or UI) with fallback to env vars. Never stored in `alms.toml` (`#[serde(skip)]`)
- See `alms.toml.example` for all options with documentation
- Key env vars: `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `ALMS_LLM_PROVIDER` (`openai`|`anthropic`), `TELEGRAM_BOT_TOKEN`, `ALMS_LLM_MOCK=1`, `DEFAULT_MODEL`, `LLM_BASE_URL`, `ALMS_AGENT_ID`, `ALMS_AUTH_TOKEN`, `ALMS_SANDBOX_ROOT`, `ALMS_SHELL_POLICY`, `ALMS_DATA_DIR` (default: `./data`), `ALMS_WORKSPACE_DIR` (default: `{data_dir}/workspace`), `ALMS_MASTER_KEY` (encrypts `data/secrets.json` at rest with AES-256-GCM)
- `GatewayConfig::from_env()` uses `AlmsConfig::load()` internally — single source of truth
- **Agent ID persistence**: the default agent UUID is stored in `./data/agent_id` (plain-text sidecar file). Precedence: `ALMS_AGENT_ID` env var > sidecar file > generate new. To reconnect existing data after a migration: `echo "<uuid>" > ./data/agent_id`

## Agent Runtime Architecture

The agent runtime (`alms-runtime`) has three key subsystems:

1. **ContextBuilder** (`context.rs`): Assembles token-budgeted context windows for LLM calls. Strategies: `truncate` (default), `full`, `sliding-summary` (rolling LLM summary of old messages + recent window verbatim). Config via `ContextConfig`.

2. **AgentWorkspace** (`workspace.rs`): Per-agent persistent identity files:
   - `personality.md` — the *agent's* tone, style, role, constraints (agent + user editable; agent writes during bootstrap)
   - `goals.md` — the agent's current objectives (agent + user editable)
   - `memories.md` — what the agent has learned: domain facts, past decisions, accumulated knowledge (agent + user editable)
   - `user.md` — who the *user* is: name, working style, preferences, background (agent + user editable; filled during bootstrap interview)
   - Prepended to system prompt when workspace is attached to runtime
   - `needs_bootstrap()` detects first-time agents (no `personality.md`)
   - `alms agent create` initializes empty workspace files; `init_workspace_files()` in `alms-core` is idempotent

3. **ToolRegistry** (`tools.rs`): Tools expose JSON Schema parameters via `fn parameters() -> Value`. Definitions serialize to OpenAI format: `{"type": "function", "function": {"name", "description", "parameters"}}`.

## LLM Types

- `LlmMessage.content` is `Option<String>` (null when LLM returns tool calls only)
- Two `LlmConfig` types exist: `alms_core::config::LlmConfig` (canonical) and `alms_runtime::llm_types::LlmConfig` (legacy, with `From` bridge). Prefer the core one for new code.

## Claude Code Agent Team

Three agents work in parallel using git worktree isolation (`.claude/agents/`):

| Agent | Role | Worktree | Co-Authored-By | Can Edit Code |
|-------|------|----------|----------------|---------------|
| **Atlas** | Main developer | Main repo | `Atlas <noreply@anthropic.com>` | Yes |
| **Tim** (`alms-dev-guardian`) | Code reviewer | Isolated | `Tim <noreply@anthropic.com>` | No (read-only) |
| **Larry** (`larry-bug-fix`) | Bug fixer | Isolated | `Larry <noreply@anthropic.com>` | Yes |

- All agents use `isolation: "worktree"` so they never interfere with each other
- Tim posts reviews as GitHub PR comments with `## Review by Tim (automated)` header
- Larry creates branches, fixes bugs, pushes, creates PRs, and comments on GitHub issues
- Atlas coordinates, plans, implements features, and posts Tim's reviews when Bash is blocked in worktrees

## Git Workflow

- Feature branches: `fix/<name>` or `feature/<name>`
- PRs target `main` — always use branches + PRs, never commit directly to main
- Run `make ci` before pushing

### VPS & Remotes

- **VPS**: `root@<vps-host>` (Ubuntu 24.04, 4GB RAM)
- **Canonical repo on VPS**: `</srv/alms` (has `main` checked out)
- **Git remote `atlas`**: points to the VPS canonical repo
- **Pushing to VPS**: The VPS repo has `main` checked out, so direct pushes are refused by default. To push: temporarily set `receive.denyCurrentBranch=updateInstead` on VPS, push, then reset to `refuse`.
- **Agent workspace repos**: `</srv/workspace-atlas/alms`, `</srv/workspace-mustafa/alms`

## Key Design Decisions

- **SSE over WebSockets** for streaming (simpler, proxy-friendly, reconnect via Last-Event-ID)
- **SQLite persistence** via `SqliteStore` — sessions + audit events persisted to `./data/alms.db` by default
- **WASM sandbox** for tool isolation; native builtins bypass WASM for now
- **Single-process daemon** — no microservice split planned for MVP
- **Mock LLM** available via `ALMS_LLM_MOCK=1` env var for testing without API keys
- **Simple config** — avoid the OpenClaw pattern of confusing nested settings; flat, predictable keys
- **Multi-agent: hierarchy + peer messaging** — vertical delegation via `invoke_agent` tool (any agent can spawn subagents; results flow up from children to parents). Peer-to-peer direct messaging via `send_message` tool (Layer 2 — agents can send messages to any other agent by name through a shared MessageBus; delivered into the recipient's next context window via DM sessions with perspective mapping). Named subagents (`name` param) must be pre-registered via `alms agent create` (which creates workspace dir + empty identity files and outputs the workspace path); config (model, posture) loaded from agent registry, workspace attached at `{workspace_dir}/{name}/`. Persistent sessions via UUID v5 deterministic identity — conversation history preserved across invocations. `read_subagent_session` tool for on-demand context retrieval from subagent sessions. Default system prompt tells agents they can run `alms --help` via shell_exec to discover CLI commands, enabling autonomous subagent creation.

## Known Issues

- `fs_*` tools are sandboxed via `canonicalize()` + prefix check against `tools.sandbox_root` (default: cwd). When a workspace is attached (via `with_workspace()`), `fs_read`/`fs_write`/`fs_list` are re-sandboxed to the agent's workspace directory, which is more restrictive than `tools.sandbox_root`. Ephemeral subagents (without a named workspace) get a disposable workspace at `{workspace_dir}/.ephemeral/{task_id}/` to prevent project-root access. `shell_exec` cwd is restricted in sandboxed mode, but the executed command itself can still access files outside the sandbox — for true shell isolation, use a restricted OS user or Landlock (future task)
- `data/secrets.json` is protected by a filename denylist in `fs_read`, `fs_write`, and `shell_exec` (argv check). This is best-effort — indirect access via shell_exec is still possible. For production deployments, set `ALMS_MASTER_KEY` to enable AES-256-GCM encryption of secrets at rest
- Run cancellation does not propagate to active subagents — cancelling a parent run drops the `join_all` future but subagent tasks in the Coordinator continue running to completion (or timeout). This means subagent LLM calls keep burning tokens after the user cancels. Fix requires threading the parent's `CancellationToken` into `spawn_subagent`.

## Current State (as of 2026-03-22)

**Working**: core types, unified config, session management, agent runtime with tool loop + context builder + workspace integration, HTTP gateway with SSE, Telegram adapter, SQLite persistence (`./data/alms.db`), agent workspace files (personality/goals/memories/user — auto-created on `alms agent create`), bootstrap interview, builtin tools (echo, math, http_get, shell_exec, fs_read, fs_write, fs_list, workspace_write, invoke_agent, get_task_result, read_subagent_session, send_message), per-run overrides (model, max_tokens, posture), approval workflow (guarded posture), cron/scheduler, scheduled jobs (SQLite-backed), audit log, web UI with agent selector dropdown + session sidebar + dedicated Agents panel + workspace/jobs/audit panels + agent onboarding UI, multi-agent (hierarchy + peer messaging — foreground and background subagents via invoke_agent; peer-to-peer DM via send_message tool + MessageBus + DM sessions with perspective mapping; named subagents with registry-based config + workspace; persistent sessions via UUID v5; on-demand context via read_subagent_session; parallel tool execution via join_all; sliding-summary context compression), bearer auth (`ALMS_AUTH_TOKEN`), graceful shutdown (Ctrl+C / SIGTERM → drain in-flight runs → flush WAL), agent registry (named persistent agents with SQLite CRUD + auto-migration from sidecar `./data/agent_id`), agent HTTP API (`/agents` CRUD — list, create, get, update, delete, set-default; resolves by UUID or name slug), per-agent config overrides (model/posture merged into runs with three-layer precedence: per-run > per-agent > server default), CLI agent management (`alms agent {list, create, show, delete, set-default, config}`), CLI session management (`alms session {list, show, delete}`), CLI run commands (`alms run {create, list, show}` via HTTP API), CLI job commands (`alms job {list, show}` via SQLite, `alms job {create, cancel}` via HTTP API), shell completions (`alms completions <shell>`), browser dashboard (`alms dashboard`), tool call persistence (tool calls and results persisted to session DB and reconstructed into structured LLM messages during context building — full tool execution history survives across runs, parallel tool calls grouped into single assistant message; DM sessions excluded from session-level persistence — tool calls stored in per-run `run_tool_calls` table only, retrievable via `GET /runs/{run_id}/tool-calls`; partial tool call records persisted on error/cancellation for debugging), multi-provider LLM support (OpenAI/OpenRouter + Anthropic Messages API — provider selected via `llm.provider` config or `ALMS_LLM_PROVIDER` env var; Anthropic streaming, tool use, and response format fully mapped).

**Not yet real**: recursive subagent spawning (subagents can't yet spawn sub-subagents), progress reporting, invoke_agent result truncation (full responses still stored in parent context). See `docs/autonomous-subagents-design.md`.

See `docs/TASKS.md` for the prioritized task list, `docs/agent-runtime-design.md` for the runtime design, and `docs/agent-ux-requirements.md` for UX requirements.
