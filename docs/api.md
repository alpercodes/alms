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
  "version": "0.2.0"
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

Returns all active sessions. Internal sessions (DM, notifications, episodic,
subagent, job) are excluded by default.

**Query parameters**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent_id` | UUID | _(none)_ | Filter sessions by agent UUID. Does not apply to DM sessions (they use a nil sentinel agent). |
| `include_dms` | bool | `false` | When `true`, DM sessions (`dm:*` context IDs) are included alongside regular sessions. Other internal session types remain excluded. |
| `include_notifications` | bool | `false` | When `true`, notification sessions (`notifications:*` context IDs) are included. These contain agent activity triggered by DM conversation endings, subagent completions, etc. The `agent_id` filter applies to notification sessions. |

**Response 200**
```json
{
  "sessions": [
    {
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "context_id": "telegram_main_1853446411",
      "session_type": "telegram",
      "has_active_run": false,
      "created_at": "2026-02-11T07:00:00Z",
      "last_activity": "2026-02-11T07:52:00Z",
      "status": "active"
    },
    {
      "session_id": "<uuid>",
      "agent_id": "00000000-0000-0000-0000-000000000000",
      "context_id": "dm:alice:bob",
      "session_type": "dm",
      "participants": ["alice", "bob"],
      "has_active_run": true,
      "created_at": "2026-02-11T08:00:00Z",
      "last_activity": "2026-02-11T08:15:00Z",
      "status": "active"
    },
    {
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "context_id": "notifications:alice",
      "session_type": "notification",
      "agent_name": "alice",
      "has_active_run": false,
      "created_at": "2026-02-11T09:00:00Z",
      "last_activity": "2026-02-11T09:30:00Z",
      "status": "active"
    }
  ]
}
```

**Response fields**

| Field | Type | Description |
|-------|------|-------------|
| `session_type` | string | Session type derived from the `context_id`. Always present. See table below. |
| `participants` | string[] | Participant names parsed from the DM context ID (e.g. `["alice", "bob"]`). Only present when `session_type` is `"dm"`. |
| `agent_name` | string | Agent name extracted from the notification context ID (e.g. `"alice"` from `"notifications:alice"`). Only present when `session_type` is `"notification"`. |
| `has_active_run` | bool | `true` if any queued or running run is currently tied to this session, `false` otherwise. Drives the sidebar's "active" indicator on the initial load and after SSE reconnect. Always present. Pairs with the agent-scoped SSE feed (`GET /agents/{agent_id}/events`, section 5.7) which emits live `session_activity_started` / `session_activity_ended` transitions between calls to this endpoint. See #856. |

**`session_type` values**

| Value | Context ID pattern | Description |
|-------|-------------------|-------------|
| `"chat"` | _(default)_ | Regular web chat sessions (no recognised prefix). |
| `"dm"` | `dm:{a}:{b}` | Direct message session between two agents. |
| `"notification"` | `notifications:{agent}` | Notification session for an agent (DM endings, subagent completions). |
| `"telegram"` | `telegram_{name}_{chat_id}` | Telegram channel session. |
| `"job"` | `job_{id}` | Scheduled job session. |
| `"subagent"` | `subagent_{task}` | Subagent execution session. |
| `"episodic"` | `episodic:{id}` | Episodic memory session. |

> **Note**: DM sessions only appear when `?include_dms=true` is set. Notification sessions only appear when `?include_notifications=true` is set.
> DM sessions use `AgentId::nil()` as a sentinel, so the `agent_id` filter does not apply to them.
> Job, subagent, and episodic sessions are always excluded from the listing.

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

### 4.5 Delete session
`DELETE /sessions/{session_id}`

Deletes a session by UUID. The session must not have any active (queued or running) runs.

**Response 200**
```json
{
  "ok": true,
  "deleted": "<uuid>"
}
```

**Response 404** — session not found
**Response 409 ACTIVE_RUNS** — cannot delete a session that has queued or running runs. Cancel or wait for active runs to finish before retrying.

### 4.6 Get session tool calls
`GET /sessions/{session_id}/tool-calls`

Returns all tool call records across every run in a session, ordered by run creation time (`runs.created_at`) then tool call sequence number (`run_tool_calls.seq`). Each entry includes the originating `run_id` so clients can group or correlate calls with their run.

This endpoint supplements the per-run `GET /runs/{run_id}/tool-calls` (section 5.4) by providing a session-level view. It is especially important for **DM sessions**, where tool calls are stored only in the per-run `run_tool_calls` table (not in `session_messages`) and would otherwise be lost across page reloads or session switches.

**Response 200**
```json
{
  "session_id": "<uuid>",
  "tool_calls": [
    {
      "run_id": "<uuid>",
      "seq": 0,
      "role": "assistant",
      "tool_name": "shell",
      "tool_id": "call_abc123",
      "params": "{\"command\":\"ls\"}",
      "timestamp": "2026-03-22T10:00:00Z"
    },
    {
      "run_id": "<uuid>",
      "seq": 1,
      "role": "tool",
      "tool_name": "shell",
      "tool_id": "call_abc123",
      "result": "\"file1.txt\\nfile2.txt\"",
      "timestamp": "2026-03-22T10:00:01Z"
    },
    {
      "run_id": "<uuid>",
      "seq": 0,
      "role": "assistant",
      "tool_name": "math",
      "tool_id": "call_def456",
      "params": "{\"expr\":\"2+2\"}",
      "timestamp": "2026-03-22T10:01:00Z"
    }
  ]
}
```

**Response 404** — session not found.

**Response fields**

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | string (UUID) | The session these tool calls belong to. |
| `tool_calls` | array | Ordered list of `SessionToolCall` objects (see below). |

**SessionToolCall object**

| Field | Type | Presence | Description |
|-------|------|----------|-------------|
| `run_id` | string (UUID) | always | The run that produced this tool call. |
| `seq` | integer | always | Sequence number within the run (monotonically increasing). |
| `role` | string | always | `"assistant"` for tool call requests, `"tool"` for tool results. |
| `tool_name` | string | optional | Name of the tool (e.g. `"shell"`, `"math"`). Always set in practice; absent fields use `skip_serializing_if`. |
| `tool_id` | string | optional | Provider-assigned tool call ID that correlates a call to its result. |
| `params` | string | optional | JSON-encoded tool parameters. Present on `"assistant"` role records. |
| `result` | string | optional | JSON-encoded tool result. Present on `"tool"` role records. |
| `timestamp` | string (RFC 3339) | always | When the record was created (UTC). |
| `from_agent` | string | optional | Name of the agent that issued this tool call. Mirrors the `from_agent` metadata on DM session messages so the frontend fallback merge path can attribute reasoning blocks to the correct agent when session-level persistence is missing. |

Notes:
- Ordering: records are sorted by `runs.created_at` ascending (oldest run first), then by `seq` ascending within each run. This produces a chronological view of all tool activity across the session.
- When SQLite persistence is not enabled, the endpoint returns an empty `tool_calls` array.
- For non-DM sessions, this data is also available as structured messages via `GET /sessions/{session_id}/messages` (section 4.4). The session-level tool-calls endpoint is primarily useful for DM sessions where tool calls are excluded from `session_messages`.

---

## 5) Runs (agent executions)

### 5.1 Create a run
`POST /runs`

**Request**
```json
{
  "session_id": "<uuid>",
  "agent_id": "<uuid>",
  "input": {
    "type": "text",
    "text": "Hello"
  }
}
```

`agent_id` is optional for normal sessions (the gateway resolves it from the session's owning agent) and **required** for shared DM sessions. The request body carries no config knobs — per-run overrides were removed in the #941 pivot. Operators change model / provider / posture / reasoning budgets via `PATCH /agents/{id}` (or `PATCH /settings` for server defaults) before starting the run.

**Forward compatibility.** Unknown fields on the request body are silently ignored — the deserializer does NOT use `deny_unknown_fields`. UI clients on stale builds that still send `model`, `max_tokens`, `posture`, `provider`, `debug_mode`, `thinking_budget_tokens`, `reasoning_effort`, or `gemini_thinking_budget` will continue to function: the gateway accepts the request, drops the stale fields on the floor, and runs with the agent's resolved config.

**Response 201**
```json
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "status": "queued"
}
```

**Response 400** (resolved per-agent + server budget overshoots provider cap, #919):
```json
{
  "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
  "message": "configured token budget exceeds provider cap: context.max_input_tokens (250000) + agent.max_tokens (32000) = 282000 > anthropic claude-haiku-4-5 context window (200000). Lower one or both knobs, or set ALMS_LLM_BUDGET_VALIDATION=warn to bypass.",
  "agent_id": "<uuid>",
  "provider": "anthropic",
  "model": "claude-haiku-4-5",
  "max_input_tokens": 250000,
  "max_tokens": 32000,
  "effective_total": 282000,
  "provider_cap": 200000
}
```

The gateway pre-flights every `POST /runs` against the published context window for the resolved `(provider, model)` pair (per-agent override > server default). If the sum `[context].max_input_tokens + agent.max_tokens` overshoots the cap, the run is rejected before any LLM call is made. The body carries every datum the operator needs to fix the config (provider, resolved model, both knobs, computed total, table-published cap). Pairs the cap table doesn't know about (e.g. user-declared OpenRouter models, fine-tunes) skip the check silently. The `ALMS_LLM_BUDGET_VALIDATION=warn` env var downgrades the 400 to a structured WARN log; see [`docs/config.md`](config.md#alms_llm_budget_validation--provider-context-window-enforcement-919). The same error envelope is returned by `PATCH /settings` when the candidate `[context].max_input_tokens` would create the overshoot — see § 10.2.

**Non-HTTP run failure mode (`run_error` SSE).** Triggered or queued runs that have no synchronous caller — peer DMs initiated via the `send_message` tool, scheduler / cron jobs, notification runs, subagent-completion runs, and HTTP runs whose effective budget was mutated by `PATCH /settings` or `PATCH /agents/{id}` while they sat in the queue — cannot receive a synchronous 400. The same pre-flight check fires inside `execute_run` for these paths and emits a `run_error` SSE event instead. The event uses the standard `run_error` shape documented in § 6.3 (`{ "run_id": "<uuid>", "error": { "code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER", "message": "..." } }`) — no extra structured fields are added beyond `error.code` and `error.message`. The same budget detail the 400 envelope spreads across `provider` / `model` / `max_input_tokens` / `max_tokens` / `effective_total` / `provider_cap` keys is folded into the human-readable `error.message` string (e.g. `"configured token budget exceeds provider cap: context.max_input_tokens (250000) + agent.max_tokens (32000) = 282000 > anthropic claude-haiku-4-5 context window (200000). ..."`). The run is marked `Failed` before it ever enters the running set, and queue advance is broadcast so queued-behind runs see their positions decrement. Clients that already branch on `error.code` will pick this up without code changes; clients that want structured budget detail on the SSE surface should treat the 400 envelope from § 5.1 as the canonical field shape and either parse `error.message` or correlate by `run_id` to a separate `GET /runs/{run_id}` lookup.

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
  "tool_call_count": 6,
  "resolved_config": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 4096,
    "posture": "guarded",
    "debug_mode": false,
    "thinking_budget_tokens": 0
  }
}
```

Notes:
- `response` and `error` use `skip_serializing_if = "Option::is_none"` — they are absent (not `null`) for in-flight runs, present only once the run reaches a terminal state.
- `response` maps to the agent's text output (`Run.output`); renamed at the API boundary for clarity.
- `usage` is `null` for failed/cancelled runs.
- `parent_run_id` is present (as a UUID string) for subagent runs; absent for top-level runs (uses `skip_serializing_if = "Option::is_none"`).
- `tool_call_count` (optional integer) — number of tool call records stored for this run. Present when SQLite persistence is enabled. Use `GET /runs/{run_id}/tool-calls` to retrieve the full records.
- `queue_position` (optional integer, 1-indexed) — present and `>= 1` only while `status == "queued"`. Carries the same semantic as `run_created.queued_behind` and the live `run_queue_position` SSE event so a late-joining client (page reload, polling) can render the queued state without waiting for the next decrement. Absent for running/terminal runs.
- `resolved_config` (optional object, #837) — snapshot of the layered (per-agent > server-default) config the run committed to at start-time. Per-run config overrides were removed in the #941 pivot, so the snapshot now reflects a two-layer chain instead of three. Absent for runs still queued, runs that never advanced past `queued` (e.g. queued-then-cancelled fast-path), and pre-#837 SQLite rows. Fields: `provider`, `model`, `max_tokens`, `posture`, `debug_mode` (always present); `thinking_budget_tokens` (Anthropic, `0` = disabled, always present as `u32`); `reasoning_effort` (OpenAI-compat, `"low"`/`"medium"`/`"high"`/`"minimal"`, omitted on the wire when no value reached the adapter); `gemini_thinking_budget` (Gemini, omitted on the wire when no value reached the adapter). The reasoning / thinking shape asymmetry is intentional — see the `ResolvedRunConfig` field docs in `crates/alms-core/src/run.rs` for the rationale (each field mirrors its underlying `AgentConfig` shape so the snapshot is a faithful projection of what the adapter saw).

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
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "ts": "...",
  "resolved_config": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 4096,
    "posture": "guarded",
    "debug_mode": false,
    "thinking_budget_tokens": 0
  }
}
```

`resolved_config` (optional object, #837) carries the same snapshot surfaced on `GET /runs/{run_id}`. It is absent on the wire (uses `skip_serializing_if = "Option::is_none"`, **not** emitted as `null`) when the snapshot wasn't built — for example a queued-then-cancelled fast-path that never reached the `Queued -> Running` transition. Replay of `run_started` via SSE reconnect (`Last-Event-ID`) carries the same field identically to the live broadcast. See `GET /runs/{run_id}` notes above for the field shape and the rationale for the per-knob shape asymmetry.

`run_queue_position`
Emitted when the head of the per-agent queue advances (a run finishes, fails, or is cancelled) so still-queued runs can show a live decrementing position in the UI. The event fires once per remaining queued run on the same agent each time the head advances. `position` matches the same 1-indexed semantic as `run_created.queued_behind` — `1` means "next up" (one run still ahead). No event is emitted with `position == 0`; the existing `run_started` event signals that a run has left the queue and is now executing. Fanned out on both the per-run and per-session SSE feeds.
```json
{
  "run_id": "<uuid>",
  "session_id": "<uuid>",
  "agent_id": "<uuid>",
  "position": 1,
  "ts": "..."
}
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
{ "run_id": "<uuid>", "warning": {"code":"DM_TEXT_ONLY_RETRY","message":"..."} }
```

Emitted for non-fatal conditions that the frontend should display distinctly (yellow warning styling). Warning codes: `DM_TEXT_ONLY_RETRY` (DM agent responded with text only instead of using `send_message`/`ignore_message` -- retrying), `DM_TEXT_ONLY_DROPPED` (DM retry also failed -- text response was dropped). When the warning originates from a subagent, the payload includes a `source_agent` field identifying which subagent emitted it.

`run_cancelled`
```json
{ "run_id": "<uuid>", "ts": "..." }
```

`dm_message`
Emitted on the DM session SSE stream whenever a peer message is persisted to a shared DM session. This enables live rendering of DM messages in the web UI without requiring a page reload. See #632.
```json
{
  "session_id": "<uuid>",
  "from_agent": "agent-a",
  "from_agent_id": "<uuid>",
  "message": "Hello, how are you?",
  "ts": "..."
}
```

`dm_conversation_ended`
Emitted on the DM session SSE stream when a DM conversation between two agents ends (via `ignore_message`, depth limit exceeded, user cancellation, or run failure). The web UI can use this to show a "conversation ended" indicator.
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

`reason` values: `"ignored"` (agent called `ignore_message`), `"depth_exceeded"` (MAX_DM_DEPTH reached), `"user_cancelled"` (operator cancelled the run via `POST /runs/{id}/cancel` or `POST /sessions/{id}/cancel-dm`), `"errored"` (the run failed mid-flight — LLM error, tool error, posture trip, etc.).

Note: If both agents call `ignore_message` simultaneously, duplicate `dm_conversation_ended` events may be emitted for the same session. Clients should handle duplicates gracefully.

`dm_activity_started`
Cross-session event forwarded to the agent's user-facing webchat session when a DM run starts. Enables the status bar to show "Chatting with {peer}" while a DM conversation is active. This is a lightweight echo -- no marker message is persisted because DM activity is transient. See #651.
```json
{
  "session_id": "<uuid>",
  "peer": "researcher",
  "ts": "..."
}
```

`dm_activity_status`
Cross-session event forwarded to the agent's user-facing webchat session during an active DM run. All status phases are forwarded (including `building_context`, `calling_llm`, `executing_tools`, `summarizing`, etc.) so the frontend can show real-time DM activity details. See #651, #688.
```json
{
  "session_id": "<uuid>",
  "peer": "researcher",
  "phase": "executing_tools",
  "detail": "shell_exec, fs_read",
  "ts": "..."
}
```

`phase` values: any `status` event phase (e.g. `building_context`, `calling_llm`, `executing_tools`, `summarizing`).
`detail` is present only for `executing_tools` (comma-separated tool names being executed); `null` otherwise.

`dm_activity_ended`
Cross-session event forwarded to the agent's user-facing webchat session when a single DM run completes. Distinct from `dm_conversation_ended` (which signals the entire DM conversation is over) -- this signals that one turn finished. The frontend uses this to keep "Chatting with {peer}..." visible between DM turns. See #688.
```json
{
  "session_id": "<uuid>",
  "peer": "researcher",
  "ts": "..."
}
```

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
- `from_agent` (optional string) is set whenever the runtime has a resolved agent name — that is, for any named agent (resolved via the agent registry). Unnamed-agent records omit the field. The DM UI fallback merge path uses this value to attribute reasoning blocks to the correct agent; non-DM clients can ignore the field.
- For DM sessions, tool calls are stored per-run only (not in the session history).

### 5.5 Cancel a run
`POST /runs/{run_id}/cancel`

Cancels a running or queued run. Returns 200 with `{"run_id":"...","status":"cancelling"}`.
Returns 404 if run not found, 409 if already finished.

Cancellation is cooperative — the agent loop checks a `CancellationToken` at four points
(iteration boundary, LLM call, tool execution, approval wait). The run transitions to
`cancelled` status and emits a `run_cancelled` SSE event.

### 5.6 List runs
`GET /runs?session_id=<uuid>&limit=<n>` — list runs for a session (original behaviour).
`GET /runs?agent_id=<uuid>&limit=<n>` — list runs across all sessions for an agent.

Exactly one of `session_id` or `agent_id` must be provided. Providing both returns 400.

**Query parameters**
| Parameter    | Required | Description |
|-------------|----------|-------------|
| `session_id` | one-of   | Filter runs by session |
| `agent_id`   | one-of   | Filter runs across all sessions for an agent |
| `limit`      | no       | Max results (default 50) |

**Response 200 (session_id)**
```json
{
  "runs": [
    {
      "run_id": "<uuid>",
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "status": "completed",
      "response": "...",
      "started_at": "2026-04-10T12:00:00Z",
      "ended_at": "2026-04-10T12:00:05Z",
      "usage": { "prompt_tokens": 150, "completion_tokens": 42 },
      "ts": "2026-04-10T12:00:05Z"
    }
  ]
}
```

**Response 200 (agent_id)** — enriched entries for the agent run log panel:
```json
{
  "runs": [
    {
      "run_id": "<uuid>",
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "status": "completed",
      "response": "First 200 chars of output...",
      "started_at": "2026-04-10T12:00:00Z",
      "ended_at": "2026-04-10T12:00:05Z",
      "usage": { "prompt_tokens": 150, "completion_tokens": 42 },
      "ts": "2026-04-10T12:00:05Z",
      "session_type": "chat",
      "trigger": "user",
      "context_id": "web",
      "duration_ms": 5000,
      "tool_call_count": 3
    }
  ]
}
```

`session_type` values: `"chat"`, `"dm"`, `"notification"`, `"job"`, `"subagent"`, `"telegram"`, `"episodic"`.

`trigger` values: `"user"`, `"scheduled"`, `"subagent"`, `"dm"`, `"notification"`, `"telegram"`.

Notes:
- `response` is truncated to 200 characters in agent-level listings. Use `GET /runs/{run_id}` for the full text.
- `tool_call_count` is present when SQLite persistence is enabled.
- Runs are sorted newest-first (by `ended_at`, falling back to `started_at` then `created_at`).

**Response 400** — missing or ambiguous filter:
```json
{ "error": { "code": "MISSING_FILTER", "message": "..." } }
{ "error": { "code": "AMBIGUOUS_FILTER", "message": "..." } }
```

### 5.7 Stream agent-scoped events (SSE)
`GET /agents/{agent_id}/events`

Persistent SSE feed scoped to a single agent, carrying activity events
across **all** of the agent's sessions (regular chat, DMs, notifications,
jobs). Backs the web UI's session sidebar so it can light up the
"active" indicator on any session — not just the currently-viewed one
(#856).

Filtering is performed at the broadcast layer: subscribers to one
agent's feed never see events for any other agent.

**Response 200**
- `Content-Type: text/event-stream`

**Response 404** — `agent_id` does not resolve to a known agent in the
registry. The check happens before any sender is registered, so
unknown-agent connections never insert orphan entries into the in-memory
sender map (#887).

#### Event types

`session_activity_started`
Emitted when a run on any of the agent's sessions transitions out of
`Queued` and starts executing. Pairs 1:1 with `session_activity_ended`
when the run actually executes.
```json
{
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "agent_id": "<uuid>",
  "ts": "..."
}
```

`session_activity_ended`
Emitted when a run on any of the agent's sessions reaches a terminal
state (completed, failed, or cancelled). Always paired with a prior
`session_activity_started`, **except** for the pre-cancellation path:
when a queued run is cancelled before it starts executing, the feed
emits an `ended` without a paired `started` so the sidebar's
snapshot-derived `has_active_run: true` indicator clears. Consumers
should treat the snapshot from `GET /sessions` as the source of truth
for "indicator on" and `ended` as the universal "indicator off" signal
(#888).
```json
{
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "agent_id": "<uuid>",
  "ts": "..."
}
```

#### Reconnect
Supported via `Last-Event-ID` header (automatic browser reconnect) or
`?last_event_id=<n>` query parameter (initial connection). The query
parameter takes precedence when both are present. Event IDs are scoped
to the agent's event log — separate from per-run and per-session event
log counters.

> **Note:** The agent event log is held in memory and lost on daemon
> restart. After a restart, clients should open the SSE stream without a
> `last_event_id` parameter and rely on `GET /sessions` to repopulate
> any sidebar indicators.

---

## 6) Approvals (minimal but real)

If the security posture requires approval, this must be reflected in the API.

### 6.1 List pending approvals
`GET /approvals?session_id=<uuid>`

Returns all pending approvals. The `session_id` query parameter is optional --
when provided, results are filtered to approvals belonging to runs in that session.
Resolved approvals are removed from the store automatically, so this endpoint
always returns only pending items (there is no `status` filter).

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
      "debug_mode": false,
      "is_default": true,
      "created_at": "2026-03-12T...",
      "last_active": "2026-03-12T..."
    }
  ]
}
```

`debug_mode` (#1003) is a per-agent toggle that enables a `context_debug` SSE event on each turn so the web UI can render the full assembled LLM context window in a dedicated panel. PATCH-mutable; not a config override of any kind — it never affects what the LLM receives, only what is mirrored to the UI for triage.

> **CLI note.** `alms agent create` does not expose a `--debug-mode` flag — agents are always created with `debug_mode = false`. Operators flip the flag after creation via `PATCH /agents/{id}` (or the per-agent edit modal / Settings modal in the web UI), the same way every other PATCH-mutable knob is toggled. Debug mode is a triage tool, not a creation-time decision.

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
  "posture": "guarded",
  "debug_mode": true
}
```

Valid `posture` values: `"guarded"` (default — requires approval for risky tools), `"full_control"` (no approvals), `"autonomous"` (no approvals, no human-in-the-loop expected — for background agents, scheduled jobs, and subagents).

> **Note:** When a run is system-triggered (peer-to-peer DMs via `send_message`, notification runs, subagent completions, and scheduled jobs), Guarded posture is automatically overridden to Autonomous for that run via the `is_system_triggered` flag, since there is no human in the loop to approve tool calls. Without this override the run would hang indefinitely waiting for approval that can never arrive. User-initiated runs (via the HTTP API) are never affected — Guarded posture is preserved as configured.

To clear a string override (`model`, `posture`, `provider`, `telegram_token`), pass an empty string: `"model": ""`.

**Clearing reasoning overrides (#809):** The three reasoning knobs
— `thinking_budget_tokens`, `reasoning_effort`, `gemini_thinking_budget` —
cannot use the empty-string trick because `Some(0)` is a legitimate
per-agent override meaning "disable extended thinking for this agent even
when the server default enables it". Each has a dedicated boolean clear
sentinel:

| Reasoning knob              | Clear flag                          |
|-----------------------------|-------------------------------------|
| `thinking_budget_tokens`    | `clear_thinking_budget_tokens`      |
| `reasoning_effort`          | `clear_reasoning_effort`            |
| `gemini_thinking_budget`    | `clear_gemini_thinking_budget`      |

Setting the flag to `true` resets the stored value back to `None`
(inherit server default). Example:
```json
{ "clear_thinking_budget_tokens": true }
```

Sending both the value AND the clear flag for the same knob in one
request is a `400 BAD_REQUEST` / `CLEAR_AND_VALUE_CONFLICT` — the caller
is asking for two contradictory things, so we reject rather than silently
pick one. `clear_*: false` is equivalent to omitting the field entirely
(the stored value is unchanged).

After a successful clear, a subsequent `POST /runs` resolves to the
server default from `[llm.anthropic]` / `[llm.openai]` / `[llm.gemini]`,
completing the two-layer precedence chain (per-agent > server default).
Per-run overrides were removed in the #941 pivot; agents are the single
per-tenant config surface.

**Debug mode (#1003).** `debug_mode` is a plain `bool` (not `Option<bool>`), so
no `clear_*` sentinel is needed — `false` is itself the cleared / default
state. Send `"debug_mode": true` to enable the `context_debug` SSE event
emission for this agent's runs (web UI renders the full assembled context
window in a dedicated panel); send `"debug_mode": false` to disable; omit
the field to leave the existing value unchanged. Works for both webchat
and DM sessions — for DMs, the snapshot reflects the per-perspective
context the agent currently being inspected sees on its turn.

**Response 200** — updated `AgentRecord`
**Response 400 CLEAR_AND_VALUE_CONFLICT** — both a value and the matching `clear_*` flag were sent for the same reasoning knob.

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
  "version": "0.2.0",
  "provider": "openai",
  "model": "openai/gpt-4o",
  "base_url": "https://openrouter.ai/api/v1",
  "max_tokens": 4096,
  "posture": "guarded",
  "context_strategy": "truncate",
  "stream_chunk_timeout_secs": 60,
  "enabled_tools": ["datetime", "echo", "fs_edit", "fs_glob", "fs_grep", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"],
  "agent_id": "<uuid>",
  "agents": [{"name": "main", "id": "<uuid>", "is_default": true, "model": null, "needs_bootstrap": false}],
  "workspace_dir": "./.alms/workspace",
  "context": {
    "strategy": "truncate",
    "max_input_tokens": 100000,
    "compact_trigger_pct": 0.80,
    "compact_retain_pct": 0.40,
    "summary_model": "minimax/minimax-m2.7",
    "run_summary_mode": "llm",
    "run_summary_budget": 2000,
    "summary_max_tokens": 1000
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
    "enabled": ["echo", "fs_edit", "fs_glob", "fs_grep", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"]
  },
  "llm": {
    "anthropic": {
      "thinking_budget_tokens": 0,
      "prompt_cache_enabled": true
    },
    "openai": {
      "reasoning_effort": null
    },
    "gemini": {
      "thinking_budget": null,
      "cache_enabled": true,
      "cache_ttl_seconds": 300
    }
  }
}
```

Note: Top-level flat keys (`context_strategy`, `enabled_tools`) are preserved for backward compatibility alongside the new nested objects (`context`, `session`, `logging`, `tools`, `llm`). The nested objects contain the same data in a structured form. New consumers should prefer the nested objects. The `llm` block (added in #809) mirrors the `[llm.anthropic]` / `[llm.openai]` / `[llm.gemini]` sections of `alms.toml` — these are the *server-level* defaults that feed the two-layer precedence chain (per-agent > server default; per-run overrides were removed in #941).

### 10.2 Update server settings
`PATCH /settings`

Partially update server-level configuration at runtime. The `context`, `session`, `tools`, and `llm` (#809) sections are mutable. Changes take effect on the next run; in-flight runs are unaffected. Logging requires a restart and is not accepted here.

> **Live-mutation propagation — HTTP path only.** `PATCH /settings` mutations to all four mutable sections (`context`, `session`, `tools`, `llm`) propagate to the **next `POST /runs` immediately**, with no daemon restart required. Telegram-triggered runs read from a boot-time snapshot of the `AgentConfig` held inside the `Gateway` and continue to use that snapshot until the daemon is restarted. This is pre-existing behaviour for the `context` / `session` / `tools` sections; the new `llm` block in #809 inherits the same limitation. Tooling and frontends building on `PATCH /settings` should treat HTTP and Telegram as separate propagation domains.
>
> **Persistence — `settings.json` wins over `alms.toml` on restart.** Once any field has been PATCHed, the entire mutable surface is written to `{data_dir}/settings.json` and that file is the source of truth on the next boot. Subsequent edits to the corresponding sections of `alms.toml` are silently overwritten by the persisted snapshot. To revert a PATCHed value to a TOML- or env-var-driven configuration, edit `settings.json` directly (or remove it) before restart.

Within `tools`, only `shell_policy`, `sandbox_root`, `timeout_secs`, and `max_output_bytes` are dynamically mutable. **`tools.shell_permissions` is configured in `alms.toml` only** — its allow/deny regex patterns are compiled once at startup and baked into each `ShellTool` instance (see `docs/agent-runtime-design.md` for the config schema and `docs/security-model.md` § 4.3 for the policy semantics). This applies to every field in the block: `allowed_commands`, `denied_commands`, and `classifier_overrides` are all config-file-only and are **not** PATCH-mutable. Sending `shell_permissions` in a `PATCH /settings` body is ignored; restart the gateway to pick up new patterns.

Within `llm`, each provider-family sub-block mirrors the shape of `[llm.anthropic]` / `[llm.openai]` / `[llm.gemini]` in `alms.toml`. Mutations feed the server-default layer of the two-layer precedence chain (per-agent > server default; per-run overrides were removed in #941) and are picked up on the next `POST /runs` without a restart. All provider-family sub-blocks and fields are optional; fields that are `None` in the patch body are left unchanged. API keys and endpoints are **not** in scope — those live under a separate security surface.

#### `llm` field semantics — clear sentinels and wire shape

The reasoning / caching knobs on `/settings.llm.*` use **two different "no override" semantics** at the PATCH layer, depending on the underlying type. The asymmetry is deliberate but confusing without context, so the table below is the authoritative reference.

| Field                                | Type            | "Leave alone"   | "Clear / disable" sentinel                    | Notes                                                                                                       |
|--------------------------------------|-----------------|-----------------|-----------------------------------------------|-------------------------------------------------------------------------------------------------------------|
| `llm.anthropic.thinking_budget_tokens` | `u32`         | omit the field  | `0` (= "extended thinking off")               | No "unset" state — `0` is a real value the runtime forwards as "thinking disabled".                          |
| `llm.anthropic.prompt_cache_enabled`   | `bool`        | omit the field  | `false`                                       | No tri-state.                                                                                               |
| `llm.openai.reasoning_effort`          | `Option<enum>`| omit the field  | `""` (empty string clears to `null`)          | Empty string is required because non-reasoning models (gpt-4o, claude-sonnet) reject `reasoning_effort` on the wire — clearing the server default back to "don't send" is operationally meaningful. Valid non-empty values: `"minimal"`, `"low"`, `"medium"`, `"high"`. |
| `llm.gemini.thinking_budget`           | `Option<u32>` | omit the field  | `0` (= "thinking disabled")                   | **No clear sentinel on this wire surface.** `Some(0)` is "disable", `None` (omitted) is "leave alone". To revert a PATCHed value back to the TOML / env-var default, edit `settings.json` directly and restart — the server-default layer is the bottom of the stack, so this is a operator-only escape hatch. |
| `llm.gemini.cache_enabled`             | `bool`        | omit the field  | `false`                                       | No tri-state.                                                                                               |
| `llm.gemini.cache_ttl_seconds`         | `u64`         | omit the field  | n/a — `0` rejected with 422                   | Must be > 0 if provided.                                                                                    |

**Why the asymmetry between `openai.reasoning_effort` and `gemini.thinking_budget`?** The OpenAI knob has an explicit "don't send the field at all" wire shape that is materially different from any specific value (non-reasoning models 400 if you send any `reasoning_effort`). The Gemini knob has no such state — `Some(0)` and "field absent" both produce a `thinkingBudget: 0` server-side disable in practice, so a clear sentinel would buy you nothing at the runtime layer. Tooling that needs to detect a PATCHed value can do so by reading `GET /settings`: `null` for OpenAI means "cleared", and any specific number for Gemini (including `0`) means "operator has a server-level value set".

**Wire-shape note for UI consumers — `null` vs. omitted across endpoints.** `GET /settings` always emits every `llm.*` key, with `null` for `openai.reasoning_effort` when the server default is unset. `GET /agents/{id}` **omits** the per-agent reasoning fields entirely when they are unset (`thinking_budget_tokens`, `reasoning_effort`, `gemini_thinking_budget`). Frontends rendering both surfaces should treat "field absent" and "field is `null`" as equivalent ("no value set"). The asymmetry is preserved for backward compatibility — `GET /agents` predates #809 and other consumers depend on the omit-when-null shape; `GET /settings` was added in #809 with explicit `null` to make the empty-string-clear sentinel for OpenAI legible to UI form code.

**Request body** (all fields optional):
```json
{
  "context": {
    "strategy": "compact",
    "max_input_tokens": 128000,
    "compact_trigger_pct": 0.80,
    "compact_retain_pct": 0.40,
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
  },
  "llm": {
    "anthropic": {
      "thinking_budget_tokens": 8192,
      "prompt_cache_enabled": true
    },
    "openai": {
      "reasoning_effort": "medium"
    },
    "gemini": {
      "thinking_budget": 4096,
      "cache_enabled": true,
      "cache_ttl_seconds": 300
    }
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
  "errors": ["context.strategy must be one of [\"compact\", \"sliding-summary\", \"full\", \"truncate\"], got 'invalid'"]
}
```

**Response 400** (security knob — config-file-only, #947):
```json
{
  "status": "error",
  "code": "SECURITY_KNOB_NOT_PATCHABLE",
  "errors": ["SECURITY_KNOB_NOT_PATCHABLE: settings.security is config-file-only and cannot be modified via PATCH /settings. Edit `[security]` in alms.toml ..."]
}
```

Any `PATCH /settings` body that contains a top-level `security` key — including `{ "security": {} }`, `{ "security": null }`, and a populated `{ "security": { "allow_full_os_access": [...] } }` — is rejected as a whole with `400 SECURITY_KNOB_NOT_PATCHABLE` before any other field is applied. Mixed payloads `{ "llm": {...}, "security": {...} }` reject the entire request — no partial application. The `[security]` section is config-file-only by design; PATCH mutability would let a compromised auth token silently widen the agent sandbox. Edit `[security]` in `alms.toml` and restart the gateway. See `docs/security-model.md` § 4.4 (operator escape hatch) for the threat model.

**Response 400** (legacy context field — removed in #869):
```json
{
  "status": "error",
  "code": "CONTEXT_LEGACY_FIELD_DEPRECATED",
  "errors": ["CONTEXT_LEGACY_FIELD_DEPRECATED: context.recent_window / context.summary_interval are no longer recognised on the PATCH wire — the compact strategy is now token-threshold-driven. Use compact_trigger_pct / compact_retain_pct instead."]
}
```

`PATCH /settings` rejects any context block containing `recent_window` or `summary_interval` with `400 CONTEXT_LEGACY_FIELD_DEPRECATED` before structural deserialisation runs. The threshold-based `compact_trigger_pct` (range 0.50–0.95, default 0.80) and `compact_retain_pct` (range 0.20–0.60, default 0.40) replace them — the cross-field invariant `compact_retain_pct + 0.10 <= compact_trigger_pct` is enforced at PATCH time and on TOML load. The `strategy = "sliding-summary"` value is accepted as a deprecated alias for `"compact"` (rewritten on commit, scheduled for removal in v0.3.0).

**Response 400** (candidate `context.max_input_tokens` overshoots the boot-time provider cap, #919):
```json
{
  "error_code": "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
  "message": "configured token budget exceeds provider cap: ...",
  "provider": "anthropic",
  "model": "claude-haiku-4-5",
  "max_input_tokens": 250000,
  "max_tokens": 32000,
  "effective_total": 282000,
  "provider_cap": 200000,
  "agents": [
    {
      "name": "tight-budget-agent",
      "provider": "anthropic",
      "model": "claude-haiku-4-5",
      "agent_cap": 200000,
      "would_be_total": 282000
    }
  ]
}
```

Mirrors the `POST /runs` envelope from § 5.1 (`agent_id` is omitted on the PATCH path because the budget is server-level, not agent-scoped). The validator runs against the candidate `[context].max_input_tokens` plus the live `max_tokens` in TWO layers: first against the boot-time server-default `(provider, model)`, then against every registered agent's per-agent provider/model override (PR #1020 Codex P2 #2 follow-up). On strict-mode overshoot the entire PATCH is rejected with no partial commits and the persistence file is not written; the response body's `agents` array names every offender so the operator sees the full fleet impact in one response (single-offender layouts also populate the top-level `provider` / `model` / `effective_total` / `provider_cap` for back-compat with clients that only read those fields). Warn-mode (`ALMS_LLM_BUDGET_VALIDATION=warn`) downgrades the 400 to a structured WARN log per offending agent and lets the PATCH proceed.

**Response 503** (agent store unavailable during fleet budget evaluation, #1020 follow-up):
```json
{
  "error_code": "AGENT_STORE_UNAVAILABLE",
  "message": "could not validate PATCH /settings against per-agent token budgets: failed to list agents from the registry (<sqlite error>). The PATCH was REJECTED to avoid silently accepting a budget that some agents would overshoot — retry once the agent store is reachable."
}
```

The per-agent layer of the fleet budget check loads every registered agent via `store.list_agents()`. If that call errors (typically SQLite contention or a temporary outage), the PATCH **fails closed** with a `503 AGENT_STORE_UNAVAILABLE` rather than soft-skipping the per-agent layer and committing a budget that some agents would silently overshoot. No partial mutation occurs — live config stays untouched and `settings.json` is not rewritten. Retry semantics match the existing 503 surface on `agents.rs::get_store` (registry temporarily unreachable) and the `runs/lifecycle.rs` budget path (same `store.list_agents()` failure mode on the run side): the same request body is safe to replay once the agent store is reachable again. The envelope shape is the same structured-error JSON as the 400, just with the different `error_code` and no `provider` / `model` / `agents` fields (the fleet evaluation never got far enough to populate them).

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

### 11.3 Open workspace in host file explorer
`POST /agents/{id_or_name}/workspace/open`

Spawns the host's native file explorer at the agent's workspace directory (Windows Explorer / Finder / `xdg-open`). Operator-trust: the gateway is expected to run on the same host as the operator's browser, so the existing bearer-auth gate is the only privilege check. Bearer auth applies as on other write endpoints.

The endpoint takes no client-supplied path — the workspace path is resolved server-side from the agent registry record and the configured `ALMS_WORKSPACE_DIR`. Path-traversal is closed-by-construction by `validate_agent_name`, which restricts agent names to ASCII lowercase + digits + hyphens.

The launcher process is fire-and-forget — the response returns as soon as the OS accepts the spawn. The launcher itself is expected to outlive the request (the file explorer window should stay open until the user closes it).

**Request** — empty body.

**Response 200**
```json
{ "ok": true, "path": "/abs/path/to/agents/<name>" }
```

**Errors**
- `503 NOT_CONFIGURED` — `ALMS_WORKSPACE_DIR` is unset.
- `404 NOT_FOUND` — agent not found.
- `500 WORKSPACE_PATH_MISSING` — workspace dir does not exist on disk (the agent record exists but its workspace dir was never created or has been deleted).
- `500 LAUNCHER_FAILED` — could not spawn the launcher binary (e.g., `xdg-open` not on PATH on a server-only Linux box).

**Platform notes**
- **Windows**: launches `explorer.exe`. `explorer.exe` exits with status 1 even on a successful folder-open, so the gateway does NOT inspect the launcher exit code on any platform — successful `Command::spawn` is treated as success.
- **macOS**: launches `open`.
- **Linux / other Unix**: launches `xdg-open` (relies on `xdg-utils`).

---

## 12) Timeline (cross-channel unified activity view)

### 12.1 Get agent timeline
`GET /agents/{id_or_name}/timeline?limit=N&before=TIMESTAMP`

Returns a unified, reverse-chronological stream of events across all sessions for an agent. Aggregates runs, tool calls, and significant messages into a single timeline.

Path parameter `{id_or_name}` accepts either a UUID or a name slug (same as other `/agents/{id_or_name}` endpoints).

**Query parameters:**

| Parameter | Type   | Default | Description |
|-----------|--------|---------|-------------|
| `limit`   | int    | 50      | Max events to return (capped at 200) |
| `before`  | string | —       | RFC3339 timestamp cursor; only events before this time are returned |

**Event types:**

| `event_type`     | Source          | Description |
|------------------|-----------------|-------------|
| `run_started`    | `runs` table    | Agent run started executing |
| `run_completed`  | `runs` table    | Agent run completed successfully |
| `run_failed`     | `runs` table    | Agent run failed with error |
| `run_cancelled`  | `runs` table    | Agent run was cancelled |
| `tool_call`      | `run_tool_calls`| Agent invoked a tool |
| `message_received` | `messages`   | User sent a message |
| `marker`         | `messages`      | Synthetic system event (DM ended, job notification, etc.) |

**Response 200:**
```json
{
  "agent_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_name": "atlas",
  "events": [
    {
      "timestamp": "2026-04-12T10:46:00Z",
      "event_type": "run_completed",
      "session_id": "...",
      "session_type": "chat",
      "context_id": "web",
      "run_id": "...",
      "summary": "Completed run (2100 tokens)",
      "metadata": {
        "status": "completed",
        "prompt_tokens": 1500,
        "completion_tokens": 600,
        "error": null,
        "job_id": null,
        "parent_run_id": null
      }
    },
    {
      "timestamp": "2026-04-12T10:45:00Z",
      "event_type": "tool_call",
      "session_id": "...",
      "session_type": "chat",
      "context_id": "web",
      "run_id": "...",
      "summary": "Called shell_exec",
      "metadata": {
        "tool_name": "shell_exec",
        "tool_id": "call_abc123"
      }
    },
    {
      "timestamp": "2026-04-12T10:44:00Z",
      "event_type": "run_started",
      "session_id": "...",
      "session_type": "dm",
      "context_id": "dm:alice:bob",
      "run_id": "...",
      "summary": "Started run",
      "metadata": {
        "status": "running",
        "input": "Can you review..."
      }
    }
  ],
  "pagination": {
    "limit": 50,
    "has_more": true,
    "next_before": "2026-04-12T10:44:00Z"
  }
}
```

**Pagination:** Use the `pagination.next_before` value as the `before` query parameter for the next page. When `has_more` is `false`, there are no more events.

**Response 404** — Agent not found
**Response 503** — No database configured (agent registry unavailable)

---

## 13) Auth

Bearer token authentication. Enabled when `ALMS_AUTH_TOKEN` is set.

- `Authorization: Bearer <token>` header required on all endpoints except `GET /health`
- SSE endpoints (`/runs/{id}/events`, `/sessions/{id}/events`, `/agents/{id}/events`) also accept `?token=<token>` query parameter, since the browser `EventSource` API cannot set custom headers
- Query-string auth is rejected on all non-SSE routes to prevent credential leakage into server logs, browser history, and HTTP `Referer` headers
- Single shared token configured via env var (never in config files)

---

## 14) Built-in tool response shapes (selected)

Most built-in tool response shapes are documented inline in the tool's `description()` (the surface the LLM sees) and on the `RuntimeEvent` payload. This section captures shapes whose nuances are easy to miss when reading per-call output.

### 14.1 `fs_grep`

`fs_grep` searches file contents using regex patterns. The response shape varies slightly across the three `output_mode` values (`files_with_matches`, `count`, `content`); all three share the truncation-reporting fields described below.

**Common response fields**
| Field             | Type    | Description |
|-------------------|---------|-------------|
| `matches`         | array   | Matching results — shape depends on `output_mode`. |
| `total` / `total_matches` | int | Total result count (pre-pagination). |
| `truncated`       | bool    | `true` when the file iteration was cut short by `head_limit` or the output cap. Files past the cutoff are *not* visited. |
| `truncated_lines` | int     | Count of over-cap lines (per the 256 KiB per-line cap from #913) encountered across files actually scanned. See note below. |

**`truncated_lines` semantics — important caveat.** `truncated_lines` reflects only files that were *actually visited* during the scan. When `head_limit` short-circuits the iteration (or the output-byte budget caps `files_with_matches` mode), files past the cutoff are not opened and any over-cap lines they contain are not counted. This matches the existing `truncated` flag's "results were cut short" semantic — `truncated: true` is the structural signal that the count is partial; `truncated_lines` reports only what the scan observed before stopping. To get a complete `truncated_lines` count, re-issue with `head_limit: 0` (unlimited) or paginate via `offset`.

`truncated_lines` is `0` in `output_mode: "content"`, which uses a 1 MiB whole-file gate rather than the per-line cap (the field is emitted unconditionally for response-shape consistency).

---

*Authored by Mesut (2026-02-11). Updated 2026-03-28 with episodic memory config in `/settings` response, new tools (`list_my_sessions`, `read_session`, `read_messages`, `send_message`, `list_agents`, `ignore_message`), `dm_conversation_ended` SSE event, and `notification:dm_ended` run source. Updated 2026-04-12 with `GET /sessions/{session_id}/tool-calls` endpoint (section 4.6). Updated 2026-04-12 with `GET /agents/{id_or_name}/timeline` endpoint (section 12) for cross-channel unified activity view (#608). Updated 2026-04-29 with `fs_grep` response-shape section (14.1) noting `truncated_lines` reflects only visited files when `head_limit` short-circuits the scan (#913 follow-up).*
