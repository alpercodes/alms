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
- **Sandboxed tools** — shell and filesystem tools gated by path canonicalization, a
  built-in destructive-command classifier, configurable permission rules, and Landlock on
  Linux
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

# Store a provider key (openai, anthropic, openrouter)
./target/release/alms auth set openrouter

# Create an agent
./target/release/alms agent create atlas \
    --description "Coordinator" \
    --posture guarded

# Start the gateway (defaults to 127.0.0.1:8080)
./target/release/alms gateway

# In another shell: open the web UI
./target/release/alms dashboard
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
  `.alms/` lives inside it — so `.alms/secrets.json` is reachable via `fs_read`. Treat any
  secret an agent can reach as disclosed to your model provider.
- **Sandboxing is not equal across platforms.** Linux gets OS-level enforcement through
  Landlock. Windows and macOS get application-layer checks only — path canonicalization,
  command classification, and permission gates, with no second line of defence.
- **`[security].allow_full_os_access` disables containment.** It exists for workloads that
  need it. Setting it makes ALMS a remote code execution service aimed at your own machine.
- **Prompt injection is not solved.** Tool output enters the model's context; a hostile
  repository or web page can attempt to steer an agent.

Full detail in [`docs/security-model.md`](docs/security-model.md) and
[`SECURITY.md`](SECURITY.md).

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
