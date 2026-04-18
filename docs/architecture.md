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

### Layer 2 — Peer-to-Peer Direct Messaging (Phase 1 + DM lifecycle implemented)

Agents can send messages to any other agent by name via the `send_message` tool. Messages are delivered through a shared `MessageBus` in the Coordinator and stored in DM sessions (deterministic UUID v5 identity based on the sorted agent-name pair). The recipient's `ContextBuilder` uses perspective mapping (`build_with_perspective`) to correctly attribute messages as "self" vs "other" based on the `from_agent` metadata. This enables bidirectional collaboration without requiring a parent-child relationship.

**DM conversation lifecycle (#384):** DM conversations have an explicit start/exchange/end lifecycle:
- **Start**: First `send_message` creates the shared DM session and begins depth tracking.
- **Exchange**: Each reply increments a depth counter per DM pair (max: `MAX_DM_DEPTH` = 20). The inactivity timeout is 30 minutes (`DEPTH_EXPIRY_SECS` = 1800).
- **End**: Triggered by `ignore_message` (agent declines to reply) or depth limit exceeded. `MessageBus::end_conversation()` writes a `dm_ended` marker to the DM session, resets the depth counter, and emits a `ConversationEnded` `RunTrigger` to the peer.
- **Peer notification**: The peer receives a one-shot notification run. When the peer initiated the DM from a user-facing session, the `MessageBus` routes the notification to that source session so the user sees the response inline. When `source_session_id` is `None` (the agent was a pure DM recipient), the notification run stays on the invisible `notifications:{agent_name}` session — it is NOT rerouted to a user-facing session, to avoid polluting the web-chat. A lightweight `dm_conversation_ended` SSE event + marker message is sent to the web-chat separately by `notify_dm_ended_to_webchat`. This run does NOT include the DM addendum. The agent can then report results, update goals/memories, or take other action.
- **SSE event**: A `dm_conversation_ended` event is emitted on the DM session stream for web UI rendering.

---

## Components

### Coordinator (`alms-coordinator`)

Manages the lifecycle of subagent tasks spawned by a parent agent.

**Responsibilities:**
- Accept `invoke_agent` requests (task description, agent type, timeout, capabilities)
- Spawn a subagent `AgentRuntime` for each request
- Return the subagent's final response to the caller as a `TaskResult`
- Cancel subagents when the parent run is cancelled
**Current state:** Fully implemented. `invoke_agent` tool wired to real `AgentRuntime` loops. Supports foreground (blocking) and background (non-blocking with auto-notification) modes. Named subagents with persistent sessions via UUID v5 deterministic identity. Subagent runs are registered as proper runs visible in `GET /runs`.

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

**Context assembly order** (for all agents):
```
[System prompt] → [Episodic summaries*] → [Rolling summary*] → [Recent messages] → [Current input]
```
\* Episodic summaries injected when `run_summary_mode != off` and past session summaries exist. Rolling summary injected when strategy is `sliding-summary` and enough messages have been compressed.

**Episodic memory** (cross-session awareness):

After each successful run, the gateway may generate a per-session summary and store it in the `session_summaries` SQLite table. On subsequent runs, these summaries are loaded (excluding the current session), formatted with source labels and timestamps, and injected as a system message between the main system prompt and the session history. This gives agents awareness of what they were doing in other conversations without re-reading full transcripts.

Summary generation modes (controlled by `run_summary_mode`):
- `off` — no summaries, no episodic injection
- `heuristic` — deterministic one-liner from truncated input + output (zero LLM cost)
- `llm` (default) — rich 1-3 sentence summary via a lightweight LLM call using `session_summarizer.md` prompt

The episodic token budget (`run_summary_budget`, default: 2000) is hard-capped at 15% of `max_input_tokens` and subtracted from the total context budget so episodic content never starves the current conversation.

```
[Run completes successfully]
        │
        ▼
[generate_and_persist_summary()] (fire-and-forget tokio::spawn)
        │
        ├── derive_source_label(context_id) → skip subagent/episodic sessions
        ├── load existing summary from session_summaries table
        ├── generate new/updated summary (heuristic or LLM)
        └── upsert to session_summaries (agent_id, session_id, summary, source_label)
        
[Next run starts]
        │
        ▼
[load_episodic_summaries()] → load summaries (exclude current session)
        │
        ▼
[format_episodic_for_injection()] → token-budgeted formatted text
        │
        ▼
[build_with_perspective()] → injected as system message after main system prompt
```

### Tool Sandbox (`alms-sandbox`)

Isolated tool execution used by every agent regardless of hierarchy level.

**Built-in tools:** `echo`, `math`, `http_get`, `shell` (primary, `bash -c` command strings with persistent cwd, background execution, and 30KB output truncation; aliased as `shell_exec` for backward compatibility), `fs_read`, `fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob` (in alms-sandbox), `workspace_write` (in alms-runtime), `invoke_agent`, `read_subagent_session`, `send_message`, `list_agents`, `read_messages`, `ignore_message`, `list_my_sessions`, `read_session` (in alms-tools)

**Read-before-write guard:** `fs_write` and `fs_edit` enforce a read-before-write policy via `FileStateCache` (per-run, shared across all fs tools). Existing files must be read via `fs_read` before they can be written or edited. The guard also detects external modifications (mtime + content-hash fallback) and rejects stale writes. New file creation bypasses the guard. See `crates/alms-sandbox/src/file_state_cache.rs`.

**Sibling workspace reads (#242):** When a named agent is attached via `with_workspace()`, its read-family fs tools (`fs_read`, `fs_list`, `fs_grep`, `fs_glob`) gain an additional read-only root at the workspace parent directory, so a parent agent can read a subagent's `personality.md`/`goals.md`/`memories.md` without being able to modify them. Write-family tools (`fs_write`, `fs_edit`, `workspace_write`) stay scoped to the primary sandbox root. See the "Filesystem sandboxing" section of `docs/security-model.md` for the full trust model (including ephemeral subagent asymmetry).

**Capability inheritance:** Each subagent receives a capability set derived from the parent's `invoke_agent` call. The runtime enforces these boundaries; a subagent cannot exceed the capabilities granted to it.

### LLM Client (`alms-runtime`)

Multi-provider LLM support with streaming. Provider selected via `llm.provider` config or `ALMS_LLM_PROVIDER` env var.

**Providers:**
- **OpenAI / OpenRouter** — OpenAI-compatible chat completions API (default)
- **Anthropic** — Messages API with full streaming, tool use, and response format mapping

API keys loaded exclusively from `.alms/secrets.json` via `alms auth set`. Provider-aware key selection: each provider prefers its own key, falls back to others.

### Session Manager (`alms-session`)

Owns conversation history and workspace state. Backed by **SQLite** (`./.alms/alms.db`) for durable persistence of sessions, audit events, scheduled jobs, and the agent registry.

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
- **Episodic budget cap** — Cross-session summaries are hard-capped at 15% of `max_input_tokens` so episodic context never starves the current conversation
- **Tiered routing** — Subagents can be routed to cheaper models for simpler tasks (planned)

---

## Implementation Status

### Completed ✅
- Core types, session manager, agent runtime, WASM sandbox
- HTTP gateway with SSE streaming, approval workflow, audit log
- Built-in tools: echo, math, http_get, shell (primary name; bash -c, persistent cwd, background execution, 30KB truncation; shell_exec alias preserved), fs_read, fs_write, fs_list, fs_edit, fs_grep, fs_glob, workspace_write, invoke_agent, read_subagent_session, send_message, list_agents, read_messages, ignore_message, list_my_sessions, read_session
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
- DM conversation lifecycle: `ignore_message` and depth-exceeded trigger `end_conversation` with `dm_ended` session markers, depth counter reset, `ConversationEnded` peer notification via `notifications:{agent}` sessions, and `dm_conversation_ended` SSE events (#384 Phases 1-7)
- Cross-session episodic memory via run summaries (`session_summaries` table, heuristic + LLM modes, context injection with 15% budget cap)

### Pending 🎯
- Autonomous subagent loops — see `docs/autonomous-subagents-design.md`

---

## Code Structure

```
crates/
  alms-core/          # Shared types, errors, unified config
  alms-coordinator/   # Subagent lifecycle management (hierarchy root)
  alms-runtime/       # Agent loop (shared by all levels of hierarchy)
                      #   agent/ — AgentRuntime, loop, context building, DM helpers
                      #   context.rs — ContextBuilder (token-budgeted context window)
                      #   workspace.rs — AgentWorkspace (personality/goals/memories/user files)
                      #   workspace_tool.rs — WorkspaceWriteTool (stays here, depends on AgentWorkspace)
  alms-tools/         # Tool implementations extracted from alms-runtime
                      #   9 agent tools (send_message, invoke_agent, read_session, etc.)
                      #   SubagentDispatcher, MessageSender traits
                      #   EventForwarder trait for type-erased runtime event forwarding
  alms-session/       # Session state, SQLite persistence
  alms-sandbox/       # Tool execution, WASM sandbox, builtin tools
  alms-channel/       # User-facing adapters (Telegram, web)
  alms-gateway/       # HTTP/SSE control plane
  alms-cli/           # CLI entrypoint
```

### Dependency graph (no cycles, 9 crates)

```
alms-cli → alms-gateway → alms-runtime      → alms-core
                        → alms-tools        → alms-core
                                            → alms-session
                                            → alms-sandbox
                        → alms-coordinator  → alms-core
                                            → alms-session
                                            → alms-runtime
                                            → alms-tools
                        → alms-channel      → alms-core
                        → alms-session      → alms-core
           alms-runtime → alms-sandbox      → alms-core
                        → alms-session
         → alms-session
```

The `EventForwarder` trait in `alms-tools` enables type-erased event forwarding from subagent runs back to the gateway's SSE stream, without introducing a dependency from alms-tools to alms-runtime.

---

*Architecture Date: 2026-03-30*
*Topology: Hierarchy (invoke_agent) + Peer DM (send_message via MessageBus)*
