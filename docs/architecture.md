# ALMS Architecture — Multi-Agent Hierarchy

## What is ALMS?

**ALMS** = **Agent Loop Management System**

A Rust-based agent platform with two communication layers: (1) vertical delegation via `invoke_agent`, where any agent can spawn subagents to delegate work, forming a tree hierarchy with results flowing up from children to parents; and (2) peer-to-peer direct messaging via `send_message`, where any agent can send messages to any other agent by name through a shared MessageBus. Subagents can be ephemeral (one-shot) or persistent (named, with session history preserved across invocations).

---

## Core Design Principles

1. **Hierarchy + Peer Messaging** — Any agent can be an orchestrator via `invoke_agent` (vertical delegation); agents can also communicate directly via `send_message` (peer-to-peer DM through a shared MessageBus)
2. **Workers** — Subagents do work and return a result. They can be ephemeral (one-shot, fresh session) or persistent (named, with conversation history preserved across invocations via deterministic session IDs)
3. **Two Communication Layers** — Layer 1: parent-child delegation via `invoke_agent` (blocking/background). Layer 2: peer-to-peer direct messaging via `send_message` (asynchronous, delivered into recipient's next context window via DM sessions with perspective mapping)
4. **Explicit over Implicit** — Clear task boundaries, observable handoffs via SSE
5. **Security First** — Capability-based permissions, strict sandboxing

---

## Multi-Agent Topology

### Option 3 — Pure Hierarchy (current design)

Any agent running inside ALMS can call the `invoke_agent` tool to spawn a subagent. The subagent executes its task independently and returns its result as a tool call response. The parent receives the result and continues its own loop. Subagents can themselves spawn subagents, creating an arbitrary-depth tree.

```
[User] ──► [Agent A]
                │
                ├── invoke_agent → [Subagent B]
                │                       │
                │                       └── returns result ──► [Agent A continues]
                │
                └── invoke_agent → [Subagent C]
                                        │
                                        ├── invoke_agent → [Subagent D]
                                        │                       │
                                        │                       └── returns result ──► [C continues]
                                        │
                                        └── returns result ──► [Agent A continues]
```

**Key properties:**
- No special "Main Agent" type — any agent instance can orchestrate
- Subagents can be **ephemeral** (fresh session per invocation) or **persistent** (named — same `name` reuses session, preserving conversation history via UUID v5 deterministic identity)
- Results propagate up the tree via tool responses
- Cancellation cascades downward (cancel parent → cancel all children)
- Each subagent has its own tool registry, context window, and system prompt
- Subagent runs a full `agent_loop` (multi-iteration tool use, up to `max_iterations`)

### Layer 2 — Peer-to-Peer Direct Messaging (Phase 1 implemented)

Agents can send messages to any other agent by name via the `send_message` tool. Messages are delivered through a shared `MessageBus` in the Coordinator and stored in DM sessions (deterministic UUID v5 identity based on the sorted agent-name pair). The recipient's `ContextBuilder` uses perspective mapping (`build_with_perspective`) to correctly attribute messages as "self" vs "other" based on the `from_agent` metadata. This enables bidirectional collaboration without requiring a parent-child relationship.

---

## Components

### Coordinator (`alms-coordinator`)

Manages the lifecycle of subagent tasks spawned by a parent agent.

**Responsibilities:**
- Accept `invoke_agent` requests (task description, agent type, timeout, capabilities)
- Spawn a subagent `AgentRuntime` for each request
- Return the subagent's final response to the caller as a `TaskResult`
- Cancel subagents when the parent run is cancelled
- Expose active tasks via `GET /tasks` and `GET /tasks/{id}`

**Current state:** Fully implemented. `invoke_agent` and `get_task_result` tools wired to real `AgentRuntime` loops. Supports foreground (blocking) and background (non-blocking with poll) modes. Named subagents with persistent sessions via UUID v5 deterministic identity.

```
[Parent AgentRuntime]
        │  invoke_agent tool call
        ▼
[Coordinator::spawn_subagent()]
        │
        ▼
[Subagent AgentRuntime] ──► runs its own agent loop ──► returns TaskResult
        │
        ▼ (forwarded back as tool result)
[Parent AgentRuntime continues]
```

### Agent Runtime (`alms-runtime`)

Executes agent loops for all agents — both top-level (user-facing) and subagents. There is no separate "Main Agent" implementation; the same `AgentRuntime` is used at every level of the hierarchy.

**Agent loop:**
```
Assemble context → LLM call → Parse response →
  If tool calls: execute tools (including invoke_agent) → loop
  If final reply: emit run_finished → stop
```

**Subagent loop (same, different inputs):**
```
Receive task description as initial user message →
Assemble minimal context (task-specific system prompt + capabilities) →
LLM call → ... → emit result → stop
```

### Tool Sandbox (`alms-sandbox`)

Isolated tool execution used by every agent regardless of hierarchy level.

**Built-in tools:** `echo`, `math`, `http_get`, `shell_exec`, `fs_read`, `fs_write`, `fs_list`, `workspace_write`, `invoke_agent`, `get_task_result`, `read_subagent_session`, `send_message`

**Capability inheritance:** Each subagent receives a capability set derived from the parent's `invoke_agent` call. The runtime enforces these boundaries; a subagent cannot exceed the capabilities granted to it.

### LLM Client (`alms-runtime`)

Multi-provider LLM support with streaming. Provider selected via `llm.provider` config or `ALMS_LLM_PROVIDER` env var.

**Providers:**
- **OpenAI / OpenRouter** — OpenAI-compatible chat completions API (default)
- **Anthropic** — Messages API with full streaming, tool use, and response format mapping

API keys loaded from env vars only (`OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `ANTHROPIC_API_KEY`). Provider-aware key selection: each provider prefers its own key, falls back to others.

### Session Manager (`alms-session`)

Owns conversation history and workspace state. Backed by **SQLite** (`./data/alms.db`) for durable persistence of sessions, audit events, scheduled jobs, and the agent registry.

**Hierarchy:**
```
session:{parent_id}
  └── subagent_session:{task_id}    (child session, created per subagent spawn)
      └── subagent_session:{task_id}  (grandchild, if subagent spawns further)
```

- Parent sessions track active child task IDs
- Cancellation cascades: cancelling a parent cancels all descendants
- Token usage is aggregated at each level and rolled up to the root

### Gateway (`alms-gateway`)

HTTP/SSE control plane. Handles top-level user interactions and exposes coordinator state.

**Run endpoints:** `POST /runs`, `GET /runs/{id}`, `GET /runs/{id}/events`, `POST /runs/{id}/cancel`
**Agent endpoints:** `GET/POST /agents`, `GET/PUT/DELETE /agents/{id_or_name}`, `POST /agents/{id_or_name}/default`
**Workspace endpoints:** `GET /agents/{id_or_name}/workspace`, `PUT /agents/{id_or_name}/workspace/{file}`
**Task endpoints:** `GET /tasks`, `GET /tasks/{id}`
**Other:** `GET /settings`, `GET /audit`, `POST /jobs`, `GET /jobs/{id}`, `GET /sessions`, `GET /health`

**SSE event propagation:** Subagent `tool_start`/`tool_end`/`progress` events are forwarded into the parent run's SSE stream so the UI can show subagent activity inline.

### Channel Adapters (`alms-channel`)

User-facing interfaces (Telegram, web UI) connect only to top-level runs. Subagent activity is surfaced through the parent's event stream — subagents are never directly addressable by users.

---

## Message Flow Example

**User:** "Build me a Rust web server with JWT auth"

```
[User] ──► [Top-level Agent]
                │
                │  Decides to delegate:
                ├── invoke_agent("Design the API schema") ──► [Subagent: Design]
                │                                                   │ returns OpenAPI spec
                │                                                   ▼
                ├── invoke_agent("Implement auth middleware") ──► [Subagent: Auth]
                │   (receives spec as context)                       │ returns auth code
                │                                                   ▼
                └── Synthesizes results: "Here's your server with JWT auth..."
                    │
[User] ◄────────────┘
```

The top-level agent decides *when* and *what* to delegate. Subagents can also communicate with each other directly via `send_message` for peer coordination, in addition to the parent sequencing or parallelizing them.

---

## Token Efficiency

Token cost is a first-class constraint:

- **Minimal subagent context** — Subagents receive a task-specific system prompt, not the full agent persona
- **Context compression** — `ContextBuilder` with `truncate` (default) and `sliding-summary` strategies
- **Usage tracking** — `prompt_tokens` + `completion_tokens` accumulated per run, including subagent usage rolled up to parent
- **Cost observability** — `run_finished` SSE event and `GET /runs/{id}` expose per-run token counts
- **Tiered routing** — Subagents can be routed to cheaper models for simpler tasks (planned)

---

## Implementation Status

### Completed ✅
- Core types, session manager, agent runtime, WASM sandbox
- HTTP gateway with SSE streaming, approval workflow, audit log
- Built-in tools: echo, math, http_get, shell_exec, fs_read, fs_write, fs_list, workspace_write, invoke_agent, get_task_result, read_subagent_session, send_message
- Cron/scheduler, SQLite persistence, web UI with agent selector
- Coordinator with real AgentRuntime loops, foreground + background subagents
- `invoke_agent` tool with `name` param for persistent subagent sessions (UUID v5 deterministic identity)
- Named subagent workspaces with registry-based config (model, posture)
- `alms agent create` initializes workspace directory with empty identity files
- Default system prompt includes CLI awareness (`alms --help` via shell_exec)
- Subagent SSE events forwarded into parent run stream
- `GET /tasks`, `GET /tasks/{id}` HTTP endpoints
- Agent registry with named persistent agents, per-agent config overrides
- Token-by-token SSE streaming, sliding-summary context compression
- Peer-to-peer direct messaging via `send_message` tool + MessageBus + DM sessions with perspective mapping (Layer 2 Phase 1)

### Pending 🎯
- Autonomous subagent loops — see `docs/autonomous-subagents-design.md`

---

## Code Structure

```
crates/
  alms-core/          # Shared types, errors, unified config
  alms-coordinator/   # Subagent lifecycle management (hierarchy root)
  alms-runtime/       # Agent loop (shared by all levels of hierarchy)
  alms-session/       # Session state, SQLite persistence
  alms-sandbox/       # Tool execution, WASM sandbox, builtin tools
  alms-channel/       # User-facing adapters (Telegram, web)
  alms-gateway/       # HTTP/SSE control plane
  alms-cli/           # CLI entrypoint
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

---

*Architecture Date: 2026-03-22*
*Topology: Hierarchy (invoke_agent) + Peer DM (send_message via MessageBus)*
