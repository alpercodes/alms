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
  "version": "0.1.0"
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

### 4.1 Create or get a session by context
`POST /sessions:resolve`

**Request**
```json
{
  "agent_key": "main",
  "context_id": "telegram:1853446411"
}
```

**Response 200**
```json
{
  "session_id": "<uuid>",
  "created": true
}
```

Notes:
- `agent_key` is a stable name (e.g. `main`) rather than requiring clients to invent UUID agent ids.
- Internally, ALMS can still map to `AgentId`.

### 4.2 Get session
`GET /sessions/{session_id}`

**Response 200**
```json
{
  "session_id": "<uuid>",
  "agent_key": "main",
  "context_id": "telegram:1853446411",
  "created_at": "2026-02-11T07:00:00Z",
  "last_activity": "2026-02-11T07:52:00Z",
  "status": "active"
}
```

### 4.3 List sessions (optional MVP)
`GET /sessions?agent_key=main&limit=50`

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
  "status": "running",
  "started_at": "2026-02-11T07:52:00Z",
  "ended_at": null
}
```

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

`run_started`
```json
{ "run_id": "<uuid>", "session_id": "<uuid>", "ts": "..." }
```

`token_delta`
```json
{ "run_id": "<uuid>", "delta": "text chunk" }
```

`tool_start`
```json
{
  "run_id": "<uuid>",
  "tool_invocation_id": "<uuid>",
  "tool": "shell_exec",
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
    "tool": "shell_exec",
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

#### Reconnect (post-MVP)
Support `Last-Event-ID` to resume streaming without losing events.

### 5.4 Cancel a run (optional MVP)
`POST /runs/{run_id}:cancel`

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

**Request**
```json
{
  "name": "daily-summary",
  "schedule": { "kind": "cron", "expr": "0 9 * * *", "tz": "Europe/Berlin" },
  "target": { "kind": "session", "session_id": "<uuid>" },
  "payload": { "kind": "run", "input": {"type":"text","text":"Summarize yesterday"} },
  "capabilities": ["net.http"],
  "enabled": true
}
```

### 7.2 Run job now
`POST /jobs/{job_id}:run`

### 7.3 List job runs
`GET /jobs/{job_id}/runs`

---

## 8) Audit (MVP placeholder)

MVP may store audit records in-memory or alongside snapshots, but the shape should be defined.

Planned:
- `GET /audit?session_id=<uuid>&limit=100`

Audit records should align with `docs/security-model.md`.

---

## 9) Auth (optional MVP)

If ALMS is local-only, auth may be omitted.

If exposed beyond localhost, add:
- `Authorization: Bearer <token>`
- single shared token in env/config

---

## 10) Open questions (to resolve early)

1) Do we commit to `POST /runs` + `GET /runs/{id}/events` for MVP, or keep a shorter `/agent/run/stream` and alias it?
2) What is the canonical `agent_key` set (e.g. `main`, `planner`, …)?
3) What is the minimal approval surface we’re willing to ship in MVP?
4) Do we need run persistence for reconnect in MVP (store events), or is best-effort streaming acceptable?

---

*Authored by Mesut (2026-02-11). Updated after rethinking API fit for ALMS goals.*
