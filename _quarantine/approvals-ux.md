# ALMS Approvals UX (minimal spec)

Approvals are where ALMS becomes *real*: they are the interface between autonomous execution and human intent.

This document defines the minimal approval UX + data contract for MVP.

**See also:**
- `docs/security-model.md` (capabilities/scopes/guardrails)
- `docs/events-and-audit.md` (approval events + invariants)
- `docs/api.md` (approval API surface, run event stream)
- `docs/policy-reasons.md` (reason codes shown in approvals)

---

## 0) Goals

- Make dangerous actions **obvious** before execution.
- Make decisions **fast** (approve/deny in seconds, not minutes).
- Prevent “approval fatigue” by offering **progressive trust** (allow once → allow for session → saved rule).
- Ensure every approval is **auditable** and **reproducible**.

Non-goals (MVP):
- complex policy editor UI
- org/team approval workflows

---

## 1) Approval posture (user setting)

ALMS supports a posture setting (per agent / session / job):
- `guarded` (default): approvals may be required
- `full_control`: no approvals (still enforces capabilities/scopes/limits/audit)

In `full_control`, `approval_required` must never be emitted. (See invariants in `docs/events-and-audit.md`.)

---

## 2) What triggers an approval

An approval is triggered when:
- policy evaluates a requested action as `approval_required`

Common triggers (examples):
- `shell.exec` outside workspace
- risky commands (`rm`, `chmod`, `curl | sh`, package installs)
- writing outside allowed paths
- outbound network to non-allowlisted domains
- creating/modifying jobs
- reading secrets

---

## 3) What the user sees (approval card)

An approval request must render a single “approval card” with:

### 3.1 Summary (one line)
- “Allow shell command?” / “Allow file write?” / “Allow network request?”

### 3.2 The exact action
For shell:
- argv array shown exactly (no pretty-printed shell string)
- working directory
- timeout
- output limits

For fs write:
- target path(s)
- operation (create/overwrite/append)

For network:
- method + URL
- resolved host (if available)

### 3.3 Why the agent wants this
- short explanation (1–2 sentences)

### 3.4 Risk cues (minimal)
- show a warning banner when action is destructive/broad

### 3.5 Options (buttons)
Buttons must be explicit:
- **Approve once**
- **Deny**

Optional but recommended (MVP+):
- **Approve for this session**
- **Create rule** (saved allowlist)

---

## 4) Decision semantics

### Approve once
- grants approval for this single invocation only

### Approve for session
- temporary grant (TTL) for the session_id

### Create rule (saved)
- creates/updates policy rule that matches a scope pattern

Rule matching should be conservative and understandable.

---

## 5) Event + API integration

### 5.1 Run event stream
When approval is needed, the run stream emits:
- `policy_decision` with decision=`approval_required`
- `approval_required`

When user decides:
- `approval_resolved`

Then either:
- tool runs (`tool_start`/`tool_end`), or
- run finishes with a clear error if denied

### 5.2 Approval object shape (suggested)
```json
{
  "approval_id": "<uuid>",
  "status": "pending",
  "session_id": "<uuid>",
  "run_id": "<uuid>",
  "capability": "shell.exec",
  "scope": {"cwd":"workspace","argv":["git","status"]},
  "request": {"tool":"shell_exec","params":{}},
  "reason": "requires_user_approval",
  "created_at": "...",
  "resolved_at": null,
  "decision": null
}
```

---

## 6) Audit requirements

Every approval must generate audit records:
- approval requested (pending)
- approval resolved (approve/deny)

Audit should capture:
- principal
- capability + scope
- exact request
- decision

---

## 7) Anti-footgun rules

1) Never approve “raw shell string” if you can show argv.
2) Always show cwd and affected paths.
3) Default decision should not be “approve”.
4) Deny must be one click.
5) Approval UI must not leak secrets.

---

*Authored by Mesut (2026-02-11).* 
