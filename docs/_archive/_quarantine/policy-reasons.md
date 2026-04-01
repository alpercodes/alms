# ALMS Policy Decision Reasons (stable codes)

This document defines stable, human-meaningful reason codes used in:
- `policy_decision.reason` (events)
- audit records (decision explanations)
- approval UX (why approval was required)

These codes must be:
- stable strings (don’t change casually)
- safe to show to users (no secrets)
- specific enough to drive UI (warnings, grouping)

**See also:**
- `docs/events-and-audit.md` (policy_decision event)
- `docs/security-model.md` (threat model)
- `docs/approvals-ux.md` (approval triggers)
- `docs/capability-model.md` (capabilities/scopes)

---

## 0) Format

- lowercase snake_case
- namespaced by category when helpful

Examples:
- `fs.outside_workspace`
- `shell.risky_argv`
- `net.domain_not_allowlisted`

---

## 1) Where reasons appear

### Events
`policy_decision` event payload should include:
- `decision`: `allow | deny | approval_required`
- `reason`: one of the codes below

### Audit
Audit records should include the same `decision` and `reason` (plus redacted details).

### Approvals UX
When `decision=approval_required`, the reason code should map to a user-facing explanation.

---

## 2) Semantics rules (to keep this useful)

1) **One primary reason per decision**
   - Prefer a single reason code; put additional nuance into `details` fields (redacted if needed).

2) **Reason codes must be stable**
   - Changing/removing a code is a breaking change for UI/SDK.

3) **Reason codes must be safe**
   - Never include raw paths, URLs with tokens, secrets, or command output in the reason string.

4) **Reasons are explanations, not proofs**
   - The audit trail + event correlation IDs are the source of truth.

---

## 3) Minimal set for MVP

This is the smallest set that should exist in MVP so UI + logs remain coherent:

- `fs.within_scope`, `fs.outside_scope`, `fs.outside_workspace`
- `shell.within_scope`, `shell.outside_scope`, `shell.outside_workspace`, `shell.risky_argv`, `shell.requires_sudo`
- `net.allowlisted_domain`, `net.domain_not_allowlisted`, `net.private_ip_blocked`
- `jobs.create_requires_approval`, `jobs.scope_too_broad`
- `secrets.read_requires_approval`, `secrets.not_available`
- `policy.missing_grant`, `policy.expired_grant`, `policy.default_deny`, `policy.unexpected_error`

Everything else in this document is **recommended** but can be added incrementally without breaking the contract.

---

## 4) Common reason codes

### 3.1 Filesystem
- `fs.within_scope` — path matches allowed scope
- `fs.outside_scope` — path does not match allowed scope
- `fs.outside_workspace` — path is outside workspace default boundary
- `fs.write_overwrite` — write would overwrite existing content
- `fs.write_destructive` — delete/truncate-like operation

### 3.2 Shell
- `shell.within_scope` — argv + cwd match scope
- `shell.outside_scope` — argv/cwd do not match scope
- `shell.outside_workspace` — cwd outside workspace
- `shell.risky_argv` — matches risky pattern (rm/chmod/curl|sh/etc.)
- `shell.requires_sudo` — contains sudo/elevated intent
- `shell.output_limit_hit` — output was truncated due to max output bytes
- `shell.timeout` — execution hit timeout

### 3.3 Network
- `net.allowlisted_domain` — domain in allowlist
- `net.domain_not_allowlisted` — not in allowlist
- `net.private_ip_blocked` — SSRF guard: private/localhost blocked
- `net.method_not_allowed` — HTTP method not allowed by scope

### 3.4 Jobs / cron
- `jobs.create_requires_approval` — persistent autonomy requires explicit approval in guarded mode
- `jobs.scope_too_broad` — requested job capabilities exceed permitted job scope
- `jobs.disabled` — job is disabled (attempted run denied)

### 3.5 Secrets
- `secrets.read_requires_approval` — secrets are high-risk
- `secrets.not_available` — secret not configured
- `secrets.redacted` — secret was present but redacted in output/storage

### 3.6 Artifacts
- `artifacts.created` — output stored as artifact (large/binary/sensitive)
- `artifacts.redacted` — artifact content was redacted
- `artifacts.retention_expired` — artifact was deleted due to retention policy

### 3.7 Generic / fallback
- `policy.default_allow` — allowed by default rule
- `policy.default_deny` — denied by default rule
- `policy.missing_grant` — no grant exists for capability
- `policy.expired_grant` — grant exists but expired
- `policy.unexpected_error` — policy engine errored (prefer deny)

---

## 4) Mapping to UX

Suggested severity mapping:
- `*_within_scope`, `net.allowlisted_domain`, `artifacts.created` → info
- `*_outside_scope`, `policy.missing_grant`, `net.domain_not_allowlisted` → warn
- `shell.risky_argv`, `shell.requires_sudo`, `secrets.*`, `jobs.*` → warn/error (prompt strong confirmation)

Suggested user-facing copy patterns:
- `shell.risky_argv` → “This command can delete or modify system files.”
- `net.private_ip_blocked` → “Blocked request to private/localhost address (SSRF protection).”
- `policy.missing_grant` → “Agent does not have permission for this action.”

---

## 5) Notes

- Keep this list short. If you feel the urge to add many new reasons, consider whether the scope model should be improved instead.
- Reasons can be composed into analytics later (“top deny reasons”).

---

*Authored by Mesut (2026-02-11). Updated for completeness and UI stability.*
