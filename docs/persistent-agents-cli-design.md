# Persistent Named Agents & CLI System

> **Note (2026-03-22):** The `system_prompt` field was removed from `AgentRecord` and the agent registry in PR #265. Per-agent system prompts are no longer supported; agent identity is defined through workspace files (personality.md, goals.md, etc.) instead. References to `system_prompt` below reflect the original design and are retained for historical context.

Design for making ALMS support multiple persistent agents per deployment and a full CLI for managing them.

---

## Motivation

Today ALMS has a single agent per deployment — a UUID stored in `./data/agent_id`. Subagents spawned via `invoke_agent` are ephemeral (run once, return result, gone). The CLI has three commands: `gateway`, `health`, and a `sessions` stub.

This limits ALMS to one personality, one workspace, one identity per deployment. Users can't run a "researcher" and a "coder" agent side-by-side, can't manage agents from the terminal, and can't inspect system state without the web UI.

### Goals

- Multiple named, persistent agents that survive across restarts
- Each agent has its own workspace, sessions, and optional config overrides
- Full CLI for managing agents, sessions, runs, and jobs from the terminal
- Backward-compatible: existing single-agent deployments continue to work with zero manual steps

### Non-Goals (Deferred)

- Per-agent tool restrictions (use existing capability model later)
- Persistent subagents (spawned subagents remain ephemeral)
- Agent-to-agent peer messaging (pure hierarchy stays)
- Multi-tenant / multi-user
- Agent templates or marketplace

---

## Current State

What already works in our favor:

| System | Multi-agent ready? | Details |
|--------|-------------------|---------|
| Sessions | Yes | Keyed by `(AgentId, context_id)` — `sessions` table has `agent_id` column |
| Workspace | Yes | Per-agent directory: `data/workspace/{name}/` (name-based, not UUID-based) |
| Jobs | Yes | `jobs` table has `agent_id` column |
| Agent identity | No | Single UUID from sidecar file, no registry |
| Config | No | Single `LlmConfig`, single `AgentConfig` — no per-agent overrides |
| CLI | No | Only `gateway`, `health`, `sessions` (stub) |

The missing piece: a **registry** that maps human-readable names to agent UUIDs and stores per-agent metadata.

---

## Agent Registry

### SQLite Table

```sql
CREATE TABLE IF NOT EXISTS agents (
    id            TEXT PRIMARY KEY,           -- AgentId UUID
    name          TEXT NOT NULL UNIQUE,       -- human-readable slug: "atlas", "researcher"
    description   TEXT NOT NULL DEFAULT '',
    model         TEXT,                       -- per-agent model override (NULL = server default)
    system_prompt TEXT,                       -- per-agent system prompt override
    posture       TEXT,                       -- per-agent posture override
    is_default    INTEGER NOT NULL DEFAULT 0, -- exactly one agent is the default
    created_at    TEXT NOT NULL,
    last_active   TEXT NOT NULL
);
```

### Design Decisions

1. **Name is a slug** — lowercase alphanumeric + hyphens, max 64 chars. This is what users type in the CLI and see in the UI. UUIDs remain the internal key.

2. **One default agent** — The `is_default` flag replaces the sidecar file approach. When a client doesn't specify an agent, the default is used.

3. **Sparse overrides** — `model`, `system_prompt`, `posture` are nullable. When NULL, the server-wide config from `alms.toml` applies. No full config duplication per agent.

4. **No per-agent tool config** — All agents share the same tool registry and sandbox. Per-agent tool restrictions can use the existing `Capability` model in `alms-core` later.

### Rust Types

In `alms-core`:

```rust
/// A persistent agent registered in the system.
pub struct AgentRecord {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub posture: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}
```

### Store Methods

On `SqliteStore` (in `alms-session`):

```rust
fn create_agent(&self, record: &AgentRecord) -> AlmsResult<()>;
fn get_agent_by_id(&self, id: AgentId) -> AlmsResult<Option<AgentRecord>>;
fn get_agent_by_name(&self, name: &str) -> AlmsResult<Option<AgentRecord>>;
fn get_default_agent(&self) -> AlmsResult<Option<AgentRecord>>;
fn list_agents(&self) -> AlmsResult<Vec<AgentRecord>>;
fn update_agent(&self, record: &AgentRecord) -> AlmsResult<()>;
fn delete_agent(&self, id: AgentId) -> AlmsResult<()>;
fn set_default_agent(&self, id: AgentId) -> AlmsResult<()>;
```

---

## Agent Lifecycle

### Creating an Agent

1. User runs `alms agent create --name atlas --description "Code assistant"` or `POST /agents`
2. System generates a UUID, inserts into `agents` table
3. Workspace directory `data/workspace/{name}/` is created with empty identity files (personality.md, goals.md, memories.md, user.md). CLI outputs the workspace path.
4. If `--default` flag (or first agent), set `is_default = 1` (unset any previous default)
5. On first interaction, bootstrap interview runs (existing `needs_bootstrap()` logic)

### Deleting an Agent

- `alms agent delete atlas` — removes the agent record from SQLite
- Sessions, workspace files, and audit log remain on disk (recoverable)
- `--purge` flag — also deletes workspace directory and sessions from SQLite
- Cannot delete the default agent without first setting another as default (or `--force`)

### Updating an Agent

- `alms agent config atlas --model anthropic/claude-sonnet-4-20250514 --posture guarded`
- Sets per-agent overrides; pass `--model ""` to clear an override back to server default

---

## Migration: Single-Agent to Multi-Agent

Zero manual steps for existing deployments:

1. **Schema migration** — `SqliteStore::open()` runs `CREATE TABLE IF NOT EXISTS agents`. Existing databases get the table silently.

2. **Auto-migration on startup** (in `Gateway::new()`):
   - If `agents` table is empty AND `./data/agent_id` sidecar exists → create agent `{ name: "default", id: <sidecar-uuid>, is_default: true }`
   - If `agents` table is empty AND no sidecar → create `{ name: "default", id: <new-uuid>, is_default: true }`
   - Write UUID back to sidecar for backward compat during transition

3. **Sidecar deprecation** — After migration, `./data/agent_id` is no longer the source of truth. Kept for one release, then ignored.

4. **Existing data** — Sessions, workspace, and jobs reference `AgentId` UUIDs. Since migration preserves the UUID, all existing data remains valid.

5. **`ALMS_AGENT_ID` env var** — Continues to work as an override for the default agent's UUID.

---

## CLI System

### Command Tree

```
alms gateway [--bind ADDR]                    # start the gateway server (existing)
alms health [--url URL]                       # check system health (existing)

alms agent list                               # table: name, id, model, default?, last_active
alms agent create --name NAME [OPTIONS]       # --description, --model, --posture, --default
alms agent show NAME|UUID                     # details + workspace status (bootstrapped?)
alms agent delete NAME [--purge] [--force]    # remove agent record (--purge = full cleanup)
alms agent set-default NAME                   # set the default agent
alms agent config NAME [OPTIONS]              # --model, --posture, --system-prompt

alms session list [--agent NAME]              # list sessions, optionally filtered by agent
alms session show SESSION_ID                  # session details + message count
alms session delete SESSION_ID                # delete session and messages

alms run create --session ID --input "text"   # create a run (via HTTP API to running gateway)
alms run list --session ID                    # list runs for a session
alms run show RUN_ID                          # run status + result

alms job list [--agent NAME]                  # list scheduled jobs
alms job create --agent NAME --prompt "text" --schedule "0 9 * * *"
alms job cancel JOB_ID                        # cancel a job
alms job show JOB_ID                          # job details + run history

alms dashboard                                # open http://127.0.0.1:8080 in system browser
```

### Implementation Notes

- **Read-only commands** (`list`, `show`) open `./data/alms.db` directly via `SqliteStore`. No running gateway required.
- **Write commands requiring runtime** (`run create`) connect to a running gateway via HTTP API. Clear error if gateway isn't running.
- **Output format** — Default: human-readable table. `--json` flag for machine-readable output.
- **`alms dashboard`** — Simply opens the URL in the system browser (`xdg-open` / `open` / `start`).

### Clap Structure

```rust
#[derive(Subcommand)]
enum Commands {
    Gateway { bind: String, api_key: Option<String> },
    Health { url: String },
    Agent { #[command(subcommand)] cmd: AgentCommands },
    Session { #[command(subcommand)] cmd: SessionCommands },
    Run { #[command(subcommand)] cmd: RunCommands },
    Job { #[command(subcommand)] cmd: JobCommands },
    Dashboard,
}

#[derive(Subcommand)]
enum AgentCommands {
    List,
    Create { name: String, description: Option<String>, model: Option<String>, default: bool },
    Show { name: String },
    Delete { name: String, purge: bool, force: bool },
    SetDefault { name: String },
    Config { name: String, model: Option<String>, posture: Option<String>, system_prompt: Option<String> },
}

// SessionCommands, RunCommands, JobCommands follow similar patterns
```

---

## HTTP API Additions

### New Endpoints

```
GET    /agents                      → list all agents
POST   /agents                      → create agent (body: { name, description?, model?, posture? })
GET    /agents/{id_or_name}         → get agent details
PUT    /agents/{id_or_name}         → update agent config
DELETE /agents/{id_or_name}         → delete agent
POST   /agents/{id_or_name}/default → set as default agent
```

The `{id_or_name}` path parameter accepts either a UUID or a name slug. Resolution: try UUID parse first, then name lookup.

### Existing Endpoints (unchanged)

- `POST /sessions` — already takes `agent_id`; callers can now resolve name → UUID via `GET /agents/{name}`
- `POST /runs` — session-scoped, unchanged
- `GET /agents/{agent_id}/workspace` — already exists, unchanged
- `POST /jobs` — already takes `agent_id`, unchanged

### Settings Expansion

`GET /settings` response adds an `agents` array:

```json
{
  "model": "moonshotai/kimi-k2.5",
  "agents": [
    { "name": "atlas", "id": "a1b2c3...", "is_default": true, "model": null },
    { "name": "researcher", "id": "d4e5f6...", "is_default": false, "model": "anthropic/claude-sonnet-4-20250514" }
  ],
  ...
}
```

---

## Architecture Changes by Crate

### `alms-core`
- Add `AgentRecord` struct (see above)
- Validate agent name slug: 1–64 chars, lowercase alphanumeric + hyphens, no leading/trailing hyphens

### `alms-session` (`SqliteStore`)
- Add `agents` table to SCHEMA constant
- Add CRUD methods for agent records
- Auto-migration: called from `SqliteStore::open()` — existing DBs get the table added

### `alms-gateway`
- **`gateway.rs`**: Replace single `agent_id: AgentId` field with default agent lookup from store. Keep `resolve_default_agent_id()` as fallback for auto-migration. Add auto-migration logic in `Gateway::new()`.
- **`server.rs`**: Add `/agents` route group to `protected_router()`. Add `SqliteStore` (or agent store ref) to `AppState`.
- **`runs.rs`**: In `execute_run()`, look up the agent's per-agent overrides and merge with server defaults when building `AgentConfig`.
- New file: **`agents.rs`** — route handlers for `/agents` CRUD.

### `alms-runtime`
- No changes. `AgentRuntime::new()` already takes `AgentConfig` — per-agent overrides are applied before constructing it.

### `alms-coordinator`
- No changes. Subagents remain ephemeral.

### `alms-cli`
- Expand `Commands` enum with `Agent`, `Session`, `Run`, `Job`, `Dashboard` subcommand groups.
- Read commands open SQLite directly (reuse `SqliteStore`).
- Write/runtime commands call HTTP API.

---

## UI Implications

Brief notes for future implementation:

- Agent selector dropdown in the header (next to posture badge)
- Switching agents filters sessions and shows that agent's workspace
- Agent management section in settings drawer (create/delete/configure)
- Each agent shows workspace bootstrap status

---

## Implementation Phases

### Phase 1: Agent Registry
- Add `agents` table to `SqliteStore` SCHEMA
- Add `AgentRecord` to `alms-core`
- Implement CRUD methods on `SqliteStore`
- Auto-migration logic in `Gateway::new()`
- Unit tests for all store methods

### Phase 2: HTTP API
- New route handlers for `/agents` CRUD
- `GET /settings` includes agents list
- `execute_run()` reads per-agent overrides
- Integration tests

### Phase 3: CLI Foundation
- Expand clap subcommands: `agent`, `session`, `job` groups
- Read-only commands via direct SQLite
- `alms dashboard` opens browser

### Phase 4: CLI Completeness
- `alms run create` via HTTP API
- Output formatting (table/JSON)
- Shell completions
- `alms session delete`

### Phase 5: UI Agent Switching
- Agent selector dropdown
- Agent management in settings
- Session filtering by agent
