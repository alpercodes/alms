# ALMS — Claude Code Instructions

## What is this project?

ALMS (Agent Loop Management System) is a Rust-based multi-agent coordination platform. A single daemon exposes an HTTP/SSE API, runs agent loops against LLM providers, executes tools in a WASM sandbox, and manages sessions with snapshot persistence.

## Build & Run

```bash
# Requires Rust nightly (auto-installed from rust-toolchain.toml)
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
  alms-gateway/      # Axum HTTP server, SSE streaming, run lifecycle, event log
  alms-runtime/      # Agent loop, LLM client (OpenAI-compat), tool execution, audit
  alms-coordinator/  # Multi-agent orchestration (scaffold — not yet real)
  alms-session/      # Session store, JSON snapshot persistence (atomic + rotation + checksums)
  alms-sandbox/      # WASM tool sandbox, builtin tools (echo, math, http_get), registry
  alms-channel/      # Channel adapters (Telegram polling implemented)
  alms-cli/          # Thin CLI entrypoint (clap)
docs/                # Design docs — api.md, architecture.md, security-model.md, etc.
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

## Git Workflow

- Feature branches: `feature/<name>`
- PRs target `main`
- Pre-commit hook blocks direct main commits
- Remote name `canonical` is used for upstream (not `origin`)
- Run `make ci` before pushing

## Key Design Decisions

- **SSE over WebSockets** for streaming (simpler, proxy-friendly, reconnect via Last-Event-ID)
- **Snapshot persistence** (JSON + atomic write + rotation) is the current store; SQLite is planned
- **WASM sandbox** for tool isolation; native builtins bypass WASM for now
- **Single-process daemon** — no microservice split planned for MVP
- **Mock LLM** available via `ALMS_LLM_MOCK=1` env var for testing without API keys

## Current State (as of 2026-02)

Working: core types, session management, agent runtime with tool loop, HTTP gateway with SSE, Telegram adapter, snapshot persistence, builtin tools.

Not yet real: coordinator/multi-agent (stub), approval workflow, cron/scheduler, SQLite storage, tool parameter schemas (empty `{}`).

See `docs/TASKS.md` for the prioritized task list and `docs/zeki-review-2026-02-12.md` for the most recent full review.
