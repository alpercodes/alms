# Layer 2 -- Peer-to-Peer Messaging Between Agents

Design document for agent-to-agent communication in ALMS.

**Authors**: Heph + Atlas
**Date**: 2026-03-22
**Status**: Design (not yet implemented)
**Relates to**: `docs/product-vision-core.md` (Layer 2), `docs/architecture.md` (Option 2 -- Peer Mesh)

---

## 1. Problem Statement

ALMS currently uses a **pure hierarchy** model: agents communicate only through parent-child relationships. A parent invokes a child via `invoke_agent`, the child runs to completion and returns a result. There is no way for two agents to have an ongoing conversation, and no way for an agent to proactively reach out to another agent.

This is insufficient for the product vision. A team of agents needs:

- **Direct messaging**: Agent A sends a message to Agent B without going through a shared parent
- **Ongoing conversations**: Two agents maintain a persistent chat thread, not just request-response
- **Group discussions**: Multiple agents participate in a shared conversation (e.g., a design review involving a PM agent, a developer agent, and a reviewer agent)
- **Always-on agents**: Agents that run continuously, waiting for incoming messages (not invoked per-task)

The vision doc calls this Layer 2 -- the biggest architectural gap between the current system and the target product.

---

## 2. Current Architecture (What We Have)

### Session Model

Sessions are keyed by `(AgentId, context_id: String)`. The `context_id` is an opaque string that identifies the conversational context. For user-facing sessions, it is typically `"default"` or a Telegram chat ID. For subagent sessions, it is `"subagent_{parent_session_uuid}_{name}"`.

Sessions contain a flat list of `Message` entries with `Role` (System, User, Assistant, Tool) and `Content` (Text, ToolCall, ToolResult, Image). There is no concept of "who sent this message" beyond the role -- all User messages are assumed to come from one source.

### Coordinator (Hierarchy)

The `Coordinator` manages the `invoke_agent` flow:

1. Parent calls `invoke_agent(task, name)` tool
2. Coordinator creates a subagent `AgentRuntime` with its own session
3. The task string becomes a User message in the subagent's session
4. Subagent runs its full agent loop (multi-turn with tools)
5. Subagent's final text response returns to parent as a tool result

Named subagents have deterministic sessions (UUID v5 from parent session + name), preserving conversation history across invocations.

### Completion Notifications

Background subagents already have a notification channel: `SubagentCompletion` events flow from `Coordinator` to the gateway's `completion_notification_loop`, which creates follow-up runs on the parent session. This is the closest thing to "agent notification" in the current system.

### Key Observations

1. **Sessions are agent-centric**: A session belongs to one agent. Messages in it are either from "the user" (Role::User) or "the agent" (Role::Assistant). There is no concept of a session shared between two agents.

2. **Runs are triggered externally**: A run is created by an HTTP `POST /runs` request or by a channel adapter (Telegram). There is no way for an agent to trigger a run on another agent from within the system.

3. **The completion notification loop is already a proto-message bus**: It takes structured events from one context (subagent finishing) and creates runs in another context (parent session). This pattern -- "receive event, create run on a target session" -- is exactly what peer messaging needs.

---

## 3. Design: Agent Message Bus

### 3.1 Core Concept: Messages as Run Triggers

The fundamental insight is that **sending a message to an agent is the same as creating a run on that agent's session**. The existing run machinery (session queue, SSE streaming, context building, agent loop) handles everything after the message arrives.

A peer message from Agent A to Agent B works like this:

```
Agent A (during its agent loop)
  |
  |  calls send_message(to="agent-b", text="Please review PR #42")
  |
  v
MessageBus
  |
  |  1. Resolves Agent B's agent_id from registry
  |  2. Gets-or-creates a DM session between A and B
  |  3. Persists A's message in the DM session
  |  4. If B is always-on: triggers a run on B's DM session
  |     If B is not running: queues the message for next wakeup
  |
  v
Agent B (wakes up or is already running)
  |
  |  Sees A's message as the user input for this run
  |  Processes it, may respond (which gets persisted in the DM session)
  |  May send_message back to A or to other agents
```

### 3.2 DM Sessions (Agent-to-Agent)

A DM session between Agent A and Agent B is a regular session with a special `context_id` format:

```
context_id = "dm:{agent_a_name}:{agent_b_name}"
```

Where names are sorted alphabetically so both sides resolve to the same session:

```rust
fn dm_context_id(a: &str, b: &str) -> String {
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    format!("dm:{}:{}", first, second)
}
```

The session is **owned by the receiving agent** (the one whose run is triggered). Each side has its own session object for the same DM conversation, because sessions are keyed by `(agent_id, context_id)`. When Agent A sends to Agent B:

- Agent B's session: `(agent_b_id, "dm:agent-a:agent-b")` -- B is the agent, A's messages appear as Role::User
- Agent A's session: `(agent_a_id, "dm:agent-a:agent-b")` -- A is the agent, B's responses are stored here

Wait -- this creates two separate session histories for the same conversation, which is wrong. Let me reconsider.

**Revised approach: Shared DM sessions with sender metadata.**

A DM session is a single session owned by one agent (the "host"), with a new metadata field on messages to identify the sender:

Actually, the cleanest approach that avoids restructuring the session model is:

**Each agent has its own view of the conversation, stored in its own session.** When A sends a message to B:

1. The message is appended to **B's DM session** as a User message (because from B's perspective, A is a "user" sending input)
2. The message is also appended to **A's DM session** as an Assistant message with metadata `{forwarded: true}` (so A's context shows what it sent)
3. When B responds, B's response is appended to B's DM session as Assistant (normal) and to A's DM session as User (with metadata `{from: "agent-b"}`)

This is dual-write, but it preserves the existing session model perfectly. Each agent's session tells a coherent story from that agent's perspective, using the existing Role::User / Role::Assistant model. The LLM sees a natural conversation.

**Why this works better than a shared session:**

- No changes to SessionManager, ContextBuilder, or the agent loop
- Each agent's context window is built independently from its own session
- Token budgets, sliding-summary, and context strategies work unchanged
- The session model remains `(AgentId, context_id)` -- no new composite keys

**Trade-off:** Messages are stored twice (once per side). This is acceptable because:
- Messages are small (text + metadata)
- Consistency is maintained by the MessageBus writing both sides atomically
- Each agent can independently archive/compress its view without affecting the other

### 3.3 The MessageBus

A new component that lives in `alms-coordinator` (or a new `alms-bus` crate if needed, but `alms-coordinator` already manages inter-agent communication).

```rust
/// Agent-to-agent message bus.
///
/// Handles delivery of messages between agents, creating DM sessions
/// as needed and triggering runs on the receiving agent.
pub struct MessageBus {
    session_manager: Arc<SessionManager>,
    /// Channel to trigger runs on the gateway (reuses existing pattern).
    run_trigger_tx: mpsc::UnboundedSender<RunTrigger>,
    /// Agent registry for name resolution.
    agent_store: Arc<SqliteStore>,
}

/// A request to create a run on a target agent's session.
pub struct RunTrigger {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub input: String,
    pub source: MessageSource,
}

pub enum MessageSource {
    /// Message from another agent (peer DM).
    Agent { from_agent: AgentId, from_name: String },
    /// Message from the user.
    User,
    /// System notification.
    System,
}
```

The gateway's event loop already has `completion_notification_loop` which creates runs from `SubagentCompletion` events. The `RunTrigger` pattern generalizes this: the gateway listens on an `mpsc::UnboundedReceiver<RunTrigger>` and creates runs for any trigger, whether from a subagent completion, a peer message, or a scheduled task.

### 3.4 New Tool: `send_message`

```json
{
  "name": "send_message",
  "description": "Send a message to another agent. The target agent will receive it and may respond. Use this for peer-to-peer communication -- asking for reviews, sharing updates, requesting help.",
  "parameters": {
    "type": "object",
    "properties": {
      "to": {
        "type": "string",
        "description": "Name of the target agent (must be registered via `alms agent create`)"
      },
      "message": {
        "type": "string",
        "description": "The message to send"
      }
    },
    "required": ["to", "message"]
  }
}
```

**Implementation:**

```rust
pub struct SendMessageTool {
    bus: Arc<MessageBus>,
    sender_agent_id: AgentId,
    sender_name: String,
    sender_session_id: SessionId,
}

#[async_trait]
impl Tool for SendMessageTool {
    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let to = params["to"].as_str()...;
        let message = params["message"].as_str()...;

        let delivery = self.bus.send(
            &self.sender_name,
            self.sender_agent_id,
            self.sender_session_id,
            to,
            message,
        ).await?;

        Ok(json!({
            "delivered": true,
            "dm_session_id": delivery.recipient_session_id.0.to_string(),
            "note": "Message delivered. The recipient will process it asynchronously."
        }))
    }
}
```

The tool returns immediately (fire-and-forget from the sender's perspective). The sender does NOT block waiting for a response. If the sender wants to see the response, it reads the DM session later using a new tool or gets notified.

### 3.5 New Tool: `list_agents`

Agents need to discover who they can talk to.

```json
{
  "name": "list_agents",
  "description": "List all registered agents in the system. Returns each agent's name, description, and current status.",
  "parameters": {
    "type": "object",
    "properties": {}
  }
}
```

Returns:

```json
{
  "agents": [
    {
      "name": "reviewer",
      "description": "Code review specialist",
      "status": "idle",
      "last_active": "2026-03-22T10:00:00Z"
    },
    {
      "name": "developer",
      "description": "Full-stack developer",
      "status": "running",
      "last_active": "2026-03-22T10:15:00Z"
    }
  ]
}
```

### 3.6 New Tool: `read_messages`

Lets an agent check its DM conversation with another agent (analogous to `read_subagent_session` but for peer conversations).

```json
{
  "name": "read_messages",
  "description": "Read the conversation history with another agent. Returns recent messages from your DM session with the specified agent.",
  "parameters": {
    "type": "object",
    "properties": {
      "from": {
        "type": "string",
        "description": "Name of the agent whose DM thread to read"
      },
      "last_n": {
        "type": "integer",
        "description": "Number of recent messages to return (default: 20)"
      }
    },
    "required": ["from"]
  }
}
```

This is essentially `read_subagent_session` but with a different session key derivation (DM context_id instead of subagent context_id).

---

## 4. Always-On Agents (Daemon Agents)

### 4.1 Concept

An always-on agent is an agent that stays running indefinitely, processing incoming messages as they arrive. Unlike the current model where each run is a discrete request-response cycle, a daemon agent maintains a persistent event loop.

### 4.2 Implementation: Listener Loop

A daemon agent is not fundamentally different from a regular agent -- it is an agent with an **inbox** that triggers runs whenever a message arrives. The implementation uses the existing run infrastructure:

```
                      +-----------+
                      | MessageBus|
                      +-----+-----+
                            |
                            | RunTrigger { agent_id, session_id, input }
                            v
                      +-----+-----+
                      | Gateway    |
                      | RunTrigger |
                      | Loop       |
                      +-----+-----+
                            |
                            | enqueue to SessionQueue
                            v
                      +-----+-----+
                      | Session    |
                      | Queue      |
                      +-----+-----+
                            |
                            | execute_run()
                            v
                      +-----+-----+
                      | Agent      |
                      | Runtime    |
                      +-----+-----+
```

**There is no special "daemon loop" needed.** The existing machinery already handles:

- Serial message processing per session (SessionQueue)
- Context building from session history
- Tool execution
- SSE event streaming

What makes an agent "always-on" is simply that:

1. It is registered with `daemon: true` in the agent registry
2. The gateway starts a **listener** for it on boot
3. The listener watches for incoming RunTriggers and creates runs

The listener is a thin wrapper around the existing run creation logic.

### 4.3 Agent Registry Extension

Add a `daemon` flag to `AgentRecord`:

```sql
ALTER TABLE agents ADD COLUMN daemon INTEGER NOT NULL DEFAULT 0;
```

When `daemon = 1`:

- The gateway starts a listener for this agent on boot
- The agent gets a persistent default session that survives restarts
- Incoming messages (from peers, from the user, from scheduled tasks) all route to this session
- The agent processes messages serially through its SessionQueue

### 4.4 Agent Lifecycle

```
Gateway starts
  |
  | For each agent where daemon = true:
  |   1. Get-or-create the agent's default session
  |   2. Register a RunTrigger listener for this agent
  |   3. Log "Daemon agent '{name}' is listening"
  |
  v
Messages arrive (from peers, user, scheduler)
  |
  | RunTrigger dispatched to gateway
  | Gateway creates a run on the daemon's session
  | Agent loop processes the message
  | Response persisted to session
  |
  v
Shutdown
  |
  | Drain in-flight runs
  | Daemon agents stop listening (no special cleanup needed)
```

---

## 5. Group Sessions

### 5.1 Concept

A group session is a conversation with multiple agent participants. Any participant can send a message, and all participants see all messages.

### 5.2 Data Model

A new table for group membership:

```sql
CREATE TABLE IF NOT EXISTS agent_groups (
    id           TEXT PRIMARY KEY,     -- GroupId (UUID)
    name         TEXT NOT NULL UNIQUE, -- human-readable name
    description  TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    created_by   TEXT NOT NULL          -- AgentId of creator
);

CREATE TABLE IF NOT EXISTS group_members (
    group_id  TEXT NOT NULL REFERENCES agent_groups(id),
    agent_id  TEXT NOT NULL REFERENCES agents(id),
    joined_at TEXT NOT NULL,
    PRIMARY KEY (group_id, agent_id)
);
```

### 5.3 How Group Messages Work

When an agent sends a message to a group:

1. The message is broadcast to all OTHER members of the group
2. Each member receives the message in their own group session
3. Each member's session uses `context_id = "group:{group_name}"`
4. The sender's message appears as `Role::User` in each recipient's session, with metadata `{from: "sender-name"}`
5. Each recipient may independently choose to respond (creating a run on their group session)
6. Responses are broadcast back to all other members

**Important**: Unlike a real group chat where everyone sees the same linear thread, each agent has its own session for the group. This means:

- Each agent builds its own context window from its perspective
- Agents may respond at different times (async, not real-time round-robin)
- The "group conversation" is a logical construct -- physically it is N sessions with cross-posted messages

This is intentional: it matches how the existing session model works and avoids a shared-state coordination problem.

### 5.4 New Tools for Groups

```json
{
  "name": "create_group",
  "description": "Create a new group conversation with multiple agents.",
  "parameters": {
    "type": "object",
    "properties": {
      "name": { "type": "string", "description": "Group name (lowercase, hyphens, like agent names)" },
      "members": { "type": "array", "items": { "type": "string" }, "description": "Agent names to add as members" },
      "description": { "type": "string", "description": "What this group is for" }
    },
    "required": ["name", "members"]
  }
}
```

```json
{
  "name": "send_group_message",
  "description": "Send a message to a group conversation. All members will receive it.",
  "parameters": {
    "type": "object",
    "properties": {
      "group": { "type": "string", "description": "Group name" },
      "message": { "type": "string", "description": "The message to send" }
    },
    "required": ["group", "message"]
  }
}
```

---

## 6. How This Integrates with Existing Systems

### 6.1 Coexistence with invoke_agent

`invoke_agent` (the hierarchy model) and `send_message` (the peer model) coexist. They serve different purposes:

| | `invoke_agent` | `send_message` |
|---|---|---|
| **Semantics** | "Do this task and return the result" | "Here is information / a request" |
| **Blocking** | Yes (foreground) or poll (background) | No -- fire and forget |
| **Session** | Subagent session (child of parent) | DM session (peer-to-peer) |
| **Result** | Returned as tool result to caller | Recipient may or may not respond |
| **Use case** | Delegation, task decomposition | Collaboration, notification, discussion |

`invoke_agent` is for **vertical** communication (boss to worker). `send_message` is for **horizontal** communication (peer to peer).

They should NOT be merged. A developer agent asking a reviewer agent for a review should use `send_message`. A PM agent assigning a task to a developer agent should use `invoke_agent`. The LLM will learn the distinction through system prompts and experience.

### 6.2 SSE Streaming

Peer messages integrate with existing SSE infrastructure:

- Each DM/group session is a regular session with its own SessionId
- Runs on these sessions emit SSE events through the existing `RunManager`
- The UI can subscribe to any session's event stream, including DM sessions
- A new SSE event type `message_received` notifies the UI when an agent gets a peer message

### 6.3 Context Building

No changes needed. Each agent's DM session is a regular session. ContextBuilder reads from it using the standard strategies (truncate, full, sliding-summary). The only addition is metadata on messages indicating the sender (`from` field), which the system prompt instructs the agent to use.

### 6.4 User Observation

The user needs to observe agent-to-agent conversations. This is handled by:

1. **Session listing**: `GET /sessions` (or a new filtered endpoint) returns DM and group sessions alongside user sessions. The UI shows them in a sidebar or dedicated panel.
2. **Session SSE**: The user subscribes to a DM session's event stream to watch it in real-time.
3. **Message history**: `GET /sessions/{id}/messages` returns the conversation history.

No new endpoints are needed for observation -- the existing session/run/SSE infrastructure handles it.

---

## 7. Database Schema Changes

### 7.1 New Tables

```sql
-- Groups for multi-agent conversations
CREATE TABLE IF NOT EXISTS agent_groups (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    created_by   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS group_members (
    group_id  TEXT NOT NULL REFERENCES agent_groups(id),
    agent_id  TEXT NOT NULL REFERENCES agents(id),
    joined_at TEXT NOT NULL,
    PRIMARY KEY (group_id, agent_id)
);
```

### 7.2 Schema Migrations to Existing Tables

```sql
-- Add daemon flag to agents table
ALTER TABLE agents ADD COLUMN daemon INTEGER NOT NULL DEFAULT 0;

-- Add sender metadata to messages (optional -- can also use the existing
-- metadata JSON column, which already exists but is rarely used)
-- No schema change needed: metadata is already TEXT (JSON).
```

### 7.3 Message Metadata Convention

Use the existing `metadata` JSON column on messages to track sender identity in DM/group contexts:

```json
{
  "from_agent": "reviewer",
  "from_agent_id": "uuid-here",
  "message_type": "dm"
}
```

This avoids adding new columns to the messages table. The metadata column is already defined, persisted, and loaded.

---

## 8. API Changes

### 8.1 New Endpoints

```
POST   /messages              -- Send a message from one agent to another (or to a group)
GET    /agents/{name}/inbox   -- List unread/recent messages for an agent
POST   /groups                -- Create a group
GET    /groups                -- List groups
GET    /groups/{name}         -- Get group details + members
POST   /groups/{name}/members -- Add a member to a group
DELETE /groups/{name}/members/{agent} -- Remove a member
```

### 8.2 Modified Endpoints

```
GET /sessions -- Add filter params: ?type=dm|group|user to filter session types
GET /agents   -- Add daemon flag to response, add status (idle/running/listening)
```

### 8.3 New SSE Event Types

```json
// Emitted on the recipient's session when a peer message arrives
{
  "event": "message_received",
  "data": {
    "from_agent": "reviewer",
    "session_id": "...",
    "preview": "First 200 chars of the message..."
  }
}
```

---

## 9. Crate-Level Changes

### alms-core

- New `GroupId` newtype wrapper
- New `AgentGroup` / `GroupMember` types
- Add `daemon` field to `AgentRecord` / `CreateAgentRequest` / `UpdateAgentRequest`
- Add `MessageSource` enum (Agent, User, System)

### alms-coordinator

- New `MessageBus` struct with `send()`, `send_group()`, `create_group()` methods
- New `RunTrigger` type
- `Coordinator` gains a reference to `MessageBus` (or `MessageBus` wraps `Coordinator`)

### alms-runtime

- New `SendMessageTool` (requires `Arc<MessageBus>`)
- New `ListAgentsTool` (requires `Arc<SqliteStore>`)
- New `ReadMessagesTool` (requires `Arc<SessionManager>`)
- New `CreateGroupTool` (requires `Arc<MessageBus>`)
- New `SendGroupMessageTool` (requires `Arc<MessageBus>`)
- Register these tools when the runtime has a MessageBus attached

### alms-gateway

- Generalize `completion_notification_loop` into `run_trigger_loop` that handles `RunTrigger` events
- New route handlers for `/messages`, `/groups`
- On startup: start listeners for daemon agents
- Pass `MessageBus` into `AppState`

### alms-session

- New `SqliteStore` methods: `create_group()`, `load_group()`, `list_groups()`, `add_group_member()`, `remove_group_member()`, `list_group_members()`
- Add `daemon` column handling in `create_agent()` / `load_agent_*()` / `save_agent()`

### alms-cli

- `alms agent create --daemon` flag
- `alms group {create, list, show, add-member, remove-member}` commands
- `alms message send --from <agent> --to <agent> <text>` command (for manual testing)

---

## 10. Security Considerations

### 10.1 Access Control

**Phase 1 (open):** Any agent can message any other agent. This is simple and sufficient for small teams where all agents are trusted.

**Phase 2 (capabilities):** Extend the capability system to include messaging permissions:
- `can_message: ["reviewer", "developer"]` -- agent can only message these agents
- `can_join_groups: true/false`
- `can_create_groups: true/false`

### 10.2 Message Rate Limiting

Agents could flood each other with messages, creating infinite loops:
- Agent A sends to Agent B
- Agent B's run sends back to Agent A
- Agent A's run sends back to Agent B
- ... (infinite loop burning tokens)

**Mitigations:**
1. **Per-session rate limit**: Max N runs per minute per session (configurable, default: 10)
2. **DM cooldown**: After sending a message, an agent cannot send another message to the same recipient for T seconds (default: 5)
3. **Max message depth**: Track how many times a message has been "forwarded" (A->B->A->B...). After depth N (default: 5), delivery is refused with an error
4. **Token budget per DM pair per hour**: Configurable limit on total tokens spent on a DM conversation

### 10.3 User Override

The user (via the API or UI) can:
- Pause a daemon agent (stop processing incoming messages)
- Mute a DM session (messages still arrive but don't trigger runs)
- Kill a runaway conversation loop
- Set rate limits per agent

---

## 11. Implementation Phases

Each phase delivers independent value and is a PR-sized chunk.

### Phase 1: MessageBus + send_message tool (DM only)

**Goal**: Agent A can send a message to Agent B. B receives it as a run input.

**Changes:**
- `alms-coordinator`: Add `MessageBus` with `send()` method. DM session derivation (`dm:a:b`). Dual-write to sender and recipient sessions.
- `alms-coordinator`: Add `RunTrigger` type and `mpsc` channel.
- `alms-gateway`: Generalize `completion_notification_loop` into `run_trigger_loop`. Wire `MessageBus` into `AppState`.
- `alms-runtime`: Add `SendMessageTool`. Register in runtime when MessageBus is available.
- `alms-runtime`: Add `ListAgentsTool` (reads from `SqliteStore`).
- `alms-runtime`: Add `ReadMessagesTool` (reads DM session history).
- `alms-session`: No schema changes (DM sessions are regular sessions with a `dm:*` context_id).

**Tests:**
- Unit: MessageBus send/receive, DM context_id derivation, dual-write consistency
- Integration: Agent A sends to Agent B, B's session gets a run with the message

**Estimated size:** ~400 lines of new code, ~8 tests.

### Phase 2: Daemon agents (always-on listeners)

**Goal**: Agents can be marked as daemons. They stay "listening" and process incoming messages automatically.

**Changes:**
- `alms-core`: Add `daemon` field to `AgentRecord`
- `alms-session/sqlite`: ALTER TABLE to add `daemon` column. Modify CRUD methods.
- `alms-gateway`: On startup, find all daemon agents and start listeners. Listeners watch for `RunTrigger` events targeted at their agent.
- `alms-cli`: `alms agent create --daemon` flag. `alms agent update --daemon true/false`.
- Rate limiting: Per-session rate limiter in `run_trigger_loop`.

**Tests:**
- Daemon agent starts listening on boot
- Rate limiting rejects excessive messages
- Daemon flag persists across restarts

**Estimated size:** ~250 lines, ~6 tests.

### Phase 3: HTTP API for messaging

**Goal**: External systems (UI, CLI, other services) can send messages between agents via HTTP.

**Changes:**
- `alms-gateway`: `POST /messages` endpoint
- `alms-gateway`: `GET /agents/{name}/inbox` endpoint (recent messages in DM sessions)
- `alms-gateway`: Add `message_received` SSE event type for real-time UI updates
- `alms-cli`: `alms message send` command

**Tests:**
- HTTP round-trip: POST message, verify it appears in recipient's session
- SSE: subscriber receives `message_received` event

**Estimated size:** ~200 lines, ~4 tests.

### Phase 4: Group sessions

**Goal**: Agents can create groups and send messages to all members.

**Changes:**
- `alms-core`: `GroupId`, `AgentGroup`, `GroupMember` types
- `alms-session/sqlite`: `agent_groups` + `group_members` tables, CRUD methods
- `alms-coordinator/message_bus`: `send_group()`, `create_group()` methods
- `alms-runtime`: `CreateGroupTool`, `SendGroupMessageTool` tools
- `alms-gateway`: `/groups` CRUD endpoints
- `alms-cli`: `alms group {create, list, show, add-member, remove-member}` commands

**Tests:**
- Group creation and membership
- Message broadcast to all members
- Member leaves group, stops receiving messages

**Estimated size:** ~500 lines, ~10 tests.

### Phase 5: Loop prevention + rate limiting

**Goal**: Prevent infinite message loops and token burn.

**Changes:**
- `alms-coordinator/message_bus`: Message depth tracking (header on each message)
- `alms-coordinator/message_bus`: Per-DM-pair rate limiter
- `alms-coordinator/message_bus`: Per-agent-per-hour token budget for DM/group runs
- `alms-gateway`: User controls: pause agent, mute session

**Tests:**
- A->B->A->B loop is stopped at max depth
- Rate limiter rejects after N messages/minute
- Paused agent does not process messages

**Estimated size:** ~300 lines, ~8 tests.

### Phase 6: UI integration

**Goal**: The web UI shows DM sessions, group sessions, and agent-to-agent conversations alongside user sessions.

**Changes:**
- UI: Session sidebar shows DM and group sessions with agent avatars
- UI: "Send message to agent" action from the agent panel
- UI: Group management panel
- UI: Real-time message indicators (using `message_received` SSE event)

This phase is UI-only (HTML/JS/CSS) -- no Rust changes.

---

## 12. Migration Path from Current State

The transition from pure hierarchy to peer messaging is additive -- nothing is removed or changed in the existing system.

| Current feature | After Layer 2 |
|---|---|
| `invoke_agent` tool | Unchanged. Still works for delegation/task decomposition. |
| `read_subagent_session` tool | Unchanged. Still reads subagent sessions. |
| `get_task_result` tool | Unchanged. Still polls background subagent tasks. |
| Subagent sessions (`subagent_*` context_id) | Unchanged. New DM sessions use `dm:*` context_id. |
| Completion notification loop | Generalized into `run_trigger_loop` that handles all RunTrigger types. |
| SessionManager / SQLite store | No breaking changes. New DM sessions are regular sessions. |
| Agent registry | `daemon` column added with `DEFAULT 0` -- existing agents unaffected. |

**Backward compatibility:** All existing agent behavior continues to work. Peer messaging is opt-in -- agents only get `send_message` and related tools when the MessageBus is configured. Agents that don't use peer messaging (e.g., CLI-invoked subagents) are unaffected.

---

## 13. Open Questions and Future Directions

### 13.1 Response Notification

When Agent B responds to Agent A's message, should Agent A be automatically notified? Options:

- **Option A (pull):** Agent A periodically calls `read_messages(from="agent-b")` to check for responses. Simple, but wastes LLM iterations.
- **Option B (push):** Like background subagent completion, inject B's response into A's next context build. More complex, but more efficient.
- **Recommendation:** Start with Option A (pull via `read_messages`). Add Option B later if the polling overhead is significant.

### 13.2 Message Delivery Guarantees

What happens if the gateway restarts while a message is in the `RunTrigger` channel?

- **Current approach:** `mpsc` channel is in-memory. Messages in-flight during restart are lost.
- **Future:** Persist RunTriggers to SQLite before processing. On restart, replay unprocessed triggers.
- **Recommendation:** Accept in-memory delivery for Phase 1. The risk is low (single-process, restarts are rare) and the fix is straightforward when needed.

### 13.3 Message Ordering in Groups

Group messages are delivered independently to each member. Members process them at different speeds. This means:
- Member A might see message 1, 2, 3 in order
- Member B might see message 1, 3, 2 (if message 3 was delivered before it finished processing message 2's run)

This is acceptable for async agent communication. If strict ordering is needed, the SessionQueue already ensures serial processing per session.

### 13.4 Relationship to Layer 3

Layer 3 (emergent team dynamics) builds on Layer 2 infrastructure:

- **Scheduled standups:** A cron job triggers a `send_group_message` to the "daily-standup" group
- **Auto-review requests:** The developer agent's system prompt instructs it to call `send_message(to="reviewer", ...)` after opening a PR
- **PM escalation:** The PM agent subscribes to all group channels and intervenes when it detects blockers

Layer 2 provides the pipes. Layer 3 provides the behavior.

---

## 14. Summary

Layer 2 adds peer-to-peer communication to ALMS through five key components:

1. **MessageBus** -- routes messages between agents, creating DM sessions and triggering runs
2. **send_message / read_messages tools** -- agents interact with peers through familiar tool calls
3. **Daemon agents** -- always-on agents that listen for incoming messages
4. **Group sessions** -- multi-agent conversations with broadcast delivery
5. **RunTrigger generalization** -- the existing completion notification pattern becomes a universal message delivery mechanism

The design is intentionally conservative: it extends the existing session model rather than replacing it, reuses the run/SSE/context infrastructure, and keeps each phase independently deployable. The biggest change is conceptual (agents can now talk to each other) rather than architectural (the plumbing is mostly reuse).

---

*Design Date: 2026-03-22*
*Authors: Heph + Atlas*
*Status: Proposed -- pending review*
