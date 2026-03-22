# Layer 2 -- Peer-to-Peer Messaging Between Agents

Design document for agent-to-agent communication in ALMS.

**Authors**: Heph + Atlas
**Date**: 2026-03-22
**Status**: Design (not yet implemented)
**Relates to**: `docs/product-vision-core.md` (Layer 2), `docs/communication-architecture.md` (product vision), `docs/architecture.md` (Option 2 -- Peer Mesh)

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

A DM conversation between two agents works exactly like a user↔agent chat does today — **one shared session** with a persistent conversation history. When an agent receives a message, it sees the full chat history (subject to the same context strategies: truncate, full, sliding-summary).

The DM session is owned by the **receiving agent** at any given moment. When Agent A sends a message to Agent B:

1. The MessageBus resolves the DM session: `(agent_b_id, "dm:agent-a:agent-b")`
2. A's message is appended as `Role::User` (from B's perspective, A is the "user")
3. A `RunTrigger` is created on B's DM session
4. B's agent loop runs, sees the full DM conversation history, and responds
5. B's response is appended as `Role::Assistant` (normal agent response)

When B wants to reply back to A, the same flow reverses:

1. B calls `send_message(to="agent-a", message="...")`
2. The MessageBus resolves the DM session: `(agent_a_id, "dm:agent-a:agent-b")`
3. B's message is appended as `Role::User` in A's DM session
4. A `RunTrigger` is created on A's DM session
5. A processes it with full DM history

**Key point:** Each agent has its own DM session for the same conversation (`agent_a_id + context_id` vs `agent_b_id + context_id`). Messages are written to the **receiving** agent's session only. The sending agent's record of what it sent is in its own session's assistant responses (since it called `send_message` as a tool during its run).

This means:
- No dual-write complexity — messages are written once, to the recipient's session
- Context building works unchanged (ContextBuilder reads the agent's session as normal)
- Sliding-summary and token budgets work per-agent as they do today
- The existing session model `(AgentId, context_id)` is unchanged

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

### 3.4 Hybrid Messaging: Structured Data vs Natural Language

Per `communication-architecture.md`, not every agent interaction requires an LLM call. Messages are split into two types:

**Structured messages** (no LLM required): routine, predictable data passed as typed JSON.
- PR metadata (changed files, diff stats, branch info)
- Test results (pass/fail, coverage)
- Build status, merge readiness signals
- Task status updates (in progress, blocked, done)

**Natural language messages** (LLM required): interactions that need reasoning.
- Code review discussions
- Design decision debates
- Meeting contributions
- Situations requiring explanation or judgment

The `send_message` tool supports both via an optional `structured` parameter:

```rust
pub enum MessagePayload {
    /// Natural language — triggers a full agent run on the recipient.
    Text(String),
    /// Structured data — appended to session as a system message,
    /// may or may not trigger a run depending on the recipient's config.
    Structured { kind: String, data: Value },
}
```

Structured messages are cheaper (no LLM call needed to "read" them — they go into context as data), easier to search/filter, and render natively in the UI as cards/badges.

### 3.5 @-Mention Routing in Group Chats

Group messages support @-mention routing (per `communication-architecture.md` Section 6.2):

- `@agentname` — only that agent is invoked (others see the message in their session but don't trigger a run)
- `@agentname @agentname2` — multiple specific agents invoked
- `@everyone` — all group members invoked
- No tag — message is logged to all sessions but no agent is invoked

The MessageBus parses mentions from the message text and only creates `RunTrigger` events for mentioned agents. Non-mentioned agents still receive the message in their group session (for context continuity) but their SessionQueue is not triggered.

Agents are informed about the mention system via their system prompt.

### 3.6 Ignore Signal

Agents can decline to respond to an invocation (per `communication-architecture.md` Section 7). When an agent receives a message (especially from an `@everyone` group mention), it evaluates whether it has something meaningful to contribute. If not, it returns a built-in ignore signal instead of a response.

**Implementation**: A special tool `ignore_message` that the agent can call during its run:

```json
{
  "name": "ignore_message",
  "description": "Decline to respond to this message. Use when you have nothing meaningful to add.",
  "parameters": {
    "type": "object",
    "properties": {
      "reason": { "type": "string", "description": "Brief reason for ignoring (logged, not sent)" }
    }
  }
}
```

When called, the run ends early. The ignore is logged but no response is broadcast. The system prompt instructs agents on when this is appropriate.

**Future optimization**: A cheaper pre-filter (smaller model or rule-based check) before the full LLM call to avoid paying for the ignore decision itself. Deferred — build the expensive correct version first.

### 3.7 New Tool: `send_message`

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

**Note:** The `send_message` tool currently only supports natural language (text) messages. Structured message support (Section 3.4, `MessagePayload::Structured`) will be added in a later phase — likely as an optional `structured` JSON parameter on the tool, or as a separate `send_structured_message` tool.

### 3.8 New Tool: `list_agents`

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

### 3.9 New Tool: `read_messages`

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

This is essentially `read_subagent_session` but with a different session key derivation (DM context_id instead of subagent context_id). **Implementation note:** `read_messages` and `read_subagent_session` should share their core logic (load session by context_id, format messages, optional summary). Consider refactoring into a shared `read_agent_session` helper that both tools call with different context_id derivations.

---

## 4. Always-On Agents (Daemon Agents)

### 4.1 Concept

An always-on agent is an agent that the gateway keeps "warm" — ready to process incoming messages immediately.

**Important clarification:** The `RunTrigger` mechanism (Phase 1) creates runs for ANY agent, daemon or not. A non-daemon agent can still receive peer messages — they create runs just like a user's POST /runs does. The `daemon` flag adds:

1. **Boot-time readiness**: The gateway pre-creates the agent's default session on startup
2. **UI semantics**: The agent shows as "listening" in the UI, distinct from idle agents
3. **Intent signal**: Tells the system this agent is designed to receive unsolicited messages

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

Group chats work like DM chats — each agent has a regular session for the group, and sees the full conversation history when it's their turn to respond.

When an agent sends a message to a group:

1. The message is appended as `Role::User` to every OTHER member's group session (`context_id = "group:{group_name}"`) with metadata `{from: "sender-name"}`
2. For @-mentioned agents (or `@everyone`): a `RunTrigger` is created on their group session
3. Non-mentioned agents receive the message in their session (for context continuity) but no run is triggered
4. When a mentioned agent responds, their response is broadcast to all other members' sessions as `Role::User` with `{from: "responder-name"}`

Each agent has its own session for the group (`(agent_id, "group:{group_name}")`). When it's an agent's turn to respond, it sees the full group conversation history — all messages from all participants — just like a user↔agent chat. The ContextBuilder handles it normally with truncation/sliding-summary as needed.

Agents respond async — they process messages through their `AgentQueue` (Section 7) one at a time, so group participation is serialized with all their other work.

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

## 6. Team Meetings (Layer 2.5 / Layer 3 Bridge)

Per `communication-architecture.md`, meetings are the primary mechanism for team coordination. They bridge Layer 2 (communication pipes) and Layer 3 (emergent team dynamics).

### 6.1 Meeting Model

A meeting is a **structured group conversation** with:
- A **facilitator** (manager role agent) who initiates, drives agenda, and concludes
- A set of **participants** (all team members, or a subset)
- A **summary block** prepended as context (previous meeting summary + current project stats)
- A **maximum round count** to prevent infinite discussion
- A **generated summary** at conclusion that feeds forward to the next meeting

### 6.2 Meeting Lifecycle

```
Manager agent initiates meeting (scheduled or ad-hoc)
  |
  | Context block prepended: previous meeting summary + project stats
  v
Round 1: Manager sets agenda, @everyone
  |
  | Each agent responds (or ignores via ignore_message)
  v
Round 2-N: Discussion, facilitated by manager
  |
  | Manager directs questions: @developer, @reviewer
  | Agents respond, use ignore signal if nothing to add
  v
Manager ends meeting (or hard cap hit)
  |
  | System generates meeting summary
  | Summary persisted → feeds into next meeting's context block
```

### 6.3 Implementation

Meetings are built on top of group sessions (Phase 4) with additional structure:
- A `meeting` table tracking active/completed meetings with metadata
- Meeting context builder that prepends summary blocks
- Manager tool: `start_meeting`, `end_meeting`
- Auto-summary generation at meeting conclusion

**This is Phase 7** — it requires group sessions (Phase 4), @-mention routing, and the ignore signal to be in place first.

### 6.4 Context Management for Meetings

Per `communication-architecture.md` Section 5: agents in a meeting receive the **full conversation history** for that meeting (no truncation). Context limits are managed by:
1. Curated summary blocks (not full prior meeting history)
2. Meeting length caps (end meeting when context grows too large)
3. Carry-forward of unresolved items to the next meeting

---

## 7. No Parallel Agent Instances (Critical Constraint)

Per `communication-architecture.md` Section 8.2: **no two instances of the same agent may run in parallel.** All invocations — whether from a user chat, a DM, a group message, a scheduled job, or a subagent call — go through a **single per-agent queue**. Tasks are processed strictly one at a time.

This is a hard constraint, not a suggestion. Running an agent in parallel across sessions would cause:
- State conflicts (agent making contradictory decisions in two conversations)
- Memory corruption (two instances writing to the same workspace files)
- Confusing behavior (agent appearing in two places at once)

### Current state

The existing `SessionQueue` is **per-session**, not per-agent. This means an agent could currently have runs processing simultaneously on different sessions (e.g., user session + subagent session). This is already wrong in theory but rarely triggered in practice.

### Required change

Replace or wrap `SessionQueue` with a **per-agent queue** (`AgentQueue`). All runs for a given agent — regardless of which session they target — are serialized through this single queue. The session-level queue becomes unnecessary or is absorbed into the agent-level queue.

```
                     All messages for Agent B
                     (user chat, DMs, groups, jobs, subagent calls)
                            |
                            v
                     +------+------+
                     | AgentQueue  |  ← single queue per agent
                     | (serial)    |
                     +------+------+
                            |
                            v
                     execute_run() on the appropriate session
```

This change should be part of **Phase 1** since it is foundational to correct agent behavior with peer messaging.

---

## 8. Task Queue Priority

Per `communication-architecture.md` Section 8, the agent task queue (the new `AgentQueue` from Section 7) supports priority levels:

- **Normal**: FIFO. Default for all invocations.
- **Urgent**: Jumps to front of queue. For blocking bugs, security incidents, or time-sensitive invocations.

The existing `SessionQueue` already has a two-tier priority system (`enqueue()` vs `enqueue_low()`). This extends to three tiers: `urgent > normal > low`. The user or system can flag an invocation as urgent via a parameter on `send_message` or the HTTP API.

---

## 9. How This Integrates with Existing Systems

### 9.1 Coexistence with invoke_agent

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

### 9.2 SSE Streaming

Peer messages integrate with existing SSE infrastructure:

- Each DM/group session is a regular session with its own SessionId
- Runs on these sessions emit SSE events through the existing `RunManager`
- The UI can subscribe to any session's event stream, including DM sessions
- A new SSE event type `message_received` notifies the UI when an agent gets a peer message

### 9.3 Context Building

No changes needed. Each agent's DM session is a regular session. ContextBuilder reads from it using the standard strategies (truncate, full, sliding-summary). The only addition is metadata on messages indicating the sender (`from` field), which the system prompt instructs the agent to use.

### 9.4 User Observation

The user needs to observe agent-to-agent conversations. This is handled by:

1. **Session listing**: `GET /sessions` (or a new filtered endpoint) returns DM and group sessions alongside user sessions. The UI shows them in a sidebar or dedicated panel.
2. **Session SSE**: The user subscribes to a DM session's event stream to watch it in real-time.
3. **Message history**: `GET /sessions/{id}/messages` returns the conversation history.

No new endpoints are needed for observation -- the existing session/run/SSE infrastructure handles it.

---

## 10. Database Schema Changes

### 10.1 New Tables

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

### 10.2 Schema Migrations to Existing Tables

```sql
-- Add daemon flag to agents table
ALTER TABLE agents ADD COLUMN daemon INTEGER NOT NULL DEFAULT 0;

-- Add sender metadata to messages (optional -- can also use the existing
-- metadata JSON column, which already exists but is rarely used)
-- No schema change needed: metadata is already TEXT (JSON).
```

### 10.3 Message Metadata Convention

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

## 11. API Changes

### 11.1 New Endpoints

```
POST   /messages              -- Send a message from one agent to another (or to a group)
GET    /agents/{name}/inbox   -- List unread/recent messages for an agent
POST   /groups                -- Create a group
GET    /groups                -- List groups
GET    /groups/{name}         -- Get group details + members
POST   /groups/{name}/members -- Add a member to a group
DELETE /groups/{name}/members/{agent} -- Remove a member
```

### 11.2 Modified Endpoints

```
GET /sessions -- Add filter params: ?type=dm|group|user to filter session types
GET /agents   -- Add daemon flag to response, add status (idle/running/listening)
```

### 11.3 New SSE Event Types

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

## 12. Crate-Level Changes

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

## 13. System Prompt Additions

Agents need to know they can communicate with peers. The staged system prompt (`prompts/tool_loop.md`) should include:

```
## Peer Messaging

You can communicate with other agents using these tools:
- `send_message(to, message)` — send a message to another agent (fire-and-forget)
- `list_agents()` — discover available agents and their roles
- `read_messages(from, last_n)` — read your DM conversation with another agent

Messages from other agents appear in your conversation as user messages with metadata
indicating the sender. When you see `{from_agent: "reviewer"}`, that message came from
the reviewer agent, not from the human user.

Use peer messaging for collaboration, status updates, and requests. Use `invoke_agent`
for task delegation where you need a result back.
```

For DM sessions, the system prompt should also include context about who the conversation partner is:

```
You are in a direct message conversation with agent "{peer_name}".
Messages from them appear as user messages. Your responses will be delivered to them.
```

This is injected by the runtime when it detects a `dm:*` context_id on the session.

---

## 14. Security Considerations

### 14.1 Access Control

**Phase 1 (open):** Any agent can message any other agent. This is simple and sufficient for small teams where all agents are trusted.

**Phase 2 (capabilities):** Extend the capability system to include messaging permissions:
- `can_message: ["reviewer", "developer"]` -- agent can only message these agents
- `can_join_groups: true/false`
- `can_create_groups: true/false`

### 14.2 Self-Messaging

An agent sending a message to itself (`send_message(to="self-name")`) is currently allowed. This is a low-priority edge case — it could be used as a "leave a note for my next run" pattern. No explicit prevention for now; revisit if it causes issues.

### 14.3 Message Rate Limiting

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

### 14.4 User Override

The user (via the API or UI) can:
- Pause a daemon agent (stop processing incoming messages)
- Mute a DM session (messages still arrive but don't trigger runs)
- Kill a runaway conversation loop
- Set rate limits per agent

---

## 15. Implementation Phases

Each phase delivers independent value and is a PR-sized chunk.

### Phase 1: MessageBus + send_message tool (DM only)

**Goal**: Agent A can send a message to Agent B. B receives it as a run input.

**Changes:**
- `alms-coordinator`: Add `MessageBus` with `send()` method. DM session derivation (`dm:a:b`). Write message to recipient's DM session.
- `alms-coordinator`: Add `RunTrigger` type and `mpsc` channel.
- `alms-gateway`: Generalize `completion_notification_loop` into `run_trigger_loop`. Wire `MessageBus` into `AppState`.
- `alms-gateway`: Replace per-session `SessionQueue` with per-agent `AgentQueue` — all runs for a given agent serialize through one queue regardless of target session (Section 7).
- `alms-runtime`: Add `SendMessageTool`. Register in runtime when MessageBus is available.
- `alms-runtime`: Add `ListAgentsTool` (reads from `SqliteStore`).
- `alms-runtime`: Add `ReadMessagesTool` (reads DM session history).
- `alms-session`: No schema changes (DM sessions are regular sessions with a `dm:*` context_id).

**Loop prevention (must ship with Phase 1):**
- Message depth tracking: the MessageBus internally tracks a `depth` counter per DM conversation chain, incremented each time a message bounces between the same pair (A→B = 1, B→A = 2, A→B = 3...). Delivery is refused at `depth > MAX_DM_DEPTH` (default: 5). This counter is managed entirely by the MessageBus — it is **not** exposed as a parameter on `send_message` and agents are unaware of it.
- Per-DM-pair cooldown: after delivering a message, the MessageBus rejects another message between the same pair for T seconds (default: 5). This prevents tight A→B→A loops from burning tokens.
- These are simple counters/timers in the MessageBus, not separate infrastructure.

**Tests:**
- Unit: MessageBus send/receive, DM context_id derivation, dual-write consistency, depth limit rejection, cooldown rejection
- Integration: Agent A sends to Agent B, B's session gets a run with the message

**Estimated size:** ~500 lines of new code, ~12 tests.

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

### Phase 5: Advanced rate limiting + user controls

**Goal**: Configurable rate limiting and user control over agent conversations. (Basic loop prevention — depth tracking and per-DM cooldown — ships in Phase 1.)

**Changes:**
- `alms-coordinator/message_bus`: Per-agent-per-hour token budget for DM/group runs
- `alms-coordinator/message_bus`: Configurable rate limits per agent (via agent config or `alms.toml`)
- `alms-gateway`: User controls: pause agent, mute session, kill runaway conversation
- `alms-gateway`: Rate limit status in agent API response

**Tests:**
- Token budget exceeded → delivery refused
- Paused agent does not process messages
- Muted session accepts but doesn't trigger runs

**Estimated size:** ~300 lines, ~8 tests.

### Phase 6: UI integration

**Goal**: The web UI shows DM sessions, group sessions, and agent-to-agent conversations alongside user sessions.

**Changes:**
- UI: Session sidebar shows DM and group sessions with agent avatars
- UI: "Send message to agent" action from the agent panel
- UI: Group management panel
- UI: Real-time message indicators (using `message_received` SSE event)
- UI: Structured message rendering (cards/badges for PR metadata, test results, etc.)

This phase is UI-only (HTML/JS/CSS) -- no Rust changes.

### Phase 7: Team meetings (Layer 2.5)

**Goal**: Manager-facilitated team meetings with summaries that feed forward.

**Changes:**
- `alms-session/sqlite`: `meetings` table (id, group_id, status, summary, round_count, max_rounds, created_at)
- `alms-coordinator`: Meeting lifecycle management (start, round tracking, auto-end at round cap)
- `alms-runtime`: `StartMeetingTool`, `EndMeetingTool` (manager-only tools)
- `alms-runtime`: Meeting context builder — prepends summary block (previous meeting summary + project stats)
- `alms-runtime`: Auto-summary generation at meeting conclusion
- `alms-gateway`: Meeting API endpoints (`POST /meetings`, `GET /meetings/{id}`, `POST /meetings/{id}/end`)

**Tests:**
- Meeting lifecycle: start, rounds, end, summary generation
- Summary feed-forward: meeting N's summary appears in meeting N+1's context
- Hard cap: meeting ends when max_rounds reached

**Estimated size:** ~600 lines, ~10 tests.

### Phase 8: PR review loop (core workflow)

**Goal**: The primary autonomous workflow — developer writes code, reviewer reviews, developer addresses, iterate until merge.

Per `communication-architecture.md` Section 12, this is the core value proposition. Built on top of all previous phases:
- Developer agent opens PR → sends structured message (PR metadata) to reviewer
- Reviewer agent reviews → sends NL feedback via DM
- Developer addresses findings → sends updated PR metadata
- Loop until reviewer approves (ignore signal = "no more findings")
- Merge signal emitted

**Changes:**
- `alms-runtime`: PR review workflow tools (`submit_review`, `address_findings`, `approve_pr`)
- Prompt engineering: developer and reviewer system prompts with review loop instructions
- Integration with `shell_exec` for actual git/GitHub operations

**Estimated size:** ~400 lines, ~8 tests. Mostly prompt engineering + tool wiring.

---

## 16. Migration Path from Current State

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

## 17. Open Questions and Future Directions

### 17.1 Response Notification

When Agent B responds to Agent A's message, Agent A is notified using the same pattern as background subagent completions: B's response is dual-written into A's DM session as a User message, and a `RunTrigger` is created on A's DM session. If A is a daemon or has an active listener, it processes B's response as a new run. If not, the response sits in A's DM session history and is visible on the next run.

This is the push model — no polling needed. The `RunTrigger` mechanism handles delivery for both the initial message and the response symmetrically.

### 17.2 Message Delivery Guarantees

What happens if the gateway restarts while a message is in the `RunTrigger` channel?

- **Current approach:** `mpsc` channel is in-memory. Messages in-flight during restart are lost.
- **Future:** Persist RunTriggers to SQLite before processing. On restart, replay unprocessed triggers.
- **Recommendation:** Accept in-memory delivery for Phase 1. The risk is low (single-process, restarts are rare) and the fix is straightforward when needed.

### 17.3 Message Ordering in Groups

Group messages are delivered independently to each member. Members process them at different speeds. This means:
- Member A might see message 1, 2, 3 in order
- Member B might see message 1, 3, 2 (if message 3 was delivered before it finished processing message 2's run)

This is acceptable for async agent communication. If strict ordering is needed, the SessionQueue already ensures serial processing per session.

### 17.4 Relationship to Layer 3

Layer 3 (emergent team dynamics) builds on Layer 2 infrastructure:

- **Scheduled standups:** A cron job triggers a `send_group_message` to the "daily-standup" group
- **Auto-review requests:** The developer agent's system prompt instructs it to call `send_message(to="reviewer", ...)` after opening a PR
- **PM escalation:** The PM agent subscribes to all group channels and intervenes when it detects blockers

Layer 2 provides the pipes. Layer 3 provides the behavior.

---

## 18. Summary

Layer 2 adds peer-to-peer communication to ALMS, aligned with the product vision in `communication-architecture.md`. Eight implementation phases build incrementally:

1. **MessageBus + DM messaging** -- core message routing with loop prevention
2. **Daemon agents** -- always-on listeners
3. **HTTP API** -- external messaging interface
4. **Group sessions** -- multi-agent conversations with @-mention routing
5. **Advanced rate limiting** -- token budgets, user controls
6. **UI integration** -- DM/group session visibility, structured message rendering
7. **Team meetings** -- manager-facilitated meetings with summary feed-forward
8. **PR review loop** -- the core autonomous workflow

Key design principles:
- **Hybrid messaging**: structured data for routine info, natural language for reasoning (cost control)
- **Dual-write sessions**: each agent has its own view, no changes to SessionManager/ContextBuilder
- **RunTrigger generalization**: the existing completion notification pattern becomes a universal message delivery mechanism
- **Ignore signal**: agents can decline invocations to avoid wasted LLM calls
- **Conservative extension**: reuses existing session/run/SSE infrastructure, keeps each phase independently deployable

---

*Design Date: 2026-03-22*
*Authors: Heph + Atlas*
*Status: Proposed -- pending review*
