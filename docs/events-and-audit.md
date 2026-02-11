# ALMS Events & Audit (MVP spec, behavior-shaping)

This document defines:
1) the **event model** (what ALMS emits as it runs)
2) the **audit model** (what ALMS records as an append-only trace)

It is intentionally strict: the event/audit layer shapes agent behavior by making actions **observable, gated, and replayable**.

Spine docs:
- `docs/api.md` (SSE run events)
- `docs/security-model.md` (capabilities, approvals, constraints)
- `docs/tool-sandbox-abi.md` (tool boundary)
- `docs/testing-strategy.md` (golden tests + invariants)

---

## 0) Goals

- Make ALMS observable in real time (UI/SDK can render progress).
- Make ALMS accountable (audit trail for privileged actions).
- Make autonomy safe: **policy decisions and approvals are first-class events**.
- Enable deterministic tests: stable event sequences for scripted agents.
- Avoid redesign later: define shapes that can grow to jobs/subagents/artifacts.

Non-goals (MVP):
- full OpenTelemetry export (add later)
- multi-tenant audit partitioning

---

## 1) Design principles (the “rules of the world”)

1) **Everything important is an event.**
   - If the UI needs to show it, it must be an event.
   - If it’s privileged or risky, it must also be an audit record.

2) **Policy is explicit.**
   - Do not “silently” deny or allow tool actions.
   - Emit policy decisions as events (`policy_decision`, `approval_required`).

3) **At-least-once delivery is fine; idempotency is required.**
   - Streams can drop/reconnect. Event IDs + run IDs allow resumption.

4) **Don’t trust tool output.**
   - Treat tool output as untrusted content; sanitize and bound it.

5) **Human control points must be clear.**
   - Approval-required is a deliberate state, not an error.

---

## 2) Definitions

### Run
A single agent execution request bound to a session.

### Event
A time-ordered record emitted during a run/job/subagent lifecycle.

### Audit record
An append-only record that a privileged action was attempted and what happened.

### Artifact (MVP+)
A stored object referenced by ID (files, large tool outputs, logs).

Important distinction:
- **Events** are for real-time UX and debugging.
- **Audit** is for accountability and post-hoc review.

---

## 3) Correlation identifiers (required)

All events and audit records must include enough identifiers to correlate:
- `session_id`
- `run_id`
- `event_id` (monotonic per run stream)

Optional but recommended:
- `tool_invocation_id`
- `approval_id`
- `job_id`, `job_run_id`
- `subagent_id` / `task_id`
- `artifact_id`

---

## 4) Event envelope

All events share a common envelope:
```json
{
  "event_id": 1,
  "type": "token_delta",
  "ts": "2026-02-11T08:54:00Z",
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "severity": "info",
  "payload": {}
}
```

Fields:
- `severity`: `debug | info | warn | error`
- `payload`: type-specific JSON

Transport mapping:
- SSE: `type` maps to `event: <type>`
- SSE: `event_id` maps to `id:`
- SSE: `payload` maps to `data:`

---

## 5) Run state machine (MVP)

Runs should move through explicit states; events must reflect these transitions.

States:
- `queued`
- `running`
- `waiting_for_approval`
- `succeeded`
- `failed`
- `cancelled`

State/event expectations:
- entering `running` ⇒ emit `run_started`
- entering `waiting_for_approval` ⇒ emit `approval_required`
- leaving `waiting_for_approval` ⇒ emit `approval_resolved`
- terminal states ⇒ emit `run_finished`

---

## 6) Event taxonomy (what kinds of events exist)

### A) User-facing progress events
- `run_started`, `token_delta`, `run_finished`

### B) Tool lifecycle events
- `tool_planned` (optional)
- `tool_start`, `tool_end`

### C) Policy/approval events (behavior shaping)
- `policy_decision`
- `approval_required`
- `approval_resolved`

### D) System/diagnostic events (bounded)
- `warning` (human-meaningful)
- `metric` (optional)

---

## 7) Run event types (MVP)

### 7.1 `run_started`
Emitted once at the beginning.

Payload:
```json
{
  "agent_key": "main",
  "input": {"type":"text","text":"..."}
}
```

### 7.2 `policy_decision` (recommended MVP)
Emitted whenever a tool attempt is evaluated.

Payload:
```json
{
  "capability": "shell.exec",
  "tool": "shell_exec",
  "scope": {"cwd":"workspace","argv":["git","status"]},
  "decision": "allow",
  "reason": "within_workspace_allowlist"
}
```

Decisions:
- `allow`
- `deny`
- `approval_required`

### 7.3 `token_delta`
Streaming assistant output.

Payload:
```json
{ "delta": "text chunk" }
```

Notes:
- If provider is non-streaming, emit one delta with full text.

### 7.4 `tool_planned` (optional MVP)
A lightweight event indicating the model intends to call a tool (before approval).

Payload:
```json
{ "tool": "shell_exec", "params": {} }
```

### 7.5 `approval_required`
Emitted when policy requires human approval.

Payload:
```json
{
  "approval_id": "<uuid>",
  "capability": "shell.exec",
  "scope": {"cwd":"workspace","argv":["git","status"]},
  "request": {"tool":"shell_exec","params":{}},
  "reason": "requires_user_approval"
}
```

### 7.6 `approval_resolved`
Emitted when approval is granted/denied.

Payload:
```json
{ "approval_id": "<uuid>", "decision": "approve" }
```

or
```json
{ "approval_id": "<uuid>", "decision": "deny" }
```

### 7.7 `tool_start`
Emitted immediately before executing a tool.

Payload:
```json
{
  "tool_invocation_id": "<uuid>",
  "tool": "shell_exec",
  "capability": "shell.exec",
  "params": {},
  "limits": {"timeout_ms": 30000, "max_output_bytes": 262144}
}
```

### 7.8 `tool_end`
Emitted after tool completes.

Payload:
```json
{
  "tool_invocation_id": "<uuid>",
  "ok": true,
  "duration_ms": 123,
  "result": {}
}
```

On error:
```json
{
  "tool_invocation_id": "<uuid>",
  "ok": false,
  "duration_ms": 123,
  "error": {"code":"TOOL_FAILED","message":"..."}
}
```

### 7.9 `run_finished`
Emitted once when the run completes.

Payload:
```json
{ "ok": true }
```

or
```json
{ "ok": false, "error": {"code":"INTERNAL","message":"..."} }
```

---

## 8) Job / cron events (MVP+)

Jobs are “autonomy with persistence”. Keep semantics parallel to runs.

### `job_run_started`
Payload:
```json
{ "job_id": "<uuid>", "job_run_id": "<uuid>", "trigger": "schedule" }
```

### `job_run_finished`
Payload:
```json
{ "job_id": "<uuid>", "job_run_id": "<uuid>", "ok": true }
```

---

## 9) Subagent events (post-MVP shape)

Keep the model consistent and parent-correlated:
- `subagent_started` (includes `subagent_id`, `type`, `task`)
- `subagent_progress` (percent + message)
- `subagent_finished` (ok + result summary)

All must include `run_id` (parent) + `subagent_id`.

---

## 10) Event invariants (must always hold)

These invariants shape correct agent behavior:

1) **Monotonicity**: `event_id` strictly increases per run.
2) **Pairing**: every `tool_start` has exactly one `tool_end`.
3) **Approval gating**: if `policy_decision=approval_required`, there must be an `approval_required` before any `tool_start` for that invocation.
4) **No silent privilege**: any tool execution must be preceded by a `policy_decision` event.
5) **Terminal event**: every run ends with `run_finished` (even if failed).

---

## 11) Audit model (append-only)

### 11.1 What must be audited
At minimum:
- every tool invocation (attempt + result)
- every approval request + resolution
- every job run
- policy denials

### 11.2 Audit record envelope
```json
{
  "audit_id": "<uuid>",
  "ts": "...",
  "principal": "session:<session_id>",
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "kind": "tool_invocation",
  "capability": "shell.exec",
  "decision": "allow",
  "request": {},
  "result": {},
  "error": null
}
```

### 11.3 Decisions
- `allow`
- `deny`
- `approval_required`
- `approved`
- `rejected`

### 11.4 Redaction & size limits
Tool params/results can contain secrets or huge outputs.

Rules (MVP):
- truncate large blobs
- redact obvious secrets (best effort)
- store hashes/digests for integrity if full payload stored elsewhere later

Recommended:
- put large outputs in **artifacts** and reference by `artifact_id`.

---

## 12) Persistence strategy for events (MVP stance)

Two viable MVP stances:

A) **Best-effort streaming** (fastest)
- events are not persisted; reconnect loses them

B) **Persisted run event log** (recommended if approvals exist)
- store events per run so:
  - reconnect is possible
  - approvals can resume reliably

If approvals are shipped, prefer (B).

---

## 13) How this ties into testing

Golden tests should assert:
- stable event sequences for scripted LLM + tools
- invariants in §10
- audit records exist for tool invocations + approvals

See: `docs/testing-strategy.md`

---

## 14) Open questions

1) Do we persist events (for reconnect) in MVP, or treat SSE as best-effort?
2) What is the minimal redaction approach that is safe but not too complex?
3) Should audit be stored in SQLite immediately (preferred) or via snapshots for MVP?
4) Do we standardize `agent_key` values now (e.g., `main`) and keep them stable?

---

*Authored by Mesut (2026-02-11). Updated with stronger behavior-shaping semantics.*
