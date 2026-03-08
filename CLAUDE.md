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
  alms-gateway/      # Axum HTTP server, SSE streaming, run lifecycle, event log
  alms-runtime/      # Agent loop, LLM client (OpenAI-compat), tool execution, audit
                     #   context.rs — ContextBuilder (token-budgeted context window)
                     #   workspace.rs — AgentWorkspace (personality/goals/memories files)
  alms-coordinator/  # Multi-agent orchestration — pure hierarchy (scaffold, not yet wired)
  alms-session/      # Session store, JSON snapshot persistence (atomic + rotation + checksums)
  alms-sandbox/      # WASM tool sandbox, builtin tools (echo, math, http_get), registry
  alms-channel/      # Channel adapters (Telegram polling implemented)
  alms-cli/          # Thin CLI entrypoint (clap)
docs/                # Design docs — api.md, architecture.md, security-model.md, etc.
                     #   agent-runtime-design.md — detailed design for config/context/workspace
                     #   agent-ux-requirements.md — Alper's UX requirements
_quarantine/         # Archived/superseded docs
research/            # Competitive analysis and tech-stack decisions
```

### Dependency graph (no cycles)

```
alms-cli → alms-gateway → alms-runtime  → alms-core
                        → alms-channel  → alms-core
                        → alms-session  → alms-core
           alms-runtime → alms-sandbox  → alms-core
                        → alms-session
     alms-coordinator   → alms-core
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
- **Secrets** (API keys, tokens) are ONLY loaded from env vars, never from config files (`#[serde(skip)]`)
- See `alms.toml.example` for all options with documentation
- Key env vars: `OPENROUTER_API_KEY`, `TELEGRAM_BOT_TOKEN`, `ALMS_LLM_MOCK=1`, `DEFAULT_MODEL`, `LLM_BASE_URL`
- `GatewayConfig::from_env()` uses `AlmsConfig::load()` internally — single source of truth

## Agent Runtime Architecture

The agent runtime (`alms-runtime`) has three key subsystems:

1. **ContextBuilder** (`context.rs`): Assembles token-budgeted context windows for LLM calls. Strategies: `truncate` (default), `full`, `sliding-summary` (falls back to truncate for now). Config via `ContextConfig`.

2. **AgentWorkspace** (`workspace.rs`): Per-agent persistent identity files:
   - `personality.md` — tone, style, constraints (agent + user editable; agent writes during bootstrap)
   - `goals.md` — current objectives (agent + user editable)
   - `memories.md` — learned facts, preferences (agent + user editable)
   - Prepended to system prompt when workspace is attached to runtime
   - `needs_bootstrap()` detects first-time agents (no `personality.md`)

3. **ToolRegistry** (`tools.rs`): Tools expose JSON Schema parameters via `fn parameters() -> Value`. Definitions serialize to OpenAI format: `{"type": "function", "function": {"name", "description", "parameters"}}`.

## LLM Types

- `LlmMessage.content` is `Option<String>` (null when LLM returns tool calls only)
- Two `LlmConfig` types exist: `alms_core::config::LlmConfig` (canonical) and `alms_runtime::llm_types::LlmConfig` (legacy, with `From` bridge). Prefer the core one for new code.

## Git Workflow

- Feature branches: `feature/<name>`
- PRs target `main`
- Commit directly to `main` (pre-commit hook removed)
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
- **Pure hierarchy multi-agent** — any agent can spawn subagents via `invoke_agent` tool; results flow up from children to parents; no peer-to-peer messaging between agents. Peer mesh (Option 2) is a possible future direction.

## Known Issues

- 4 sandbox/wasmtime tests fail with "must use async instantiation when async support is enabled" — pre-existing wasmtime config issue
- `sliding-summary` context strategy falls back to `truncate` (not yet implemented)
- Coordinator is still a stub — `execute_task` is a placeholder; task #29 will wire it to real `AgentRuntime`
- `shell_exec` / `fs_*` tools have no path-prefix or command allowlist beyond `..` traversal rejection — treat these as power-user features; use `Guarded` posture in shared environments

## Current State (as of 2026-03-09)

**Working**: core types, unified config, session management, agent runtime with tool loop + context builder + workspace integration, HTTP gateway with SSE, Telegram adapter, SQLite persistence (`./data/alms.db`), agent workspace files (personality/goals/memories), bootstrap interview, builtin tools (echo, math, http_get, shell_exec, fs_read, fs_write, fs_list, workspace_write), per-run overrides (model, temperature, max_tokens, posture), approval workflow (guarded posture), cron/scheduler, scheduled jobs (SQLite-backed), audit log, web UI with settings/workspace/jobs/audit panels.

**Not yet real**: coordinator/multi-agent (stub — pure hierarchy topology chosen; peer mesh is future), sliding-summary context strategy.

See `docs/TASKS.md` for the prioritized task list, `docs/agent-runtime-design.md` for the runtime design, and `docs/agent-ux-requirements.md` for UX requirements.
