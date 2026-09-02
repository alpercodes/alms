# Contributing to ALMS

## Development Workflow

### Prerequisites

ALMS requires Rust nightly (specified in `rust-toolchain.toml`). The toolchain will be automatically installed when you run cargo commands.

### First-time identity setup

Three independent pieces. Set them up once after cloning; they make sure `git blame`, PR authorship, and comment attribution all point at you.

**1. Git author identity** (what `git blame` reads):

```bash
git config user.name "Your Name"
git config user.email "you@example.com"
```

Per-repo. Add `--global` if you want it for all your repos. Sanity-check after your first commit: `git log -1 --format='%an <%ae>'` should show you.

**2. GitHub identity** (what shows on PRs, issues, comments):

```bash
gh auth login
```

Interactive flow — sign in to your own GitHub account. Every `gh pr create`, `gh issue comment`, `gh pr merge`, etc. will be attributed to whoever `gh auth status` reports. If you use Claude Code or another agent, it inherits this identity automatically — there's no separate agent account.

**3. (Optional) Tell your AI coding agent to not add Co-Authored-By trailers**

Claude Code and similar tools add a `Co-Authored-By: <agent> <noreply@anthropic.com>` trailer to commits by default. If you'd prefer commits to be solely authored by you (clean blame, no agent disclosure), create your own `CLAUDE.local.md` (gitignored) at the repo root with:

```markdown
# My local Claude Code instructions

## Git identity
- Commits in this repo should be authored solely by me (whatever `git config user.name` reports)
- Do NOT add `Co-Authored-By` trailers for AI agents
- Do NOT add agent-identification footers like "🤖 Generated with Claude Code" to commits, PR bodies, or issue comments
```

Note: the `Co-Authored-By` trailer does NOT affect `git blame` — blame only reads the primary author from your git config. The trailer is purely additive metadata visible on the GitHub commit page. So the "no trailer" choice is about commit cosmetics, not blame correctness.

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

# Parse-check every static UI JS module (catches the bug class
# from PR #828 — see issue #829)
make test-static-assets
```

### Code Style

- **Formatting**: Use `cargo fmt` (enforced in CI)
- **Linting**: All clippy warnings must be resolved (enforced in CI)
- **Testing**: All tests must pass (enforced in CI)
- **Static UI**: Every `*.js` / `*.mjs` under `crates/alms-gateway/static/ui/`
  is parsed in module mode by `tests/static_assets_parse.rs` as part of
  `cargo test --all`. Module-syntax bugs (unterminated template literals,
  malformed `import` / `export`, stray tokens) fail CI before they ship.
  Module-resolution, runtime, and type errors are out of scope for this
  check — see issue #829.

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
3. `cargo test --all` - Runs all tests including golden tests and the
   static-asset JS parse-sweep (`tests/static_assets_parse.rs`, #829)
4. `cargo build --release` - Verifies release build works

`make ci` runs all of these steps locally for parity.

## Licensing

ALMS is Apache-2.0. The repository-level `LICENSE` is the authority, and the `license`
field is declared in both the workspace `Cargo.toml` and `package.json`. Between them
every file in the tree is covered.

Rust sources additionally carry a one-line SPDX header as their first line:

```rust
// SPDX-License-Identifier: Apache-2.0
```

Please add it to new `.rs` files. Nothing in CI enforces it.

Frontend sources under `crates/alms-gateway/static/ui/` and `frontend/` deliberately do
**not** carry the header. That is a convention choice — SPDX headers are a Rust-side habit
in this repository — and not a licensing gap, since `LICENSE` and the `package.json`
`license` field already cover them. Marking them too would be a reasonable change; propose
it as its own PR rather than folding it into an unrelated one.

## Project Structure

- `crates/alms-core` - Core types and errors
- `crates/alms-gateway` - HTTP API and SSE streaming
- `crates/alms-runtime` - Agent runtime and tool execution
- `crates/alms-coordinator` - Multi-agent orchestration
- `crates/alms-session` - Session management
- `crates/alms-sandbox` - WASM tool sandbox
- `crates/alms-channel` - Messaging platform adapters
- `crates/alms-cli` - Command-line interface