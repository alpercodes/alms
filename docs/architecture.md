# ALMS Architecture v0.2 — Multi-Agent Coordination

## What is ALMS?

**ALMS** = **Agent Loop Management System**

A **coordinator-based multi-agent platform** where a Main Agent orchestrates specialized Subagents to accomplish complex tasks through delegation, parallel execution, and result synthesis.

---

## Core Design Principles

1. **Coordination over Monolith** — Main Agent delegates, Subagents execute
2. **Explicit over Implicit** — Clear task boundaries, observable handoffs
3. **Parallel by Default** — Independent tasks run concurrently
4. **Security First** — Capability-based permissions, strict sandboxing

---

## System Components

### 1. Coordinator (alms-coordinator) — NEW ⭐

**Purpose:** Central orchestrator that manages the Main Agent and Subagent lifecycle

**Architecture Pattern: Hub-and-Spoke**

```
┌─────────────────────────────────────────────────────────────┐
│                      COORDINATOR                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │  Main Agent  │  │  Subagent A  │  │  Subagent B  │      │
│  │  (Planner)   │  │  (Research)  │  │  (Code Gen)  │      │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘      │
│         │                  │                  │             │
│         └──────────────────┴──────────────────┘             │
│                            │                                │
│                    ┌───────┴───────┐                        │
│                    │  Message Bus  │                        │
│                    │  (crossbeam)  │                        │
│                    └───────────────┘                        │
└─────────────────────────────────────────────────────────────┘
```

**Key Responsibilities:**
- Spawn/kill subagents based on Main Agent requests
- Route messages between agents (Main ↔ Subagents)
- Enforce resource limits per agent
- Monitor agent health, restart failed agents
- Aggregate subagent results for Main Agent synthesis

**Agent Types:**

| Type | Role | Lifecycle |
|------|------|-----------|
| **Main** | Planner, decision maker, user interface | Persistent |
| **Subagent** | Specialist worker (research, code, analysis) | Ephemeral |
| **Tool** | WASM-based capability | Per-invocation |

---

### 2. Main Agent

**Purpose:** The user's primary interface and task planner

**Loop:**
```
User Input → Understand Intent → Decompose Task → 
Spawn Subagents → Collect Results → Synthesize Response → Reply
```

**Capabilities:**
- Natural language understanding
- Task decomposition and planning
- Subagent orchestration (when, who, what)
- Result synthesis and quality control
- Escalation to user when stuck

**Never does:**
- Direct tool execution (delegates to Tool Subagents)
- Long-running computations (delegates to Compute Subagents)
- External API calls (delegates to Integration Subagents)

---

### 3. Subagent System

**Purpose:** Specialized workers for specific task types

**Spawning Protocol:**
```rust
// Main Agent requests subagent
coordinator.spawn(SubagentRequest {
    task: "Research Rust async patterns",
    agent_type: SubagentType::Research,
    timeout: Duration::from_secs(300),
    capabilities: vec!["web_search", "read_docs"],
});

// Subagent executes independently
// Results stream back to Main Agent
// Main Agent synthesizes when all complete
```

**Built-in Subagent Types:**

| Type | Specialization | Tools |
|------|---------------|-------|
| `research` | Information gathering, analysis | web_search, read_docs, summarize |
| `code` | Code generation, review, debugging | code_gen, lint, test_run |
| `data` | Data processing, transformation | query, transform, visualize |
| `integration` | External API interactions | http_request, webhook, notify |
| `security` | Security analysis, auditing | scan, audit, report |

**Subagent Lifecycle:**
```
Spawn → Execute → Stream Progress → Complete/Fail → Report → Destroy
```

**Key Properties:**
- Isolated memory space (separate WASM instances)
- Timeboxed execution (configurable timeout)
- Resource-limited (CPU, memory caps)
- Independent failure (one fails, others continue)

---

### 4. Session Manager (alms-session)

**Purpose:** Owns all session state across Main Agent and Subagents

**Storage Hierarchy:**
```
session:{main_id}           ← Main Agent session
  └── subagent:{task_id}    ← Subagent sessions (child-of)
      └── tool:{invoke_id}  ← Tool invocations (child-of)
```

**Key Design:**
- Parent sessions track child subagent sessions
- Subagent results automatically roll up to parent
- Cancellation cascades: kill parent → kills all children
- Billing/usage aggregated at parent level

---

### 5. Agent Runtime (alms-runtime)

**Purpose:** Executes agent loops for both Main and Subagents

**The Main Agent Loop:**
```
Inbound Message → Intent Classification → Task Decomposition →
Parallel Subagent Spawn → Progress Monitoring →
Result Aggregation → Synthesis → Stream Response → Persist
```

**The Subagent Loop:**
```
Task Assignment → Context Assembly → Tool Execution →
Result Generation → Stream Progress → Complete → Report
```

**Concurrency:**
- Each agent runs in its own async task
- Work-stealing across CPU cores
- Message-passing between agents (no shared state)

---

### 6. Tool Sandbox (alms-sandbox)

**Purpose:** Isolated tool execution for both Main Agent and Subagents

**Capability Inheritance:**
- Subagents inherit capabilities from Main Agent's request
- Tools inherit capabilities from Subagent manifest
- Runtime enforces capability boundaries

---

### 7. Channel Adapters (alms-channel)

**Purpose:** User-facing interface connects only to Main Agent

**Design:**
- Users interact with Main Agent only
- Subagents are invisible to users (implementation detail)
- Main Agent decides what to show vs. what to delegate

---

## Message Flow Example

**User:** "Build me a Rust web server with user auth"

```
[User] ──► [Main Agent]
              │
              ├── Decomposes: [1] Design API, [2] Implement auth, [3] Code server
              │
              ├── Spawns Subagent "code-api" → designs OpenAPI spec
              ├── Spawns Subagent "research-auth" → evaluates auth libraries  
              └── Spawns Subagent "code-server" → implements (waits for 1,2)
              │
              ├── Collects results from 1, 2
              ├── Provides context to 3
              ├── Collects final code from 3
              │
              └── Synthesizes: "Here's your server with JWT auth using axum..."
                  │
[User] ◄──────────┘
```

---

## Multi-Agent Benefits

| Aspect | Single Agent | Multi-Agent (ALMS) |
|--------|--------------|-------------------|
| **Complexity** | Becomes bloated | Each agent focused |
| **Latency** | Sequential tasks | Parallel execution |
| **Reliability** | One failure = all fail | Isolated failures |
| **Specialization** | Generalist | Domain experts |
| **Debugging** | Opaque | Observable handoffs |
| **Scaling** | Vertical only | Horizontal spawn |

---

## Implementation Status

### Completed ✅
- [x] Core types and errors
- [x] Session manager with parent-child hierarchy
- [x] Basic agent runtime
- [x] WASM tool sandbox
- [x] Telegram channel adapter

### In Progress 🚧
- [ ] Coordinator service (message routing)
- [ ] Main Agent loop with planning
- [ ] Subagent spawn/kill lifecycle
- [ ] Inter-agent message bus
- [ ] Result aggregation

### Next 🎯
- [ ] End-to-end: Main Agent spawns subagent for task
- [ ] Parallel subagent execution
- [ ] Progress streaming from subagents
- [ ] Task decomposition prompt engineering

---

## Code Structure

```
alms/
├── crates/
│   ├── alms-core/          # Shared types, messages
│   ├── alms-coordinator/   # ⭐ NEW: Agent orchestration
│   ├── alms-session/       # Session management (hierarchical)
│   ├── alms-runtime/
│   │   ├── main_agent.rs   # ⭐ NEW: Main agent loop
│   │   ├── subagent.rs     # ⭐ NEW: Subagent implementation
│   │   └── ...
│   ├── alms-sandbox/       # WASM tool execution
│   ├── alms-channel/       # User interface adapters
│   └── alms-gateway/       # Control plane
```

---

*Architecture Date: 2026-02-09*  
*Multi-Agent Update: 2026-02-09*  
*Built by Mustafa for Alper*