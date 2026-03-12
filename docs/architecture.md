# ALMS Architecture — Multi-Agent Hierarchy

## What is ALMS?

**ALMS** = **Agent Loop Management System**

A Rust-based agent platform where any agent can spawn subagents to delegate work, forming a pure tree hierarchy. Results flow up from children to parents — there is no peer-to-peer messaging between agents. Subagents can be ephemeral (one-shot) or persistent (named, with session history preserved across invocations).

---

## Core Design Principles

1. **Pure Hierarchy** — Any agent can be an orchestrator; subagents return results to their parent
2. **Workers** — Subagents do work and return a result. They can be ephemeral (one-shot, fresh session) or persistent (named, with conversation history preserved across invocations via deterministic session IDs)
3. **No Peer Messaging** — Agents do not talk to each other directly; all communication goes through the parent-child relationship
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

### Option 2 — Peer Mesh (future direction, not yet designed)

Agents form a mesh where any agent can send messages to any other agent directly, enabling bidirectional collaboration without requiring a parent-child relationship. This supports scenarios like two long-running agents coordinating on a shared task. Not planned for current implementation — noted here for future consideration.

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

**Built-in tools:** `echo`, `math`, `http_get`, `shell_exec`, `fs_read`, `fs_write`, `fs_list`, `workspace_write`, `invoke_agent`, `get_task_result`, `read_subagent_session`

**Capability inheritance:** Each subagent receives a capability set derived from the parent's `invoke_agent` call. The runtime enforces these boundaries; a subagent cannot exceed the capabilities granted to it.

### Session Manager (`alms-session`)

Owns conversation history and workspace state.

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

**Run endpoints:** `POST /runs`, `GET /runs/{id}`, `GET /runs/{id}/events`
**Coordinator endpoints (planned):** `GET /tasks`, `GET /tasks/{id}`

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

The top-level agent decides *when* and *what* to delegate. Subagents do not communicate with each other — the parent sequences or parallelizes them as it sees fit.

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
- Built-in tools: echo, math, http_get, shell_exec, fs_read, fs_write, fs_list, workspace_write, invoke_agent, get_task_result, read_subagent_session
- Cron/scheduler, SQLite persistence, web UI with agent selector
- Coordinator with real AgentRuntime loops, foreground + background subagents
- `invoke_agent` tool with `name` param for persistent subagent sessions (UUID v5 deterministic identity)
- Named subagent workspaces with registry-based config (system_prompt, model, posture)
- `alms agent create` initializes workspace directory with empty identity files
- Default system prompt includes CLI awareness (`alms --help` via shell_exec)
- Subagent SSE events forwarded into parent run stream
- `GET /tasks`, `GET /tasks/{id}` HTTP endpoints
- Agent registry with named persistent agents, per-agent config overrides
- Token-by-token SSE streaming, sliding-summary context compression

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

*Architecture Date: 2026-03-12*
*Topology: Pure hierarchy — any agent can spawn subagents, no peer-to-peer*
*Future: Option 2 (peer mesh) under consideration for long-running agent collaboration*
