# Contributing to ALMS

## Development Workflow

### Prerequisites

ALMS requires Rust nightly (specified in `rust-toolchain.toml`). The toolchain will be automatically installed when you run cargo commands.

### Running CI Checks Locally

Before pushing, run the full CI pipeline locally:

```bash
make ci
```

Or run individual checks:

```bash
# Check formatting
make fmt-check

# Run clippy lints
make clippy

# Run tests
make test

# Run golden tests specifically
make test-golden
```

### Code Style

- **Formatting**: Use `cargo fmt` (enforced in CI)
- **Linting**: All clippy warnings must be resolved (enforced in CI)
- **Testing**: All tests must pass (enforced in CI)

### Git Workflow

1. Fetch latest main from canonical:
   ```bash
   git fetch canonical main
   git reset --hard canonical/main
   ```

2. Create feature branch:
   ```bash
   git checkout -b feature/<name>
   ```

3. Make changes and commit

4. Verify changes before PR:
   ```bash
   git diff main..HEAD --stat
   make ci
   ```

5. Push and create PR summary

## CI Pipeline

The CI pipeline (`.github/workflows/ci.yml`) runs:

1. `cargo fmt --all -- --check` - Ensures consistent formatting
2. `cargo clippy --all-targets --all-features -- -D warnings` - Static analysis
3. `cargo test --all` - Runs all tests including golden tests
4. `cargo build --release` - Verifies release build works

## Project Structure

- `crates/alms-core` - Core types and errors
- `crates/alms-gateway` - HTTP API and SSE streaming
- `crates/alms-runtime` - Agent runtime and tool execution
- `crates/alms-coordinator` - Multi-agent orchestration
- `crates/alms-session` - Session management
- `crates/alms-sandbox` - WASM tool sandbox
- `crates/alms-channel` - Messaging platform adapters
- `crates/alms-cli` - Command-line interface