# ALMS - Agent Loop Management System

A technically superior, more usable, and more secure alternative to OpenClaw.

## Quick Start

```bash
# Clone and build
cd /workspace/alms
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
│   ├── alms-core/            # Core types and errors
│   ├── alms-session/         # Session management
│   ├── alms-gateway/         # HTTP/WebSocket gateway
│   ├── alms-runtime/         # Agent runtime
│   ├── alms-sandbox/         # WASM tool sandbox
│   ├── alms-channel/         # Channel adapters
│   └── alms-cli/             # Command-line interface
├── docs/
│   └── architecture.md       # System architecture
└── research/
    ├── session-issues.md     # OpenClaw analysis (subagent)
    └── tech-stack.md         # Rust vs Go decision
```

## Architecture

See [docs/architecture.md](docs/architecture.md) for detailed design.

Key improvements over OpenClaw:

| Feature | OpenClaw | ALMS |
|---------|----------|------|
| Language | Node.js | Rust (zero-cost abstractions) |
| Session Keys | Complex scope rules | Simple explicit context_id |
| Concurrency | Promise-based, locks | Actor model, lock-free |
| Storage | JSON files | Append-only log + snapshots |
| Tools | In-process | WASM sandbox |
| Transport | WebSocket only | Multi-protocol |

## Development

### Prerequisites

- Rust 1.75+ (install via [rustup](https://rustup.rs/))
- Cargo

### Build

```bash
cargo build --release
```

### Test

```bash
cargo test
```

### Run

```bash
cargo run --bin alms -- gateway
```

## License

MIT

