# Run Persistence Audit

> **Author:** Tim (automated review agent)
> **Date:** 2026-03-23
> **Scope:** All agent run types and their persistence behavior in ALMS
> **Purpose:** Determine whether cross-channel agent memory is unified or fragmented

---

## Executive Summary

ALMS has **six distinct run trigger sources**, each with its own session assignment strategy and persistence behavior. Agent memory is **fragmented by design** -- web UI sessions, Telegram sessions, DM sessions, subagent sessions, and job sessions all store conversation history in separate session records keyed by different context_id values. An agent talking to the same user via the web UI and Telegram will have **two completely separate conversation histories** with no cross-visibility.

The system prompt is never persisted -- it is reconstructed from config and workspace files on each run. Workspace files (personality.md, goals.md, memories.md, user.md) are the **only cross-channel state** shared across all run types for a given agent.

---

## Run Trigger Sources

### 1. Web UI (HTTP API) Run

**Trigger path:** `POST /runs` -> `create_run()` -> `execute_run()` in `crates/alms-gateway/src/runs.rs:264`

**Session assignment:**
- The web UI creates sessions via `POST /sessions` with `context_id = "web-chat-" + Date.now()`.
- Each "New Session" button click generates a unique timestamp-based context_id, producing a **new, isolated session** every time.
- Sessions are keyed by `(agent_id, context_id)` in the SessionManager DashMap.

**What gets persisted:**
- **User message:** Persisted to session via `session_manager.append_message()`. Write-through to SQLite `messages` table.
- **Assistant response:** Persisted via `append_message()` in `finish_run()`.
- **Tool calls (session-level):** Persisted as `Content::ToolCall` and `Content::ToolResult` messages.
- **Tool calls (run-level):** Persisted to `run_tool_calls` SQLite table via `store.save_tool_calls()`.
- **Run record:** Persisted to `runs` SQLite table.
- **System prompt:** NOT persisted. Reconstructed each run.
- **Context summary:** Persisted to `context_summaries` table if sliding-summary active.
- **Audit events:** Persisted to `audit_events` table.

**is_peer_message flag:** `false` -- uses `runtime.run()`.

---

### 2. Telegram Run

**Trigger path:** Telegram polling -> `process_telegram_message()` in `gateway.rs:660`

**Session assignment:** Context ID: `"telegram_{agent_name}_{chat_id}"`. Stable per chat.

**What gets persisted:** Same as Web UI at session level. **No** `Run` record, no SSE events, no `run_tool_calls`. Session history only.

---

### 3. Subagent Run (invoke_agent -- foreground)

**Trigger path:** `invoke_agent` tool -> `Coordinator::dispatch()` -> `run_agent_loop()`

**Session assignment:**
- **Named:** Deterministic UUID v5. `context_id = "subagent_{parent_session}_{name}"`. Stable.
- **Ephemeral:** Fresh UUID v4. `context_id = "subagent_{task_id}"`. Always new.

**Persisted:** Same as Web UI at session level. No `Run` record. No `run_tool_calls`.

**Cross-visibility:** Parent reads via `read_subagent_session`. Subagent cannot see parent.

---

### 4. Subagent Run (invoke_agent -- background)

Same session/persistence as foreground. On completion, `completion_notification_loop()` creates a follow-up notification run on the parent session (full `Run` record).

---

### 5. DM Run (Peer Messaging via send_message)

**Trigger path:** `send_message` -> `MessageBus::send()` -> `RunTrigger` -> `execute_run()` with `is_peer_message = true`

**Session assignment:** Shared DM session. `SessionId::deterministic_dm()` (UUID v5). `context_id = "dm:{first}:{second}"` sorted. Nil AgentId sentinel.

**What gets persisted:**
- **Sender message:** Persisted by `MessageBus::send()` as `Role::User` with from_agent metadata.
- **Recipient:** Uses `run_on_session()` -- skips re-persisting input.
- **Tool calls:** NOT persisted to session (`is_dm` flag). Still go to `run_tool_calls`.
- **Assistant response:** NOT persisted to session. Agent must use `send_message` to reply.
- **Run record:** Full `Run` record persisted.

**Perspective mapping:** `from_agent == self.agent_name` -> `Role::Assistant` in LLM context.

---

### 6. Scheduled Job Run

**Trigger path:** Cron -> `fire_job_run()` -> `execute_run()`

Context ID: `"job_{job_id}"`. Stable session. Full persistence. `Run` includes `job_id`.

---

## Summary Table

| Run Type | Trigger | context_id | Session Reuse | Method | Msgs | Tool Calls Session | Tool Calls run_tool_calls | Run Record | SSE |
|---|---|---|---|---|---|---|---|---|---|
| Web UI | POST /runs | web-chat-{ts} | New per click | run() | Yes | Yes | Yes | Yes | Yes |
| Telegram | Polling | telegram_{agent}_{chat} | Stable | run() | Yes | Yes | No | No | No |
| Subagent fg | invoke_agent | subagent_{parent}_{name} | Named: stable | run() | Yes | Yes | No | No | Fwd |
| Subagent bg | invoke_agent bg | Same | Same | run() | Yes | Yes | No | No | Fwd |
| DM (peer) | send_message | dm:{a}:{b} | Shared | run_on_session() | Partial | No | Yes | Yes | Yes |
| Job | Cron | job_{id} | Stable | run() | Yes | Yes | Yes | Yes | Yes |

---

## Cross-Channel Visibility Analysis

### Q1: Does a web UI run see prior Telegram conversations?
**No.** Different context_id patterns produce different sessions. Zero cross-visibility.

### Q2: Does a DM exchange appear in the agent main session?
**No.** DM sessions are completely separate (nil AgentId sentinel, different context_id).

### Q3: If agent A DMs agent B, does B main session include that?
**No.** The DM exchange lives in a shared DM session only.

### Q4: Does a subagent session include the parent context?
**No.** Subagent sessions are isolated. Parent reads via `read_subagent_session`.

### Q5: Does the agent workspace provide cross-channel continuity?
**Yes, partially.** Workspace files are prepended to system prompt for every run type. `memories.md` via `workspace_write` is the **only** cross-channel memory mechanism.

---

## Identified Gaps

### Gap 1: No unified conversation history
Each channel creates isolated sessions. An agent cannot recall conversations from other channels.

### Gap 2: Telegram runs lack run-level tracking
No `Run` records, no `run_tool_calls`, no token usage tracking.

### Gap 3: DM sessions exclude tool execution history
Tool calls skipped in session (`is_dm` flag). Only `send_message` writes stored.

### Gap 4: Subagent runs lack run-level persistence
No `Run` records. No API to query subagent run history.

### Gap 5: Context summary is per-session, not per-agent
No cross-session summary or agent-level memory beyond workspace files.

### Gap 6: Web UI always creates new sessions
No default or continuation session concept.

---

## Recommendations

### R1: Agent-level memory layer
Introduce `agent_memories` SQLite table. Populated via `remember` tool or post-run extraction. Injected into system prompt alongside workspace files.

### R2: Add Run tracking to Telegram path
Create `Run` records for token tracking and auditability.

### R3: Cross-session context injection
Inject summaries from other channels when building context.

### R4: Stable web UI sessions per agent
Default to `context_id = "web-{agent_name}"` with explicit "New Thread" for isolation.

### R5: Document DM session behavior
Document DM tool-call exclusion in `docs/architecture.md`.

---

## Files Reviewed

| File | Purpose |
|---|---|
| `crates/alms-gateway/src/runs.rs` | HTTP run creation, execute_run, job runs, DM trigger loop |
| `crates/alms-gateway/src/gateway.rs` | Telegram handling, session context_id |
| `crates/alms-runtime/src/agent.rs` | run(), run_on_session(), finish_run(), agent_loop() |
| `crates/alms-coordinator/src/lib.rs` | Subagent spawning, session identity |
| `crates/alms-coordinator/src/message_bus.rs` | DM session creation, RunTrigger |
| `crates/alms-session/src/lib.rs` | SessionManager CRUD |
| `crates/alms-session/src/types.rs` | Session/Message types |
| `crates/alms-session/src/sqlite/mod.rs` | SQLite schema |
| `crates/alms-core/src/lib.rs` | ID generation (deterministic_dm, etc.) |
| `static/ui/hooks/use-boot.js` | Web UI session creation |
