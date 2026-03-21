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

See [docs/architecture.md](docs/architecture.md) for detailed design.

| Feature | OpenClaw | ALMS |
|---------|----------|------|
| Language | Node.js | Rust (single binary) |
| Session Keys | Complex scope rules | Simple explicit context_id |
| Concurrency | Promise-based, locks | tokio async, lock-free maps |
| Storage | JSON files | SQLite (WAL mode) |
| Tools | In-process, no isolation | WASM sandbox + capability gating |
| Transport | WebSocket only | SSE streaming + HTTP API |

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
