# ALMS API (MVP contract, evolving)

This document defines a **coherent API surface** for ALMS as an Agent Loop Management System ("agent OS") — not just “call /agent/run and get text”.

It is written with two constraints in mind:
1) **MVP must ship end-to-end** with minimal surface area.
2) The API must not paint us into a corner once we add autonomy (cron), subagents, approvals, and auditing.

If something in this doc conflicts with current code, treat this as the **desired contract** and adjust code over time.

---

## 0) What we’re trying to achieve (API-wise)

ALMS needs an API that can:
- create/manage **sessions** (conversation + state)
- start **runs** (one agent turn) and stream progress/events
- execute **tools** under policy/approval
- schedule **jobs** (cron/autonomy) that run with scoped principals
- expose **audit** and **artifacts**

Key design choice: model “things that happen” as **resources + event streams**.

---

## 1) Core resources (MVP)

### Session
A stable container for:
- history/messages
- configuration references
- run lineage

### Run
A single agent execution in a session.
Runs produce:
- assistant output (streamed)
- tool invocations (events)
- audit records

### Event stream
The canonical way for clients to observe:
- tokens
- tool start/end
- approvals needed
- subagent progress (later)

### Tool invocation
A unit of privileged work (shell/fs/net/etc.) that is always:
- capability-checked
- auditable

---

## 2) Conventions

### Base URL
- Default (local): `http://127.0.0.1:8080`

### Content types
- Requests: `Content-Type: application/json`
- Responses: `application/json` unless noted.

### IDs
Use UUID strings:
- `session_id`, `run_id`, `event_id`, `tool_invocation_id`, `job_id`, `job_run_id`

### Time
Use RFC3339/ISO timestamps in UTC:
- `2026-02-11T07:52:00Z`

### Error format (JSON)
All non-2xx responses:
```json
{
  "error": {
    "code": "STRING_CODE",
    "message": "Human readable message",
    "details": {}
  }
}
```

Suggested MVP error codes:
- `BAD_REQUEST`
- `NOT_FOUND`
- `CONFLICT`
- `INTERNAL`
- `UNAUTHORIZED` (if enabled)

---

## 3) Health

### `GET /health`

**Response 200**
```json
{
  "status": "healthy",
  "service": "alms",
  "version": "0.1.5"
}
```

---

## 4) Sessions

### Rethink: session_id-first vs context_id-first

For an “agent OS”, **session_id-first** is the clean long-term model:
- stable identifiers
- easier reconnect/streaming
- clients can list/query

But channels (Telegram) naturally bring `context_id` (chat id). So MVP should support **both**:

- session resources use `session_id`
- convenience endpoint maps `(agent_key, context_id) → session_id`

### 4.1 List sessions
`GET /sessions`

Returns all active sessions.

**Response 200**
```json
{
  "sessions": [
    {
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "context_id": "telegram_main_1853446411",
      "created_at": "2026-02-11T07:00:00Z",
      "last_activity": "2026-02-11T07:52:00Z",
      "status": "active"
    }
  ]
}
```

### 4.2 Create session
`POST /sessions`

**Request**
```json
{
  "agent_id": "<uuid>",
  "context_id": "telegram_main_1853446411"
}
```

**Response 201**
```json
{
  "session_id": "<uuid>",
  "agent_id": "<uuid>",
  "context_id": "telegram_main_1853446411"
}
```

### 4.3 Get session by agent + context
`GET /sessions/{agent_id}/{context_id}`

Resolves a session by agent UUID and context ID string. This is how channels (Telegram) look up sessions.

**Response 200** — Session object
**Response 404** — no session found for this agent/context pair

### 4.4 Get session messages
`GET /sessions/{session_id}/messages`

Returns the full chat history for a session, including tool calls and results.

**Response 200**
```json
{
  "messages": [
    { "role": "user",      "type": "text",        "content": "run ls", "timestamp": "..." },
    { "role": "assistant", "type": "text",        "content": "Sure, let me run that.", "timestamp": "..." },
    { "role": "assistant", "type": "tool_call",   "tool": "shell", "params": {"command":"ls"}, "timestamp": "...", "metadata": {"tool_call_id": "call_123"} },
    { "role": "tool",      "type": "tool_result", "tool_id": "call_123", "result": "file1.txt\nfile2.txt", "ok": true, "timestamp": "..." },
    { "role": "assistant", "type": "text",        "content": "Here are the files.", "timestamp": "..." }
  ],
  "last_event_id": 42
}
```

**Fields:**
- `messages` — array of chat messages (see types below)
- `last_event_id` — the current high-water mark of the session's SSE event log, or `null` if no SSE events have been emitted yet. Clients should pass this value as `?last_event_id=<n>` when opening the session SSE stream (`GET /sessions/{session_id}/events`) to skip replay of events already reflected in the returned messages.

**Message types:**
- `text` — plain text message (user or assistant)
- `tool_call` — assistant requested a tool execution (includes `tool`, `params`, `metadata.tool_call_id`)
- `tool_result` — tool execution result (includes `tool_id`, `result`, `ok`)
- `image` — image message (includes `url`, `alt`). The `url` field is the image URL. The `alt` field is an optional description string (`null` when absent).

System messages are excluded from the response.

---

## 5) Runs (agent executions)

### 5.1 Create a run
`POST /runs`

**Request**
```json
{
  "session_id": "<uuid>",
  "input": {
    "type": "text",
    "text": "Hello"
  },
  "mode": {
    "kind": "non_stream" 
  }
}
```

**Response 201**
```json
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "status": "queued"
}
```

Why not `POST /agent/run`?
- ALMS is about runs as first-class entities (auditable, cancellable, streamable).

### 5.2 Get run status
`GET /runs/{run_id}`

**Response 200**
```json
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "agent_id": "<uuid>",
  "status": "completed",
  "response": "The agent's text output",
  "error": null,
  "started_at": "2026-02-11T07:52:00Z",
  "ended_at": "2026-02-11T07:52:05Z",
  "usage": { "prompt_tokens": 150, "completion_tokens": 42 },
  "ts": "2026-02-11T07:52:05Z",
  "job_id": null,
  "parent_run_id": null,
  "tool_call_count": 6
}
```

Notes:
- `response` and `error` use `skip_serializing_if = "Option::is_none"` — they are absent (not `null`) for in-flight runs, present only once the run reaches a terminal state.
- `response` maps to the agent's text output (`Run.output`); renamed at the API boundary for clarity.
- `usage` is `null` for failed/cancelled runs.
- `parent_run_id` is present (as a UUID string) for subagent runs; absent for top-level runs (uses `skip_serializing_if = "Option::is_none"`).
- `tool_call_count` (optional integer) — number of tool call records stored for this run. Present when SQLite persistence is enabled. Use `GET /runs/{run_id}/tool-calls` to retrieve the full records.

### 5.3 Stream a run (SSE-first)
`GET /runs/{run_id}/events`

**Response 200**
- `Content-Type: text/event-stream`

#### SSE framing
Each event:
- `event: <type>`
- `id: <event_id>` (monotonic per run)
- `data: <json>`

Example:
```
event: run_started
id: 1
data: {"run_id":"...","session_id":"...","ts":"..."}

```

#### Event types (MVP)

`run_created`
Emitted immediately when a run is accepted and queued, before the agent loop starts. Includes an optional `source` field indicating what triggered the run and `queued_behind` indicating queue position.
```json
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "is_notification": false,
  "source": "user",
  "queued_behind": 0,
  "ts": "..."
}
```

`source` values:
- `"user"` — user-initiated run (default)
- `"peer:<agent_name>"` — direct message from another agent (e.g. `"peer:researcher"`)
- `"notification:dm_ended:<agent_name>"` — DM conversation ended notification (e.g. `"notification:dm_ended:researcher"`)
- `"job"` — scheduled job
- `"subagent"` — subagent completion notification

The `source` field is omitted when not set. `is_notification` is `true` when the run was triggered by a background event (e.g. a DM delivery or subagent completion) rather than an explicit user action. `queued_behind` (integer) is the number of runs ahead of this one in the agent's queue; 0 means the run starts immediately.

`run_started`
```json
{ "run_id": "<uuid>", "session_id": "<uuid>", "ts": "..." }
```

`token_delta`
```json
{ "run_id": "<uuid>", "delta": "text chunk" }
```

`status`
Transient phase indicator emitted at key moments during the agent loop so the UI can show what the agent is doing during silent periods. Not persisted to the event log; not replayed on SSE reconnect.
```json
{ "run_id": "<uuid>", "phase": "calling_llm", "detail": null, "ts": "..." }
```
`phase` values: `building_context`, `summarizing`, `calling_llm`, `executing_tools`.
`detail` is present only for `executing_tools` (comma-separated tool names being executed).

`tool_start`
```json
{
  "run_id": "<uuid>",
  "tool_invocation_id": "<uuid>",
  "tool": "shell",
  "params": {}
}
```

`tool_end`
```json
{
  "run_id": "<uuid>",
  "tool_invocation_id": "<uuid>",
  "ok": true,
  "result": {}
}
```

`approval_required`
```json
{
  "run_id": "<uuid>",
  "approval_id": "<uuid>",
  "capability": "shell.exec",
  "request": {
    "tool": "shell",
    "params": {}
  }
}
```

`run_finished`
```json
{ "run_id": "<uuid>", "ok": true, "ts": "..." }
```

`run_error`
```json
{ "run_id": "<uuid>", "error": {"code":"INTERNAL","message":"..."} }
```

`code` values: `AUTH` (authentication/authorization failure, e.g. 401/403), `RATE_LIMIT` (provider rate limit, e.g. 429), `TIMEOUT` (request or connection timeout), `INTERNAL` (catch-all for unexpected errors). The code is auto-classified from the error message when not set explicitly.

`run_warning`
```json
{ "run_id": "<uuid>", "warning": {"code":"MAX_ITERATIONS","message":"..."} }
```

Emitted for non-fatal conditions that the frontend should display distinctly (yellow warning styling). Warning codes: `MAX_ITERATIONS` (agent hit its iteration limit before finishing), `DM_TEXT_ONLY_RETRY` (DM agent responded with text only instead of using `send_message`/`ignore_message` -- retrying), `DM_TEXT_ONLY_DROPPED` (DM retry also failed -- text response was dropped). When the warning originates from a subagent, the payload includes a `source_agent` field identifying which subagent emitted it.

`run_cancelled`
```json
{ "run_id": "<uuid>", "ts": "..." }
```

`dm_conversation_ended`
Emitted on the DM session SSE stream when a DM conversation between two agents ends (via `ignore_message` or depth limit exceeded). The web UI can use this to show a "conversation ended" indicator.
```json
{
  "session_id": "<uuid>",
  "ended_by": "agent-b",
  "peer": "agent-a",
  "reason": "ignored",
  "context_id": "dm:agent-a:agent-b",
  "ts": "..."
}
```

`reason` values: `"ignored"` (agent called `ignore_message`), `"depth_exceeded"` (MAX_DM_DEPTH reached).

Note: If both agents call `ignore_message` simultaneously, duplicate `dm_conversation_ended` events may be emitted for the same session. Clients should handle duplicates gracefully.

`job_completed`
Emitted on the agent's user-facing session when a scheduled job run finishes. The event is informational only (no new LLM run is triggered).
```json
{
  "session_id": "<uuid>",
  "job_name": "Summarize yesterday...",
  "status": "success",
  "summary": "Truncated output (max 200 chars)...",
  "ts": "..."
}
```

`status` values: `"success"`, `"error"`, `"cancelled"`, `"unknown"`.

#### Reconnect
Supported via `Last-Event-ID` header (automatic browser reconnect) or `?last_event_id=<n>` query parameter (initial connection after loading history via REST). The query parameter takes precedence when both are present. The server replays events with IDs greater than the supplied value.

For session-level streams (`GET /sessions/{session_id}/events`), clients should pass the `last_event_id` value returned by `GET /sessions/{session_id}/messages` to avoid replaying events that are already reflected in the loaded chat history.

> **Note:** The event log is held in memory. After a daemon restart, `last_event_id` in the messages response will be `null` because no SSE events have been emitted yet. Clients should treat `null` the same as "no prior events" and open the SSE stream without a `last_event_id` parameter (which means they may see duplicates of messages already loaded via REST — this is expected and typically harmless, but clients that accumulate history incrementally should deduplicate by message ID).

### 5.4 Get run tool calls
`GET /runs/{run_id}/tool-calls`

Returns the full list of tool call and result records for a run, ordered by sequence number. Tool calls are persisted for completed, failed, and cancelled runs (partial records are saved when a run ends early).

**Response 200**
```json
{
  "run_id": "<uuid>",
  "tool_calls": [
    {
      "seq": 0,
      "role": "assistant",
      "tool_name": "shell",
      "tool_id": "call_abc123",
      "params": "{\"command\":\"ls\"}",
      "timestamp": "2026-03-22T10:00:00Z"
    },
    {
      "seq": 1,
      "role": "tool",
      "tool_name": "shell",
      "tool_id": "call_abc123",
      "result": "\"file1.txt\\nfile2.txt\"",
      "timestamp": "2026-03-22T10:00:01Z"
    }
  ]
}
```

**Response 404** — run not found.

Notes:
- `role` is `"assistant"` for tool call requests and `"tool"` for tool results.
- `params` and `result` are JSON-encoded strings (may be absent depending on the role).
- For DM sessions, tool calls are stored per-run only (not in the session history).

### 5.5 Cancel a run
`POST /runs/{run_id}/cancel`

Cancels a running or queued run. Returns 200 with `{"run_id":"...","status":"cancelling"}`.
Returns 404 if run not found, 409 if already finished.

Cancellation is cooperative — the agent loop checks a `CancellationToken` at four points
(iteration boundary, LLM call, tool execution, approval wait). The run transitions to
`cancelled` status and emits a `run_cancelled` SSE event.

---

## 6) Approvals (minimal but real)

If the security posture requires approval, this must be reflected in the API.

### 6.1 List pending approvals
`GET /approvals?status=pending&session_id=<uuid>`

### 6.2 Approve / deny
`POST /approvals/{approval_id}`

**Request**
```json
{ "decision": "approve" }
```

or
```json
{ "decision": "deny" }
```

Note:
- The run event stream should emit `approval_required` and then later `tool_start` once approved.

---

## 7) Jobs / cron (MVP+ but should be designed now)

Cronjobs are “autonomy with persistence”. Even if implementation is minimal, the API should be consistent.

### 7.1 Create job
`POST /jobs`

**Request (recurring)**
```json
{
  "agent_id": "<uuid>",
  "prompt": "Summarize yesterday",
  "schedule": { "type": "recurring", "cron": "0 9 * * *" }
}
```

**Request (one-time)**
```json
{
  "agent_id": "<uuid>",
  "prompt": "Run a one-time health check",
  "schedule": { "type": "once", "run_at": "2026-03-23T15:00:00Z" }
}
```

### 7.2 Cancel a job
`DELETE /jobs/{job_id}`

Cancels a scheduled job, removes it from the scheduler, and cancels any in-progress runs that were spawned by the job.

**Response 204** — job cancelled successfully (no body).

**Response 404** — job not found.

**Response 409**
```json
{
  "error": {
    "code": "ALREADY_CANCELLED",
    "message": "job is already cancelled"
  }
}
```

### 7.3 Run job now
`POST /jobs/{job_id}:run`

### 7.4 List job runs
`GET /jobs/{job_id}/runs`

---

## 8) Audit (MVP placeholder)

MVP may store audit records in-memory or alongside snapshots, but the shape should be defined.

Planned:
- `GET /audit?session_id=<uuid>&limit=100`

Audit records should align with `docs/security-model.md`.

---

## 9) Agents (named persistent agents)

Named agents are persistent entities stored in SQLite. Each agent has a unique slug name, optional per-agent config overrides (model, posture), and a default flag.

### 9.1 List agents
`GET /agents`

**Response 200**
```json
{
  "agents": [
    {
      "id": "<uuid>",
      "name": "default",
      "description": "",
      "model": null,
      "posture": null,
      "is_default": true,
      "created_at": "2026-03-12T...",
      "last_active": "2026-03-12T..."
    }
  ]
}
```

### 9.2 Create agent
`POST /agents`

**Request**
```json
{
  "name": "researcher",
  "description": "Researches topics",
  "model": "anthropic/claude-sonnet-4-20250514",
  "is_default": false
}
```

**Response 201** — returns the created `AgentRecord`.

Side effects: creates the agent's workspace directory at `{workspace_dir}/{name}/` with empty identity files (personality.md, goals.md, memories.md, user.md).

Errors:
- `400 INVALID_NAME` — name fails validation (1–64 chars, lowercase alphanumeric + hyphens, no leading/trailing hyphens)
- `409 DUPLICATE_NAME` — name already exists

### 9.3 Get agent
`GET /agents/{id_or_name}`

Path parameter accepts either a UUID or a name slug. UUID is tried first.

**Response 200** — `AgentRecord`
**Response 404** — agent not found

### 9.4 Update agent
`PUT /agents/{id_or_name}`

**Request** — all fields optional, only provided fields are updated:
```json
{
  "description": "Updated description",
  "model": "new-model",
  "posture": "guarded"
}
```

Valid `posture` values: `"guarded"` (default — requires approval for risky tools), `"full_control"` (no approvals), `"autonomous"` (no approvals, no human-in-the-loop expected — for background agents, scheduled jobs, and subagents).

> **Note:** When a run is system-triggered (peer-to-peer DMs via `send_message`, notification runs, subagent completions, and scheduled jobs), Guarded posture is automatically overridden to Autonomous for that run via the `is_system_triggered` flag, since there is no human in the loop to approve tool calls. Without this override the run would hang indefinitely waiting for approval that can never arrive. User-initiated runs (via the HTTP API) are never affected — Guarded posture is preserved as configured.

To clear an override, pass an empty string: `"model": ""`.

**Response 200** — updated `AgentRecord`

### 9.5 Delete agent
`DELETE /agents/{id_or_name}`

**Response 200** — `{ "ok": true, "deleted": "<uuid>" }`
**Response 409 CANNOT_DELETE_DEFAULT** — cannot delete the default agent; set another default first.

### 9.6 Set default agent
`POST /agents/{id_or_name}/default`

**Response 200** — `{ "ok": true, "default_agent": "<name>" }`

Note: clears the previous default agent atomically (SQLite transaction).

---

## 10) Settings

### 10.1 Get server settings
`GET /settings`

Returns current server defaults for UI pre-population.

**Response 200**
```json
{
  "version": "0.1.5",
  "provider": "openai",
  "model": "openai/gpt-4o",
  "base_url": "https://openrouter.ai/api/v1",
  "max_tokens": 4096,
  "posture": "guarded",
  "context_strategy": "truncate",
  "stream_chunk_timeout_secs": 60,
  "enabled_tools": ["echo", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"],
  "agent_id": "<uuid>",
  "agents": [{"name": "main", "id": "<uuid>", "is_default": true, "model": null, "needs_bootstrap": false}],
  "workspace_dir": "./data/workspace",
  "context": {
    "strategy": "truncate",
    "max_input_tokens": 100000,
    "recent_window": 20,
    "summary_interval": 10,
    "summary_model": null,
    "run_summary_mode": "llm",
    "run_summary_budget": 2000
  },
  "session": {
    "max_messages": 200,
    "max_context_tokens": 100000,
    "idle_timeout_secs": 86400,
    "auto_archive": true,
    "archive_ttl_secs": 2592000
  },
  "logging": {
    "file_enabled": true,
    "file_level": "debug",
    "rotation": "daily",
    "log_dir": null
  },
  "tools": {
    "sandbox_root": ".",
    "shell_policy": "sandboxed",
    "timeout_secs": 30,
    "max_output_bytes": null,
    "enabled": ["echo", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"]
  }
}
```

Note: Top-level flat keys (`context_strategy`, `enabled_tools`) are preserved for backward compatibility alongside the new nested objects (`context`, `session`, `logging`, `tools`). The nested objects contain the same data in a structured form. New consumers should prefer the nested objects.

### 10.2 Update server settings
`PATCH /settings`

Partially update server-level configuration at runtime. Only `context`, `session`, and `tools` sections are mutable. Changes take effect on the next run; in-flight runs are unaffected. Logging requires a restart and is not accepted here.

**Request body** (all fields optional):
```json
{
  "context": {
    "strategy": "sliding-summary",
    "max_input_tokens": 128000,
    "recent_window": 20,
    "summary_interval": 30,
    "summary_model": "gpt-4o-mini"
  },
  "session": {
    "max_messages": 10000,
    "max_context_tokens": 256000,
    "idle_timeout_secs": 86400,
    "auto_archive": true,
    "archive_ttl_secs": 2592000
  },
  "tools": {
    "shell_policy": "sandboxed",
    "sandbox_root": ".",
    "timeout_secs": 30,
    "max_output_bytes": 65536
  }
}
```

**Response 200** (all fields applied successfully):
```json
{ "status": "ok" }
```

**Response 422** (some fields had validation errors):
```json
{
  "status": "partial",
  "errors": ["context.strategy must be one of [\"sliding-summary\", \"full\", \"truncate\"], got 'invalid'"]
}
```

---

## 11) Workspace (agent identity files)

### 11.1 Get agent workspace
`GET /agents/{id_or_name}/workspace`

Returns all workspace files (personality.md, goals.md, memories.md, user.md) for the agent.

**Response 200**
```json
{
  "files": {
    "personality.md": "...",
    "goals.md": "...",
    "memories.md": "...",
    "user.md": "..."
  }
}
```

### 11.2 Update a workspace file
`PUT /agents/{id_or_name}/workspace/{file}`

Updates a single workspace file. `{file}` is one of: `personality.md`, `goals.md`, `memories.md`, `user.md`.

**Request** — plain text body or JSON:
```json
{
  "content": "Updated file content here"
}
```

**Response 200** — `{ "ok": true }`

---

## 12) Auth

Bearer token authentication. Enabled when `ALMS_AUTH_TOKEN` is set.

- `Authorization: Bearer <token>` header required on all endpoints except `GET /health`
- SSE endpoints (`/runs/{id}/events`, `/sessions/{id}/events`) also accept `?token=<token>` query parameter, since the browser `EventSource` API cannot set custom headers
- Query-string auth is rejected on all non-SSE routes to prevent credential leakage into server logs, browser history, and HTTP `Referer` headers
- Single shared token configured via env var (never in config files)

---

*Authored by Mesut (2026-02-11). Updated 2026-03-28 with episodic memory config in `/settings` response, new tools (`list_my_sessions`, `read_session`, `read_messages`, `send_message`, `list_agents`, `ignore_message`), `dm_conversation_ended` SSE event, and `notification:dm_ended` run source.*
