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

Returns all active sessions. Truly internal sessions (episodic, subagent)
are excluded by default. Notification (`notifications:*`) and scheduled-job
(`job_{id}`) sessions are always returned and participate in the `agent_id`
filter; DM sessions are gated on `include_dms`.

**Query parameters**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `agent_id` | UUID | _(none)_ | Filter sessions by agent UUID. Applies to chat, notification, and other agent-keyed sessions. Does not apply to DM sessions (they use a nil sentinel agent). |
| `include_dms` | bool | `false` | When `true`, DM sessions (`dm:*` context IDs) are included alongside regular sessions. Other internal session types (subagent, episodic) remain excluded. |

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
    },
    {
      "session_id": "<uuid>",
      "agent_id": "<uuid>",
      "context_id": "job_7f3a1c2e",
      "session_type": "job",
      "has_active_run": false,
      "created_at": "2026-02-11T10:00:00Z",
      "last_activity": "2026-02-11T10:05:00Z",
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
| `agent_name` | string | The session's owning agent, recovered from the `context_id`. In **this** listing that means `notification` sessions only (`"alice"` from `"notifications:alice"`). Subagent sessions are enriched with it too (#1277), but they are never listed here — see the note below for that shape and where to observe it. Absent when the context carries no recoverable owner. |
| `has_active_run` | bool | `true` if any queued or running run is currently tied to this session, `false` otherwise. Drives the sidebar's "active" indicator on the initial load and after SSE reconnect. Always present. Pairs with the global session-activity SSE feed (`GET /events/session-activity`, section 5.10) which emits live `session_activity_started` / `session_activity_ended` transitions across every agent's sessions between calls to this endpoint (originally the per-agent feed of section 5.9, #856; made cross-agent in #1211). |

**`session_type` values**

| Value | Context ID pattern | Description |
|-------|-------------------|-------------|
| `"chat"` | _(default)_ | Regular web chat sessions (no recognised prefix). |
| `"dm"` | `dm:{a}:{b}` | Direct message session between two agents. |
| `"notification"` | `notifications:{agent}` | Notification session for an agent (DM endings, subagent completions). |
| `"telegram"` | `telegram_{name}_{chat_id}` | Telegram channel session. |
| `"job"` | `job_{id}` | Scheduled job session. |
| `"subagent"` | `subagent_{parent_agent_id}_{name}` (named, #1051)<br>`subagent_{parent_agent_id}_{task_id}` (ephemeral, #1181/#1185) | Subagent execution session. Classification is on the `subagent_` prefix alone, so the legacy pre-#1185 `subagent_{task_id}` form still lands here — but it parses as neither shape and so carries no `agent_name`. |
| `"episodic"` | `episodic:{id}` | Episodic memory session. |

> **Note**: DM sessions only appear when `?include_dms=true` is set.
> DM sessions use `AgentId::nil()` as a sentinel, so the `agent_id` filter does not apply to them.
> Notification sessions are always included in the response and participate in the `agent_id` filter.
> Job sessions (`job_{id}`) are always included and participate in the `agent_id` filter (#1197). Subagent and episodic sessions are always excluded from the listing.
>
> Because subagent sessions are excluded here, their `agent_name` enrichment (#1277) is only observable on `GET /session/{session_id}` — an endpoint this document does not yet have a section for (#1284). It resolves to the subagent's own name for the named context shape, to the literal `(subagent)` marker for the ephemeral one (an ephemeral subagent has no name; parentheses are illegal in agent names, so the marker can never be read as one), and is absent when the `context_id` matches neither — which the UI renders as no name at all rather than falling back to whichever agent is selected.

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

**Response 429** (bounded run queue):

```json
{
  "error_code": "AGENT_QUEUE_FULL",
  "message": "This agent has reached its pending run limit",
  "retryable": true,
  "retry_after_ms": 1000
}
```

The gateway admits at most 64 pending runs for one agent and 1,024 pending
runs across all agents. Active runs do not count against these pending limits.
If the per-agent limit is available but the global limit is full, error_code is
GATEWAY_QUEUE_FULL instead. Queue reservation happens before the run record,
input message, cancellation token, or run_created event is created, so a 429
has no durable or streaming side effects. The response also carries
`Retry-After: 1`; clients should wait at least `retry_after_ms` before
retrying and should apply their own exponential backoff for repeated
rejections.

**Concurrent requests on the same session do not fast-fail — they serialize.**
The queue reservation is reached only *after* acquiring the per-session
admission gate, which orders the durable commit, the in-memory projection, the
queue submission, and the `run_created` publication for one session. SQLite
assigns message sequence numbers while committing, so without this gate
concurrent handlers could commit A then B but publish and enqueue B then A.
The practical consequence for clients: a second `POST /runs` on a session that
already has one in admission waits behind it rather than returning 429
immediately. The hold window is short and bounded — the guard moves into the
`run_created` task, which the handler awaits, and the fan-out is synchronous —
so this is request ordering, not a stall. Different sessions never block each
other.

**Response 500** (admission projection failed):

```json
{
  "error_code": "ADMISSION_PROJECTION_FAILED",
  "message": "Failed to publish persisted run input: <detail>"
}
```

Returned when the run and its input committed durably but could not then be
projected into in-memory state (session history or the run registry). The
durable record is authoritative and survives; the request is rejected rather
than continuing against state the gateway cannot see. Retry the request. The
sibling `LIFECYCLE_PERSISTENCE_FAILED` (also 500) covers the earlier step, the
durable admission write itself, and leaves nothing persisted.

**Response 503** (queue unavailable):
```json
{
  "error_code": "QUEUE_UNAVAILABLE",
  "message": "Run queue is unavailable while the gateway is shutting down"
}
```

The admission point returns this response when shutdown has begun or the queue
worker can no longer accept dispatch. No run-side effects are created.

**Response 400** (DM session, #1156):
```json
{
  "error_code": "DM_SESSION_NOT_DIRECTLY_RUNNABLE",
  "message": "DM sessions are agent-to-agent only; turns are triggered via send_message, not POST /runs.",
  "session_id": "<uuid>",
  "context_id": "dm:alice:bob"
}
```

DM sessions (`context_id` starting with `dm:`) are agent-to-agent only. Peer DM turns are triggered exclusively by the `send_message` tool through the internal MessageBus (`RunTrigger` → trigger loop, which enqueues with `is_peer_message: true`) — never by `POST /runs`, which always enqueues non-peer runs. A non-peer run on a DM session would arm the implicit-reply machinery from #1154 (the DM recipient prompt and the `send_message` peer-fold) while the DM completion gate refuses delivery, guaranteeing a silent drop — so the gateway rejects the request up front.

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
  "lifecycle_revision": 2,
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
- lifecycle_revision starts at 0 and increases for every accepted status transition. Clients can reject a delayed snapshot whose revision is lower than one they have already observed.
- terminal_reason is omitted outside failed/cancelled terminal states. Current machine-readable values include "failed", "cancelled", and "gateway_restarted".
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

Every SSE endpoint emits `stream_state` first on each connection, before
replayed or live domain events:

```json
{
  "stream_epoch": "<gateway-startup-uuid>",
  "retained_from": 42,
  "newest": 1041,
  "replay_gap": false,
  "epoch_mismatch": false,
  "requires_reconciliation": false
}
```

Clients should retain `stream_epoch` alongside the numeric event cursor and
send both `?last_event_id=<n>&stream_epoch=<uuid>` when reconnecting. A cursor
older than the retained floor sets `replay_gap`; a cursor from a prior gateway
process sets `epoch_mismatch`. Either condition sets
`requires_reconciliation`, which tells the client to refresh the relevant
authoritative REST snapshot while buffering live events. `newest` is the
replay snapshot ceiling: buffered events with IDs at or below it predate the
REST response and must not be re-applied after that response; higher IDs are
live transitions and apply afterward.

Agent-scoped and global session-activity subscriber channels are bounded to
256 events. Their semantic events are replayable, so a client that cannot
drain that buffer is disconnected and recovers through this protocol.

Run and session subscriber channels are lossless rather than bounded because
they also carry transient events such as token deltas that replay cannot
reconstruct. A connected client that stops draining those feeds can therefore
accumulate buffered events until it disconnects. Every feed unregisters its
sender immediately when the connection closes, including during otherwise
idle periods.

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
{ "run_id": "<uuid>", "warning": {"code":"DM_EMPTY_REPLY_RETRY","message":"..."} }
```

Emitted for non-fatal conditions that the frontend should display distinctly (yellow warning styling). Warning codes: `DM_EMPTY_REPLY_RETRY` (peer-triggered DM run produced no deliverable reply text -- nudging once to reply with text or use `ignore_message`; #1154), `DM_EMPTY_REPLY` (the nudge was exhausted -- the gateway ends the DM conversation with an `errored` reason so the peer is notified). When the warning originates from a subagent, the payload includes a `source_agent` field identifying which subagent emitted it.

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

`suppress_banner` (optional boolean, default `false`, omitted from the wire when `false`): present and `true` ONLY on the cross-session copy forwarded to an agent's user-facing web-chat (see `notify_dm_ended_to_webchat`) when the DM-end notification run is itself the visible notification in that same chat — so the reloadable `dm_ended_notification` marker is suppressed too. When `true`, clients must still clear any "Chatting with {peer}" DM status but must NOT render a "conversation ended" banner (the run is the single notification — avoids the live half of "initiator gets both"). Every DM-session-stream emission omits this field and always renders the banner. See #1215 / #1218.

`detail` (optional string, omitted from the wire when absent): the failure text behind an `"errored"` end. Present **only** on the cross-session copy forwarded to an agent's user-facing web-chat, and only for `"errored"`; every DM-session-stream emission and every non-`errored` forward omits it. Since #1258 an *interrupted* end starts no notification run, so the banner is the only live surface that explains *why* — clients rendering a "conversation ended" banner should render `detail` as an additional line when present. The same string is mirrored into the persisted `dm_ended_notification` marker's metadata, so it survives a reload.

Which ends are interrupted (#1258): `"user_cancelled"` always, and `"errored"` when the run **died** mid-turn (LLM/tool failure, panic, setup failure). An `"errored"` end whose run *completed* but produced nothing deliverable — or whose final delivery hop failed — is not interrupted and still gets its notification run, because the DM transcript it carries is the only copy the operator's chat will ever see. The distinction is internal: both shapes are `"errored"` on the wire.

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
Emitted on the agent's user-facing session when a scheduled job's **episode closes** (#1198) — quiescence or the 4-hour deadline — not at each run's end: a job whose first turn starts a DM or background subagent emits this only once the whole arc settles (see section 7.1.1). The event is informational only (no new LLM run is triggered).
```json
{
  "session_id": "<uuid>",
  "job_name": "Summarize yesterday...",
  "status": "success",
  "summary": "Output truncated at 4000 chars (JOB_SUMMARY_MAX_CHARS)...",
  "run_id": "<uuid>",
  "job_id": "<uuid>",
  "job_session_id": "job_<job-uuid>",
  "job_session_uuid": "<uuid>",
  "truncated": true,
  "ts": "..."
}
```

`status` values: `"success"`, `"error"`, `"cancelled"`, `"unknown"`.

`run_id` / `job_id` / `job_session_id` (the job's hidden-session **context id**, not a session UUID) are deep-link handles added in #1196. Under episodes (#1198), `run_id` is the episode's **final** run. `job_session_uuid` (#1217) is the hidden job session's **real `SessionId`** — resolved from the `job_{job_id}` context handle at emission time — and is the value the "Go to job session" button navigates to (`GET /session/{id}` accepts a `SessionId`, not the `job_session_id` context handle, which 400s). It is omitted (SSE) / `null` (marker metadata) when the hidden session can't be resolved, and absent on markers persisted before #1217; the card then just doesn't render the button. `truncated` is `true` when `summary` was capped at `JOB_SUMMARY_MAX_CHARS` (4000) and the full output is fetchable via `GET /runs/{run_id}` (the run's persisted `response`); the UI keys its fetch-on-expand on this flag rather than sniffing the `...` suffix. On a deadline-forced close the summary is prefixed with an `[Episode deadline reached ...]` note (composed before the cap, so the SSE payload and the persisted marker stay byte-identical). Markers persisted before #1196 lack these metadata fields; the UI degrades to the stored summary.

The persisted `job_notification` marker's metadata additionally carries an optional `episode` object (#1198) — `{"turns", "dm_count", "subagent_count", "timed_out", "detached"}` — describing the closed episode. It is metadata-only for now (the SSE payload is unchanged and the UI does not render it yet); markers persisted before #1198 lack the key.

`subagent_started`
Emitted on the parent's session SSE stream the moment the coordinator creates the subagent's session row, ahead of any nested `tool_start` from inside the subagent. Used by the web UI's Subagent status bar to make the chip navigable (click opens the subagent session) live during a foreground `invoke_agent` run (#1105) — without this event navigation was only possible after `tool_end`, which for foreground subagents means after the subagent has finished. Fires for both foreground and background paths; the event is idempotent on the client (background subagents also carry `session_id` on the `invoke_agent` tool result and the `subagent_completed` event).

Ordering invariant (paired with `tool_start` for `invoke_agent`):

1. parent's `tool_start` for `invoke_agent` is emitted first (queued onto the runtime event channel before `tool.execute()` runs);
2. `subagent_started` follows (queued by the coordinator's `spawn_subagent` after the parent's `tool_start`, FIFO on the same channel);
3. any nested `tool_start` from inside the subagent comes after (the spawned subagent task hasn't started its loop yet at step 2).

```json
{
  "session_id": "<uuid>",
  "tool_invocation_id": "<uuid>",
  "subagent_name": "reviewer",
  "subagent_session_id": "<uuid>",
  "ts": "..."
}
```

`subagent_name` is omitted on the wire (`skip_serializing_if`) for ephemeral / unnamed subagents — the frontend resolver falls back to `tool_invocation_id` to attach the new session id to the right status-bar entry. `subagent_session_id` is the row where the subagent persists its own messages (same value the `invoke_agent` tool result carries post-#1104).

`subagent_activity`
Coarse per-subagent status signal for the parent web UI's **Subagent status bar**. Emitted on the parent's stream while a subagent (foreground or background) is running; the coordinator's subagent→parent relay reduces the subagent's runtime events to these signals — the subagent's reasoning/token **text and tool params/results are not forwarded to the parent at all** (the full content streams to the subagent's own session, reachable by clicking the chip). Deduplicated at the source: consecutive deltas of the same kind collapse, so the parent sees roughly one event per activity transition.

Like `status`, this event is **ephemeral**: not persisted to any event log and not replayed by the `last_event_id` reconnect cursor. Because the source-side dedup means a signal fires at most once per transition, every **new** `GET /sessions/{id}/events` subscription is instead brought up to date at attach time: the gateway replays the CURRENT activity of each in-flight subagent for that session as synthetic `subagent_activity` events (same shape, no event id), so a client that attaches mid-phase — page reload, second tab, SSE reconnect — sees the subagent's live status immediately instead of waiting for its next transition.
```json
{
  "run_id": "<uuid>",
  "kind": "tool_start",
  "tool": "shell",
  "tool_invocation_id": "<uuid>",
  "parent_tool_invocation_id": "<uuid>",
  "source_agent": "reviewer"
}
```

`kind` values: `reasoning` (producing extended-thinking output), `writing` (producing visible output tokens), `tool_start` (a tool began executing — the only kind that carries `tool`), `tool_end` (the tool finished). `tool` is omitted on the wire for the other kinds. `source_agent` is the subagent label the UI routes the signal by (for unnamed subagents: `subagent-{task_id_prefix}`). `tool_invocation_id` (tool kinds only, omitted otherwise) is the subagent's own tool-invocation UUID: the UI counts **distinct** ids into the chip's tool count, so an attach-time snapshot replay of the in-progress `tool_start` (which re-sends the **same** id) is recognised rather than recounted, while parallel invocations of the same tool (`run_tool_calls_parallel` — same `tool`, distinct ids, no interposed `tool_end`) each count. `parent_tool_invocation_id` (all kinds; omitted only for legacy spawn paths without one) is the **parent** `invoke_agent` tool-invocation-id — the same id `subagent_started` carries, which unnamed subagent chips are keyed by. The UI resolves the target chip by this correlator **identity-exactly**; without it, resolution falls back to a first-match on the task-derived `source_agent` label, which can persistently attach one concurrent unnamed subagent's status to another's chip on snapshot replay.

`subagent_completed`
Emitted on the parent's session SSE stream when a background subagent finishes. Foreground subagents do not produce this event because their final response arrives synchronously on the parent's `invoke_agent` `tool_end`; only background subagents go through the completion-notification path. Companion to `subagent_started` — same `subagent_session_id` value, so the frontend can render the "View session" link on the completion card without any additional resolution step.
```json
{
  "session_id": "<uuid>",
  "subagent_name": "researcher",
  "status": "done",
  "summary": "Truncated summary (max 200 chars)...",
  "subagent_session_id": "<uuid>",
  "ts": "..."
}
```

`status` values: `"done"`, `"fail"`, `"cancelled"`. `subagent_name` is omitted on the wire for ephemeral subagents (same shape as `subagent_started`).

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

### 5.5 Get in-flight reasoning text
`GET /runs/{run_id}/reasoning`

Returns the concatenated extended-thinking ("reasoning") text for the **current in-flight turn** of a run, plus the maximum SSE event_id covered by that text. Used by the web UI's `loadSession` flow to rehydrate the collapsible reasoning panel after a mid-turn page reload (#1043).

Reasoning text is streamed as `reasoning_delta` SSE events while a turn is in flight and only persisted to the session-messages store (as `reasoning_blocks` metadata on the final assistant message) at end-of-turn. The standard messages GET therefore returns nothing for an in-progress turn, and the default SSE replay cursor sits at the session HWM — past every `reasoning_delta` that has already fired. This endpoint plugs that gap by reading the per-session SSE event log directly.

**Per-turn scoping (#1077).** A run may span multiple LLM turns, each closed by one or more parent-agent tool calls. Each closed turn's reasoning has already been sealed into the corresponding assistant message's `reasoning_blocks` metadata, which the messages GET (§5.3) already returns. This endpoint must therefore return ONLY reasoning that belongs to the still-open trailing turn — otherwise prior-turn reasoning would render twice on a mid-run reload (once from the sealed bubble, once seeded into a new unsealed bubble by the rehydration path). Concretely, the response includes only `reasoning_delta` events whose `event_id` is strictly greater than the latest parent-agent `tool_start` / `tool_end` event in this run. For tool-less runs and the first turn of any run (no boundary present yet), the response contains every `reasoning_delta` in the run — the original #1043 / #1054 contract.

**Response 200**
```json
{
  "run_id": "<uuid>",
  "text": "Let me think about this step by step...",
  "last_event_id": 142
}
```

**Response 404** — run not found.

Notes:
- `text` is an empty string and `last_event_id` is `null` when the run has no post-boundary `reasoning_delta` events yet (either no reasoning has streamed, or every delta seen so far has already been sealed by a subsequent tool boundary). The endpoint is safe to call on every page load regardless of run state.
- `last_event_id` is sampled during the same snapshot the text is built from, so the rehydrate→reconnect handoff is race-free: every event reflected in `text` has an id ≤ `last_event_id`. The client should pass it as `?last_event_id=<n>` on the subsequent SSE open call so the live stream replays only events not yet reflected in `text`, and the per-delta append in the UI's `reasoning_delta` handler appends to (not duplicates) the rehydrated text.
- Only **parent-agent** tool events move the turn boundary. `tool_start` / `tool_end` events emitted with a non-null `source_agent` (subagent activity) are ignored when computing the boundary — subagent tool calls belong to the subagent's own panel and do not seal the parent's reasoning bubble. Similarly, `reasoning_delta` events emitted with a non-null `source_agent` (subagent reasoning) are filtered out of the response to mirror the UI's main-panel suppression of subagent deltas. Subagent reasoning is rendered separately.
- An unmatched parent-agent `tool_start` (approval-paused or cancelled mid-call) still seals the prior turn correctly: the boundary is the latest `tool_start` **or** `tool_end` id, so an `Inflight` tool invocation with no matching `tool_end` does not regress to the prior turn's slice.
- The boundary computation is run-scoped: a tool event from a sibling run on the same session never clips the current run's reasoning.
- For DM sessions, reasoning is routed through a distinct `dm_reasoning` block layout and this endpoint is not used by the DM rehydration path.

### 5.6 Get in-flight visible-reply text
`GET /runs/{run_id}/text`

Returns the concatenated visible assistant reply text for the **current in-flight turn** of a run, plus the maximum SSE event_id covered by that text. Used by the web UI's `loadSession` flow to rehydrate the partial assistant bubble after a mid-turn session switch or page reload (#1107). This is the visible-reply analogue of §5.5 — same response shape, same per-turn scoping contract, same race-free `last_event_id` handoff.

Visible-reply text is streamed as `token_delta` SSE events which the gateway flags ephemeral in `send_event` and therefore does NOT persist to either the per-run or per-session event log. The persistence path is end-of-turn only (flushed onto the sealed assistant message). On a mid-stream session switch the UI's `chatMessages` state is wiped, the messages GET has nothing yet for the in-flight turn, and SSE replay carries no `token_delta` (ephemeral). This endpoint plugs that gap by reading an in-memory per-run accumulator that `send_event` maintains in parallel with the visible event log.

**Per-turn scoping (mirrors §5.5's #1077 contract).** A run may span multiple LLM turns, each closed by one or more parent-agent tool calls. Each closed turn's visible text has by then been sealed onto the corresponding assistant message and persisted to the messages store (returned by the standard messages GET in §5.3). This endpoint must therefore return ONLY visible text that belongs to the still-open trailing turn — otherwise prior-turn text would render twice on a mid-run reload (once from the sealed bubble, once seeded into a new unsealed bubble by the rehydration path). Concretely, parent-agent `tool_start` / `tool_end` events clear the accumulator; only the post-boundary tail is returned.

**Response 200**
```json
{
  "run_id": "<uuid>",
  "text": "I'll look at the file and...",
  "last_event_id": 142
}
```

**Response 404** — run not found.

Notes:
- `text` is an empty string and `last_event_id` is `null` when the run has no post-boundary `token_delta` events yet (either no visible text has streamed, or every delta seen so far has already been sealed by a subsequent tool boundary, or the run has reached a terminal state and the buffer has been evicted). The endpoint is safe to call on every page load regardless of run state.
- `last_event_id` is the session event log HWM at the moment the most recent delta was appended. The client should pass it as `?last_event_id=<n>` on the subsequent SSE open so the live stream replays only events not yet reflected in `text`. The HWM is sampled under the same lock chain as the append, so it never over-reports the session HWM — it can only under-report (the safe direction: the client advances the SSE cursor too little rather than skipping events).
- Only **parent-agent** `tool_start` / `tool_end` events move the turn boundary. Subagent tool events do not clear the accumulator (the parent's turn frame is independent of subagent activity). `token_delta` events emitted with a non-null `source_agent` (subagent visible reply) are filtered out at append time, mirroring the UI's live `token_delta` handler which renders subagent output in a separate panel.
- An unmatched parent-agent `tool_start` (approval-paused or cancelled mid-call) still seals the prior turn correctly: the boundary clear fires on the `tool_start` itself, so an `Inflight` tool invocation with no matching `tool_end` does not regress to the prior turn's slice.
- The accumulator is keyed by `run_id`, so a sibling run on the same session (e.g. a background subagent run sharing the parent's session event log) never contaminates the parent's `/text` response.
- The accumulator is evicted when the run reaches a terminal state (`Completed` / `Failed` / `Cancelled`), so post-completion calls return an empty `text` / null `last_event_id`. The messages GET in §5.3 is then the authoritative source — the Ok arm has sealed the visible text onto the final assistant message; the Cancelled-mid-stream arm drops the partial text by design (out of scope for in-flight rehydration).
- For DM sessions, visible reply is routed through a distinct `dm_message` event stream and `groupDmReasoningBlocks` layout. The frontend does not call this endpoint for DM sessions (`session_type !== 'dm'` gate). The backend remains uniform and would return whatever the buffer holds for a DM run, but the DM view does not render the main chat pane so the result is never consumed.

### 5.7 Cancel a run
`POST /runs/{run_id}/cancel`

Cancels a running or queued run. Returns 200 with `{"run_id":"...","status":"cancelled"}`.
Returns 404 if run not found, 409 if already finished.

By the time `POST /runs/{run_id}/cancel` returns 200, EITHER the run state has
flipped to `Cancelled` and the `run_cancelled` SSE event has been broadcast
(the overwhelmingly common case), OR `execute_run`'s terminal arm won the race
inside the handler's tiny `get_run` → `mark_run_as_cancelled` window and the
run is in its natural terminal state (`Completed` / `Failed`) with the matching
SSE event already on the feed. The SSE feed is the authoritative source of
which terminal event fired; the response body's `"status": "cancelled"` string
reflects the request that was made, not the final state. Exactly one terminal
SSE event fires per run: when an HTTP cancel races against natural completion,
the first writer wins and the loser's `mark_*` returns `false`, suppressing
the duplicate broadcast (#1046).

Cancellation is cooperative for the in-flight agent loop — the loop checks a
`CancellationToken` at four points (iteration boundary, LLM call, tool execution,
approval wait) and unwinds at the next check-point, which can take several seconds
on platforms where an in-flight TLS-bearing LLM HTTP connection is being aborted.
The state flip + SSE broadcast on the HTTP boundary is independent of that
cooperative unwind: the user-visible cancel lands synchronously on the HTTP
response and on every subscribed SSE feed; the loop's actual exit follows.

### 5.7.1 Cancel a subagent (session-keyed)
`POST /sessions/{session_id}/subagent/cancel`

Cancels the live subagent running on the given **subagent** session. Returns
200 with `{"session_id":"...","status":"cancelling"}` when a live
(pending/running) subagent was found and its cancellation token fired;
returns 404 with error code `NO_LIVE_SUBAGENT` when the session has no live
subagent (unknown session, or the subagent already reached a terminal
state — e.g. a cancel racing natural completion).

Session-keyed rather than run-keyed because the UI's subagent surfaces (the
status-bar chips and the subagent session view) carry the subagent's session
id, not its run id — and a subagent's own run id has no cancel token in the
run manager, so `POST /runs/{run_id}/cancel` returns 409 for subagent runs
without cancelling anything. Cancelling the **parent** run still cascades to
its subagents as before; this endpoint cancels one subagent *without*
touching the parent run.

`"status": "cancelling"` is deliberate: cancellation is cooperative and
completes asynchronously. The terminal effects follow on the streams — the
subagent's own session emits `run_cancelled`, its run record flips to
`Cancelled`, and (for background subagents) the parent session receives a
`subagent_completed` event with `status: "cancelled"`. Cancelling a
**foreground** subagent instead surfaces on the parent as the blocked
`invoke_agent` tool call failing with `"Subagent was cancelled"` (the
parent run continues and handles the tool error like any other tool
failure).

### 5.8 List runs
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

### 5.9 Stream agent-scoped events (SSE)
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

Both event types include `has_active_run`, the backend's authoritative
post-transition answer to whether any queued or running run remains on that
session. Consumers should set or clear session activity from this boolean,
not from the individual event type.

`session_activity_started`
Emitted when a run on any of the agent's sessions transitions out of
`Queued` and starts executing. Pairs 1:1 with `session_activity_ended`
when the run actually executes.
```json
{
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "agent_id": "<uuid>",
  "has_active_run": true,
  "ts": "..."
}
```

`session_activity_ended`
Emitted when a run on any of the agent's sessions reaches a terminal
state (completed, failed, or cancelled). Always paired with a prior
`session_activity_started`, **except** for the pre-cancellation path:
when a queued run is cancelled before it starts executing, the feed
emits an `ended` without a paired `started` (#888). An `ended` event
can also carry `has_active_run: true` when another overlapping run remains
queued or running on the same session.
```json
{
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "agent_id": "<uuid>",
  "has_active_run": false,
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
> restart. Clients should retain and submit both their `last_event_id` and
> `stream_epoch`. The restarted gateway responds with a `stream_state`
> whose `epoch_mismatch` and `requires_reconciliation` fields are true;
> clients must then refresh the authoritative sessions snapshot while
> buffering live activity events.

> **Cross-agent activity:** this feed is scoped to a single agent, so it
> cannot surface activity on sessions owned by *other* agents. The web UI
> sidebar (which renders cross-agent Jobs / Direct-messages / Notifications
> sections) subscribes to the **global** feed in section 5.10 instead. This
> per-agent feed remains for agent-scoped consumers that want isolation.

### 5.10 Stream global session activity (SSE)
`GET /events/session-activity`

Persistent **global, cross-agent** SSE feed carrying `session_activity_started`
/ `session_activity_ended` for runs across **every** agent's sessions. Unlike
the per-agent feed (section 5.9), it is not scoped to one agent, so it can
light the web UI sidebar's active-run indicator on sessions owned by *any*
agent — the cross-agent Jobs / Direct-messages / Notifications sections a
per-agent feed can never cover (#1211).

The feed is served from a dedicated broadcast namespace (separate from the
per-agent sender map and event log), so no agent id can collide with it and
leak cross-agent activity onto a per-agent feed. There is no `{agent_id}`
path parameter and hence no registry existence check — any authenticated
client may subscribe.

**Response 200**
- `Content-Type: text/event-stream`

#### Event types
Identical payloads to section 5.9 — `session_activity_started` and
`session_activity_ended`, each carrying `session_id`, `run_id`, `agent_id`,
`ts`, and `has_active_run`. The boolean is the backend's authoritative
post-transition answer to whether *any* queued or running run remains on the
session; consumers must use it instead of treating every individual
`session_activity_ended` as session inactivity. The `agent_id` field
identifies the owning agent of the session whose activity changed (which may
differ from the agent the
operator is currently viewing).

#### Reconnect
Supported via `Last-Event-ID` header or `?last_event_id=<n>` query parameter
(the query parameter takes precedence). Event IDs are scoped to this feed's
own event log — separate from the per-run, per-session, and per-agent
counters. The bounded log can truncate an old cursor, and the log is
in-memory so its IDs reset on daemon restart. The leading `stream_state`
event reports either condition through `requires_reconciliation`. The web UI
then reconciles from `GET /sessions` (`has_active_run`) while buffering
activity events, discards replayed events through the advertised `newest`
ceiling, and applies only newer live transitions after the snapshot so no
transition during the request is lost and no retained-tail event can regress
the authoritative state.

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

**Deny semantics (#1109)** — denial means "stop", not a soft tool error:
- The denied run terminates: the runtime records a
  `{"user_denied": true, "message": ...}` tool result (distinct from the
  `{"error": ...}` shape used for real tool failures), then the run goes
  to `cancelled` (not `failed`) and emits `run_cancelled`.
- Runs still `Queued` on the same session are also cancelled (status
  `cancelled` + `run_cancelled` event each) so they don't auto-start when
  the per-agent queue advances. A queued run whose cancel token is not
  yet registered (the brief `POST /runs` insert-to-register window) is
  skipped and left `Queued` — it will run normally (see #1142 for the
  structural fix).
- Queued runs on *other* sessions are untouched.

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

**Response 201** — the final persisted job entity after scheduler
registration. For recurring jobs this includes the computed `next_run_at`
and the lifecycle revision that stored it.

### 7.1.1 Job episodes (#1198)

A firing opens a **job episode**: the job stays active until the agent's full
multi-step task is done — across DMs it starts (`send_message`) and background
subagents it dispatches (`invoke_agent(background=true)`) — not just its first
turn. The completion notification, `record_run`, and the recurring re-arm all
fire at **episode close**: quiescence (a turn ends with no pending DMs /
subagents and no queued follow-up run) or the 4-hour deadline
(detach-and-complete: the job completes with a deadline note; still-live
pending work keeps running on its own lifecycle). Recurring firings that
become due while an episode is open **queue** — coalesced into exactly one
immediate catch-up at close. See `docs/jobs-await-completion-design.md`.

`GET /jobs` and `GET /jobs/{job_id}` include an `episode` object while one is
open (absent otherwise):

```json
{
  "id": "<uuid>",
  "status": "active",
  "episode": {
    "started_at": "2026-07-06T10:00:00Z",
    "pending_dms": 1,
    "pending_subagents": 0,
    "in_flight_runs": 0,
    "runs": 2,
    "catch_up_queued": false,
    "deadline_remaining_secs": 13800
  }
}
```

Every job object carries `lifecycle_revision`, `retry_count`, and optional
`last_error`. Status is one of `pending`, `active`, `completed`, `failed`, or
`cancelled`. Terminal reasons are `completed`, `deadline_reached`,
`retry_exhausted`, and `operator_cancelled`. Dispatch failures are retried from
persisted scheduler intent with a bounded budget; a successful dispatch resets
the retry fields.

### 7.2 Cancel a job
`DELETE /jobs/{job_id}`

Cancels a scheduled job, removes it from the scheduler, and cancels any in-progress runs that were spawned by the job — including episode continuation runs. An open episode is torn down: pending DM conversations are ended with the `user_cancelled` reason (the peer gets the standard ended-notification) and pending background subagents are cancelled (#1198).

**Response 200** — the final persisted cancelled job entity. The response
includes the authoritative `lifecycle_revision`, `status: "cancelled"`, and
`terminal_reason: "operator_cancelled"`.

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
Completed or failed jobs also return `409`, with code `JOB_TERMINAL`; their
persisted status, terminal reason, retry count, and last error are unchanged.

### 7.3 Run job now
`POST /jobs/{job_id}:run`

### 7.4 List job runs
`GET /jobs/{job_id}/runs`

---

## 8) Audit and operations

Audit records align with `docs/security-model.md`:

- `GET /audit?session_id=<uuid>&limit=100`

### 8.1 Operational metrics

`GET /operations/metrics` is authenticated and returns process-lifetime
counters plus current subscriber gauges. Fields are serialized in the group
order described below; the blank lines here mark the group boundaries and are
not part of the response.

```json
{
  "queue_saturation_rejections_total": 0,
  "lifecycle_transition_rejections_total": 0,
  "replay_gaps_total": 0,
  "replay_epoch_mismatches_total": 0,
  "persistence_snapshot_rejections_total": 0,
  "job_dispatch_retry_attempts_total": 0,
  "job_dispatch_retry_exhaustions_total": 0,

  "job_rearm_failures_total": 0,
  "stale_run_recovery_failures_total": 0,
  "job_bootstrap_failures_total": 0,
  "persistence_rows_skipped_total": 0,
  "persistence_rows_skipped_by_table": {
    "agents": 0,
    "audit_events": 0,
    "jobs": 0,
    "messages": 0,
    "run_tool_calls": 0,
    "runs": 0,
    "session_summaries": 0,
    "sessions": 0,
    "timeline": 0
  },
  "persistence_fields_degraded_total": 0,
  "persistence_fields_degraded_by_field": {
    "agents.name": 0,
    "runs.job_id": 0,
    "runs.parent_run_id": 0,
    "session_summaries.last_run_id": 0
  },

  "job_boot_catch_ups_total": 0,

  "subscribers": { "runs": 0, "sessions": 0, "agents": 0, "activity": 0 }
}
```

**Read the groups, not the names.** The wire names do not say which group a
counter belongs to, and the three groups want opposite reactions from an
operator — one is expected to be non-zero, one means something is broken. The
grouping below is the same one carried by the `OperationalMetricsSnapshot`
field comments in `alms-gateway/src/operations.rs`; **change the two
together.**

**1. Rejections** — `queue_saturation_rejections_total`,
`lifecycle_transition_rejections_total`, `replay_gaps_total`,
`replay_epoch_mismatches_total`, `persistence_snapshot_rejections_total`,
`job_dispatch_retry_attempts_total`, `job_dispatch_retry_exhaustions_total`.

A request, transition, or dispatch the daemon refused or had to retry.
**Expected to be non-zero under load** — read them as a rate, not as an
absolute, and alert on a slope rather than on `> 0`. Nothing durable is in
doubt.

**2. Quarantine and degradation** — `job_rearm_failures_total`,
`stale_run_recovery_failures_total`, `job_bootstrap_failures_total`,
`persistence_rows_skipped_total` (and its `persistence_rows_skipped_by_table`
breakdown), `persistence_fields_degraded_total` (and its
`persistence_fields_degraded_by_field` breakdown).

Durable state the daemon could not take at face value. **Any non-zero value
means the daemon is serving a view of the database that does not match what is
on disk** and something needs repairing; alerting on `> 0` is correct here. See
[`docs/architecture.md` § "Reconciliation policy: absence must be a safe
belief"](architecture.md#reconciliation-policy-absence-must-be-a-safe-belief)
for what each site owes an operator. Per-counter detail follows below.

**The group holds two different faults, and they want different urgency.** The
*quarantine* counters mean trust was **withheld**: the row was dropped and kept
out of live state, so the daemon's view is incomplete but nothing it serves is
wrong. `persistence_fields_degraded_total` (#1246) means trust was
**misplaced**: the row *is* being served, carrying a column the parser could
not read. The fault is projected into live state rather than contained, and it
is invisible from the outside — a degraded value does not read as corrupt, it
reads as an ordinary value. **Incomplete beats wrong**, so treat a non-zero
`persistence_fields_degraded_total` as the more urgent of the two — then read
`persistence_fields_degraded_by_field`, because the four fields differ by more
than an order of magnitude in what they cost you.

**3. Workload** — `job_boot_catch_ups_total`.

Work done, not trust withheld. A large value after a long outage is expected,
not a fault.

`subscribers` is in none of the three: it is a point-in-time **gauge** of live
SSE subscribers per stream, and unlike everything above it goes down as well
as up.

`job_rearm_failures_total` (#1233) counts episode-close job writes that
exhausted their bounded retry budget. Non-zero means at least one job advanced
its schedule **in the scheduler only**. The job entity did not advance at all —
the write persists before the in-memory commit, so a failure leaves both stale.
**`GET /jobs` will therefore report a `next_run_at` in the past for that job
while it is in fact armed for its next occurrence**, and the web UI renders
that past date verbatim; a stale `next_run_at` next to a non-zero counter is
the expected symptom, not a second bug. `last_run_at` is stale for the same
reason. The next successful run overwrites both, and a restart before then
replays the already-executed tick. Investigate SQLite health when this moves.

`job_bootstrap_failures_total` counts jobs that could not be re-registered
with the scheduler at startup and were skipped so the daemon could still
start. Non-zero means those jobs **will not fire at all** until they are
repaired or recreated; each one is logged at `error!` with its job id.

`stale_run_recovery_failures_total` (#1236) counts run rows the startup sweep
could not reconcile and skipped so the daemon could still boot. Non-zero means
at least one durable row still claims `queued`/`running` from a dead process
and will never complete. Each skipped row is logged at `error!` with its
`run_id` and a `remediation` field carrying the SQL that clears it.

`job_boot_catch_ups_total` (#1235) counts jobs whose persisted fire time was
already past due when the daemon started. These form the **catch-up cohort**:
rather than all firing at `now + 1s`, they are ordered most-overdue-first and
spaced 15 seconds apart, so a restart after long downtime spreads the missed
firings instead of producing a concurrent burst. Jobs that are *not* past due
keep their real schedule and are never staggered. The cohort size scales with
how long the daemon was down, so a large value after a restart is expected and
is the number to reach for when reasoning about post-restart LLM spend.

`persistence_rows_skipped_total` (#1241) counts durable rows the daemon either
could not parse, or could not classify safely enough to act on — and therefore
left out of whatever it was doing, with `persistence_rows_skipped_by_table`
giving the per-table breakdown. Non-zero means one of two things, and the
`detail` field tells you which: the daemon is serving an **incomplete view of
the database** (an agent missing from the registry, a session missing from the
sidebar, messages missing from a context window), or an **operation completed
without finishing its cleanup** (a delete that committed while one of its
sessions was left standing). Each skip is logged at `warn!` with a `table`
field and whatever identifies the row — for the loaders often only the parse
error, because the column that failed to parse is frequently the id.

Three things to know before reading the number:

- **It counts skips, not distinct rows.** Most of these sites are loaders that
  run on every read, not once at startup, so one corrupt row on a hot path
  increments the counter on every load that passes over it. The rate matters
  more than the total; a counter climbing steadily under normal traffic is one
  bad row, not many.
- **The key set is stable.** Every table is reported including the zeroes, and
  reported even when no SQLite store is configured, so a scraper never sees
  keys appear and disappear. `timeline` is the `messages`/`runs` union behind
  `GET /timeline` rather than a table of its own.
- **The key names the table the row came from, not the symptom — and one table
  can have several producers.** `sessions` is incremented by the three session
  loaders (symptom: a session missing from the sidebar), *and* by `delete_agent`
  in **two different shapes** — a row the delete could not read (symptom: an
  agent deleted while one of its sessions keeps its messages, runs, and
  tool-call rows — durable orphans, not lost data) and a DM-cascade peer probe
  that failed (symptom: a DM session left unpurged because the daemon could not
  prove its other participant was gone — nothing orphaned and nothing lost,
  just uncollected) — *and* by the Telegram context-id migration (symptom: a
  session left on the legacy `telegram_{chat_id}` context id). Same number,
  four different remediations.
  **The `detail` field on the `warn!` line is the disambiguator** — the
  write-path sites prefix it (`delete_agent <id>: ...`,
  `telegram context-id migration: ...`), the loaders do not.

**Remediation never requires a restart, but it is a different action for each
of the four shapes** — read the `detail` prefix first, then pick from the
list below. "Fix the row and the daemon re-reads it" is true only for the
loaders. At every write-path site below, the operation has already committed
and nothing re-runs it, so repairing the row on its own changes nothing.

- **A loader drop** — no `detail` prefix. Find the row with
  `sqlite3 .alms/alms.db` (the `warn!` detail names the failing column), then
  fix or delete it. The daemon picks up the repair on the next read.
- **A `delete_agent` orphan** — `detail` starts `delete_agent <agent_id>:` and
  reports an unreadable session id or DM candidate. The delete transaction
  committed without that session, and the agent is already gone, so there is
  nothing left to re-run. Finish the delete by hand: locate the stranded
  session (`SELECT session_id FROM messages WHERE session_id NOT IN (SELECT id
  FROM sessions)`, and the same for `runs`), delete its dependent rows in FK
  order — `context_summaries`, `session_summaries`, `audit_events`, `messages`,
  `run_tool_calls`, `runs` — and then the `sessions` row itself if it survived.
- **A `delete_agent` unpurged DM** — `detail` starts `delete_agent <agent_id>:
  dm-cascade peer probe for session <session_id> failed`. **The orphan query
  above will not find this one**, and that is not a bug in either: the DM
  `sessions` row deliberately survives, because the probe could not prove the
  other participant was gone and purging on an unproven absence is the one
  thing this site refuses to do (#1246). Nothing is orphaned and nothing is
  lost — a DM session simply was not collected. The `detail` names the session
  id, so start there rather than with a query. To sweep for it generally, list
  the DM sessions with `SELECT id, context_id FROM sessions WHERE context_id
  LIKE 'dm:%'` and check each `context_id`'s two named participants against
  `agents`; delete only those where **neither** participant still exists, in
  the same FK order as above. If either is still live the session is correct as
  it stands and there is nothing to do.
- **A Telegram context-id migration drop** — `detail` starts
  `telegram context-id migration:`. There is no next read here either: the
  migration runs once per agent at channel startup
  (`alms-gateway/src/gateway.rs`, "Phase 2b"), so a repaired row is not
  revisited until the next one. Apply the rename in the same statement that
  repairs the row — `UPDATE sessions SET context_id =
  'telegram_<agent_name>_<chat_id>' WHERE id = ...` — rather than waiting for
  a restart to do it.

`persistence_fields_degraded_total` (#1246) counts durable **columns** a parser
could not read and replaced with a fallback, **keeping the row**, with
`persistence_fields_degraded_by_field` giving the per-field breakdown. Each one
is logged at `warn!` with a `field` key and a `detail` naming the row and the
consequence. The keys are `<table>.<column>`, so the key *is* the `sqlite3`
query you need.

It is deliberately **not** part of `persistence_rows_skipped_total`. That
counter means "rows the daemon cannot see"; these rows are perfectly visible,
just wrong — and folding them together would destroy both numbers. The four
fields, **worst first**:

- **`session_summaries.last_run_id`** — the one to alert on. This column is not
  attribution; it is the compare-and-swap sentinel for episodic-summary
  upserts. Degraded to null, every future summary write for that session takes
  the `WHERE last_run_id IS NULL` branch, matches nothing, and comes back as a
  **conflict** — so the agent burns three LLM summarization calls, gives up,
  and logs `Failed to persist session summary due to concurrent updates` when
  there is no concurrent update. **Episodic memory for that session is stuck
  permanently**, and the error names the wrong cause. It is also the only one
  of the four on a live read path, so it is the only one whose counter can
  climb between restarts.
- **`agents.name`** — `DELETE /agents/{id_or_name}` could not read an agent
  name, so a DM session was left uncleaned. Two shapes, same counter: the
  deleted agent's own name was unreadable and the whole **DM-cleanup pass was
  skipped**, or a *peer's* name was unreadable so that one DM session could not
  be classified and was deliberately left alone rather than risk purging a live
  peer's conversation. Either way the delete itself succeeded, and any shared
  DM session whose participants are now all gone survives as an unreachable
  row, along with its `messages`, `audit_events`, and `context_summaries`.
  `detail` is prefixed `delete_agent <agent_id>:`, following the same
  write-path convention as the row-skip counter.
- **`runs.job_id`** — the run is hydrated with no job, so it is no longer
  attributable to the job that spawned it: `GET /runs` reports `job_id: null`
  and the run's trigger is labelled `user` instead of `scheduled`. **Nothing is
  left running** — this parser is only reached by the boot-time stale-run sweep
  and by hydration, both of which see terminal rows, so a degraded `job_id`
  cannot hide an active run from `DELETE /jobs/{job_id}`.
- **`runs.parent_run_id`** — the run reads as top-level instead of as a
  subagent run of its parent, with a null `parent_session_id` breadcrumb.
  Subagent attribution in the UI and in `GET /runs/{id}` is wrong for that row.

**Remediation.** For `session_summaries.last_run_id`, repair the cell and the
next episodic-summary load picks it up without a restart. Find them with
`SELECT agent_id, session_id, last_run_id FROM session_summaries WHERE
last_run_id IS NOT NULL AND last_run_id NOT GLOB '[0-9a-f]*-*-*-*-*'`, then
either `UPDATE ... SET last_run_id = '<the run that produced this summary>'` or
`SET last_run_id = NULL`. Null is safe here and unsticks the session: it puts
the row on the same branch the upsert already takes, and this time the `WHERE
last_run_id IS NULL` predicate matches.

For the two `runs.*` fields, repairing the cell **takes effect at the next
start, not the next request**. The `Run` in the live registry is never
refreshed from disk, and the only production readers of this parser are the
boot sweep and hydration — so the running daemon keeps serving the degraded
value however many times you fix the row. Repair it anyway, then restart when
convenient: `UPDATE runs SET job_id = '<uuid>' WHERE run_id = '<id>'`, or `SET
job_id = NULL` if the job is gone and detaching the run is what you actually
want (same end state, but *believed on purpose*). Find them with `SELECT
run_id, job_id FROM runs WHERE job_id IS NOT NULL AND job_id NOT GLOB
'[0-9a-f]*-*-*-*-*'`.

For `agents.name` there is no next read: the delete already committed and
nothing re-runs it. Finish it by hand — find unreachable DM sessions with
`SELECT id, context_id FROM sessions WHERE context_id LIKE 'dm:%'` and drop the
ones whose named participants no longer appear in `agents`, deleting dependent
rows in FK order (`context_summaries`, `session_summaries`, `audit_events`,
`messages`, `run_tool_calls`, `runs`, then `sessions`). Repair the name cells
first — `SELECT id, typeof(name) FROM agents WHERE typeof(name) <> 'text'` —
or the next delete hits the same branch, and DM purging stays suppressed for
*every* agent while any one name is unreadable. **If that query returns no
rows, the readability check itself failed rather than finding a bad cell** —
it fails closed, so an I/O or corruption error on `agents` reports the same
"could not be proven readable" as a genuine BLOB name does. Look for a SQLite
error in the same log window instead of chasing a cell that is fine.

Like the row-skip counter, these count **occurrences, not distinct rows**, but
how fast that accumulates differs by field.
`session_summaries.last_run_id` sits on a live read path and is re-counted on
every episodic-summary load. The two `runs.*` fields move roughly once or twice
per boot (the sweep runs from `Gateway::new` and again inside hydration) and
never per request. `agents.name` is a write path, and it increments **on calls
that then fail and roll back**, since the counter fires inside the transaction.
Its two shapes accumulate at different rates: the deleted agent's own
unreadable name is one increment per `DELETE /agents/{id}` call, but the peer
arm records inside the per-DM-candidate loop, so a single corrupt name anywhere
in `agents` adds one increment for **every** DM session the deleted agent had.
A jump of ten from one delete is one bad cell, not ten.

> These counters are currently visible only on this endpoint — there is no CLI
> subcommand and no UI surface for `/operations/metrics`, so scrape it or
> `curl` it.

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
  "version": "0.2.4",
  "provider": "openrouter",
  "model": "z-ai/glm-5.2",
  "base_url": "https://openrouter.ai/api/v1",
  "max_tokens": 4096,
  "posture": "guarded",
  "context_strategy": "truncate",
  "stream_chunk_timeout_secs": 180,
  "enabled_tools": ["datetime", "echo", "fs_edit", "fs_glob", "fs_grep", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"],
  "agent_id": "<uuid>",
  "agents": [{"name": "main", "id": "<uuid>", "is_default": true, "model": null, "needs_bootstrap": false}],
  "workspace_dir": "./.alms/workspace",
  "llm_providers": ["anthropic", "gemini", "openai", "openrouter"],
  "context": {
    "strategy": "truncate",
    "max_input_tokens": 100000,
    "compact_trigger_pct": 0.80,
    "compact_retain_pct": 0.40,
    "summary_model": "google/gemma-4-31b-it",
    "summary_provider": "openrouter",
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
    "enabled": ["echo", "fs_edit", "fs_glob", "fs_grep", "fs_list", "fs_read", "fs_write", "http_get", "math", "shell_exec", "invoke_agent", "read_subagent_session", "workspace_write", "list_my_sessions", "read_session", "send_message", "list_agents", "read_messages", "ignore_message"]
  },
  "security": {
    "allow_full_os_access": []
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

Partially update server-level configuration at runtime. The `context`, `session`, `tools`, and `llm` (#809) sections are mutable, as are the top-level `model` / `provider` keys (#1148). Changes take effect on the next run; in-flight runs are unaffected. Logging requires a restart and is not accepted here.

> **Live-mutation propagation — HTTP path only.** `PATCH /settings` mutations to all four mutable sections (`context`, `session`, `tools`, `llm`) and to the top-level `model` / `provider` pair propagate to the **next `POST /runs` immediately**, with no daemon restart required. Telegram-triggered runs read from a boot-time snapshot of the `AgentConfig` and `LlmClient` held inside the `Gateway` and continue to use that snapshot until the daemon is restarted. This is pre-existing behaviour for the `context` / `session` / `tools` sections; the `llm` block (#809) and the server-default pair (#1148) inherit the same limitation. Tooling and frontends building on `PATCH /settings` should treat HTTP and Telegram as separate propagation domains.
>
> **Persistence — `settings.json` wins over `alms.toml` on restart.** Once any field has been PATCHed, the entire mutable surface is written to `{data_dir}/settings.json` and that file is the source of truth on the next boot. Subsequent edits to the corresponding sections of `alms.toml` are silently overwritten by the persisted snapshot. To revert a PATCHed value to a TOML- or env-var-driven configuration, edit `settings.json` directly (or remove it) before restart.

Within `tools`, only `shell_policy`, `sandbox_root`, `timeout_secs`, and `max_output_bytes` are dynamically mutable. **`tools.shell_permissions` is configured in `alms.toml` only** — its allow/deny regex patterns are compiled once at startup and baked into each `ShellTool` instance (see `docs/agent-runtime-design.md` for the config schema and `docs/security-model.md` § 4.3 for the policy semantics). This applies to every field in the block: `allowed_commands`, `denied_commands`, and `classifier_overrides` are all config-file-only and are **not** PATCH-mutable. Sending `shell_permissions` in a `PATCH /settings` body is ignored; restart the gateway to pick up new patterns.

#### Top-level `model` / `provider` — the server-default LLM pair

`model` and `provider` sit at the top level of the PATCH body (not nested under `llm`), mirroring the `[llm]` section of `alms.toml` and the top-level `model` / `provider` keys `GET /settings` returns. They are the **server default** — the bottom layer of the two-layer precedence chain. Agents carrying a per-agent `model` / `provider` on their registry record (`PATCH /agents/{id}`) are unaffected by changes here; agents without one pick the new pair up on their next run.

Since #1148 this pair is **live**. An accepted PATCH commits it, rebuilds the shared `LlmClient` that `POST /runs`, the scheduler, peer-DM triggers, subagent spawns and completion notifications all resolve from, and persists it to `settings.json` for restart survival. The response is a plain `{ "status": "ok" }` — there is no `restart_required` flag on this endpoint any more. Runs already executing keep the client they resolved at start.

Validation. The pair is committed **all or nothing**: if any rule below fails, neither `model` nor `provider` lands — not on the live client, not in `settings.json`. A body naming both keys that is rejected for one of them therefore leaves the running daemon exactly as it was, and the error explains which half was at fault. This holds for a rejection on *either* half, in either direction: `{"provider": "typo", "model": "gpt-4o"}` does not commit the model, and `{"provider": "anthropic", "model": ""}` does not commit the provider (nor `[llm.providers.anthropic].model`, which the body never named). A body whose two halves are *both* invalid gets one error each.

The guarantee is structural rather than case-by-case: the handler validates the entire would-be post-patch pair first and only then commits, with a single gate between the two phases, so no rule can be evaluated after a commit it should have prevented.

| Rule | Failure |
|------|---------|
| `provider` must be a key in `[llm.providers]` | 422, `provider '<name>' is not configured — known providers: [...]` |
| `model` / `provider` must be non-empty when present | 422, `empty string not accepted` — there is no clear sentinel; "no server default" is not a runnable state |
| The post-patch `(provider, model)` pair must be wire-compatible | 422, `INCOMPATIBLE_MODEL_FOR_PROVIDER` |
| `[context].max_input_tokens` must fit the post-patch pair's context window | 400, `INVALID_TOKEN_BUDGET_FOR_PROVIDER` (see § 10.2 budget validation) |

The compatibility rule is the same `model_belongs_to_kind` gate the runtime applies to per-agent provider switches (#860 / #863 / #942): `anthropic` wires accept `claude-*`, `gemini` wires accept `gemini-*` / `models/gemini-*`, and `OpenAiCompatible` wires accept everything (OpenRouter routes vendor-prefixed slugs from every namespace). It fires in both directions — on a provider-only PATCH whose surviving model belongs to the old namespace, and on a model-only PATCH whose new model cannot be spoken by the provider that stays in force. Supply both keys in one body for a cross-namespace switch.

The `[llm.providers]` map itself is **config-file-only** and is not PATCH-mutable; add or edit `[llm.providers.<name>]` in `alms.toml` and restart before switching to a new provider.

> **Partial failure across sections — the pair can outlive its own 422.** "All or nothing" scopes to the pair, not to the whole body. A request that mixes a valid `model` / `provider` with an invalid *other* section (a bad `context.strategy`, say) is rejected with `422` / `status: "partial"`, but the pair has already been committed and applied to the live client by then — only persistence is gated on a globally clean request, which is the same contract the `context` / `session` / `tools` / `llm` sections have always had. The operator-visible consequence is specific to this pair being live: **the running daemon uses the new model while `settings.json` still holds the old one, so a restart silently reverts it.** If you meant the switch to stick, re-send `model` / `provider` on their own and check for a `200`.

Concurrent `PATCH /settings` requests are serialised server-side, so two overlapping switches cannot interleave into a pair neither of them asked for.

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
  "model": "claude-sonnet-4-6",
  "provider": "anthropic",
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
- SSE endpoints (`/runs/{id}/events`, `/sessions/{id}/events`, `/agents/{id}/events`, `/events/session-activity`) also accept `?token=<token>` query parameter, since the browser `EventSource` API cannot set custom headers
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
