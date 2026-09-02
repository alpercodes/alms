# ALMS — Agent Loop Management System

A self-hosted platform for running teams of LLM agents that collaborate on a project.
One Rust binary provides the HTTP/SSE gateway, the agent runtime, a sandboxed tool layer,
and SQLite persistence.

ALMS exists because most agent frameworks give you *one* agent in a loop. ALMS is built
around several: agents own persistent identity and memory, invoke each other as
subagents, message each other directly, and keep working across restarts.

## What it does

- **Agent runtime** — tool loop with a token-budgeted context builder, per-agent workspace
  files (personality, goals, memories), and cross-session episodic memory
- **Multi-agent coordination** — hierarchical subagents via `invoke_agent`, peer-to-peer
  direct messages via `send_message`, and a message bus underneath both
- **Sandboxed tools** — filesystem tools pinned to the project root by path
  canonicalization, a destructive-command classifier and configurable permission rules over
  shell commands, and Landlock confinement of shell children on Linux
- **Multi-provider LLM support** — OpenAI-compatible, Anthropic, and Gemini, including
  reasoning/thinking blocks and prompt caching
- **Gateway + web UI** — REST API with SSE streaming, and a browser UI served from the
  same binary
- **Scheduling and approvals** — cron-style jobs, plus a human-in-the-loop approval gate
  for sensitive tool calls

## Quick start

Requires Rust nightly, installed automatically from `rust-toolchain.toml`.

```bash
cargo build --release

# Start the gateway (defaults to 127.0.0.1:8080)
./target/release/alms gateway
```

The dashboard is the shortest path to a first run: it creates the session for you and
streams the agent's output live.

To drive it over the API instead — sessions are created against an agent UUID:

```bash
AGENT=$(./target/release/alms agent show atlas --json | jq -r .id)

SESSION=$(curl -sX POST http://127.0.0.1:8080/sessions \
    -H 'content-type: application/json' \
    -d "{\"agent_id\":\"$AGENT\",\"context_id\":\"cli\"}" | jq -r .session_id)

./target/release/alms run create --session "$SESSION" --input "Summarise this repo"
./target/release/alms health
```

Agent postures are `guarded` (approval required for sensitive tools), `full_control`, and
`autonomous`.

## Before you run this

ALMS executes LLM-directed tool calls on your machine. These are deliberate design
decisions, and you should know them before deploying:

- **Single-operator trust model.** No multi-user support, no privilege separation between
  agents and the operator. Do not expose the gateway to untrusted users.
- **Agents can read the secrets store.** The sandbox root is the project root, and
  `.alms/` lives inside it — so `.alms/secrets.json` is reachable via `fs_read`. Set
  `ALMS_MASTER_KEY` to encrypt that file at rest (AES-256-GCM); the daemon's shell children
  never see that variable, so an agent that reads the file gets ciphertext. Without it,
  treat any secret an agent can reach as disclosed to your model provider.
- **Sandboxing is not equal across platforms, and the gap is in `shell`.** The `fs_*` tools
  enforce the project-root boundary identically everywhere. The `shell` tool does not check
  paths in the command at all. On **Linux 5.13+** Landlock gives each shell child a
  kernel-enforced boundary; on **Windows and macOS there is no filesystem boundary on
  `shell`** — a command can read and write anything the daemon's OS user can. What remains
  there is the `[tools.shell_permissions]` regex list, the destructive-command classifier,
  and a working-directory revert that reports an escape *after* the command has already
  run. On those platforms — and on Linux below 5.13, where Landlock silently degrades to
  unsandboxed — run the daemon as a low-privilege OS user with filesystem ACLs.
- **`[security].allow_full_os_access` removes the filesystem sandbox for the agents you
  list.** It is a list of agent names, not a boolean: a listed agent's `fs_*` and `shell`
  run against the real root. Shell permissions and the destructive-command classifier still
  apply. Note that a listed name is matched — case-folded — against the name an
  `invoke_agent` call supplies, so any agent can claim it, registered or not.
- **Prompt injection is not solved.** Tool output enters the model's context; a hostile
  repository or web page can attempt to steer an agent.

Full detail in [`docs/security-model.md`](docs/security-model.md).

## Architecture

A single binary, nine crates, no cyclic dependencies.

| Crate | Responsibility |
|-------|----------------|
| `alms-core` | Shared types, layered configuration, capabilities, errors |
| `alms-gateway` | Axum HTTP server, SSE streaming, run lifecycle, web UI |
| `alms-runtime` | Agent loop, LLM clients, context builder, workspace files |
| `alms-tools` | Agent-facing tools (`send_message`, `invoke_agent`, session readers) |
| `alms-coordinator` | Multi-agent orchestration — subagent hierarchy and DM message bus |
| `alms-sandbox` | Builtin tools (shell, `fs_*`, http, math) and the tool registry |
| `alms-session` | SQLite persistence, migrations, episodic summaries |
| `alms-channel` | External transport adapters (Telegram implemented) |
| `alms-cli` | Command-line entrypoint |

See [`docs/architecture.md`](docs/architecture.md) for the full design, and
[`docs/agent-runtime-design.md`](docs/agent-runtime-design.md) for the runtime internals.

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — system design
- [`docs/api.md`](docs/api.md) — HTTP and SSE surface
- [`docs/security-model.md`](docs/security-model.md) — threat model and sandboxing
- [`docs/config.md`](docs/config.md) — configuration reference
- [`docs/engineering-reviews/`](docs/engineering-reviews/) — curated code-review threads
  from the project's history

## Development

```bash
make ci            # fmt-check + clippy + test + build-release
make test          # cargo test --all
npm ci && npm run ui:check
npm run ui:build   # rebuild the committed frontend assets
```

Contributions and workflow conventions are covered in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
