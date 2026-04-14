# ALMS - Agent Loop Management System

A platform where teams of AI agents collaborate on projects — like a virtual company.

## Quick Start

```bash
# Requires Rust nightly (auto-installed from rust-toolchain.toml)
cargo build --release

# Run the gateway
./target/release/alms gateway --bind 127.0.0.1:8080

# Check health
curl http://127.0.0.1:8080/health
```

## Project Structure

```
alms/
├── Cargo.toml                 # Workspace configuration
├── crates/
│   ├── alms-core/            # Core types, config, errors
│   ├── alms-session/         # Session management, SQLite persistence
│   ├── alms-gateway/         # HTTP/SSE gateway, web UI
│   ├── alms-runtime/         # Agent runtime, LLM client, context builder
│   ├── alms-coordinator/     # Multi-agent orchestration
│   ├── alms-sandbox/         # WASM tool sandbox
│   ├── alms-channel/         # Channel adapters (Telegram)
│   └── alms-cli/             # Command-line interface
├── docs/                     # Design docs, reviews, task list
└── research/                 # Competitive analysis, tech-stack decisions
```

## Architecture

A single Rust binary runs the entire platform — HTTP gateway, agent runtime, tool sandbox, and session store.

- **Gateway** — Axum HTTP server with SSE streaming, web UI, and REST API for agents, sessions, and runs
- **Runtime** — Agent loop: builds context, calls LLM, executes tool results, manages workspace files (personality, goals, memories)
- **Coordinator** — Multi-agent orchestration: hierarchy (subagents), peer messaging (DM), and message bus
- **Sandbox** — WASM-based tool isolation with capability gating; builtin tools (shell, file I/O, datetime) run sandboxed per-agent
- **Session** — SQLite persistence (WAL mode) for sessions, messages, tool calls, episodic summaries, and agent registry
- **Channel** — Adapter layer for external transports (Telegram polling implemented)

See [docs/architecture.md](docs/architecture.md) for the full design.

## Development

### Prerequisites

- Rust nightly (auto-installed via `rust-toolchain.toml`)

### Build & Test

```bash
cargo build --release
cargo test --all
make ci          # fmt-check + clippy + test + build-release
```

### Run

```bash
cargo run --bin alms -- gateway
```

## License

MIT
