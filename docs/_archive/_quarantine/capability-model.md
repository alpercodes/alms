# ALMS Capability Model (MVP spec)

This document defines the **single canonical capability model** for ALMS. It is the source of truth for:
- what capabilities exist
- how scopes work per capability
- how grants are structured
- how policy evaluation works

**Non-negotiable:** one model, used everywhere (tools, jobs, subagents, approvals).

**See also:**
- `docs/security-model.md` (threat model, principals)
- `docs/events-and-audit.md` (policy_decision events + invariants)
- `docs/approvals-ux.md` (approval triggers + posture)
- `docs/api.md` (runs/sessions/events)
- `docs/policy-reasons.md` (stable reason codes)

---

## 0) Glossary

- **Capability**: a named permission category (e.g. `ShellExec`).
- **Scope**: a structured limiter for a capability (e.g. “only `/workspace/**`”, “only `git *`”).
- **Grant**: assignment of a capability+scope to a principal, optionally expiring.
- **Posture**: approval behavior mode (`guarded` vs `full_control`).
- **Policy decision**: the result of evaluating a request against grants (allow/deny/approval_required).

---

## 1) Goals

- Make privilege explicit and auditable.
- Support progressive trust (scoped grants, expiry, posture).
- Enable safe delegation (subagents inherit strict scoped subsets).
- Keep the set small and composable (avoid a combinatorial permission explosion).
- Make decisions deterministic enough to golden-test.

---

## 2) Capability enum (MVP)

Capabilities are **enums, not strings**. The enum lives in `alms-core`.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    // Filesystem
    FsRead,
    FsWrite,
    FsList,

    // Shell / process
    ShellExec,
    ProcessSpawn,

    // Network
    NetHttp,
    NetDns,

    // Git
    GitExec,

    // Scheduling
    JobsCreate,
    JobsRun,
    JobsModify,

    // Agents
    AgentsSpawnSubagent,

    // Secrets
    SecretsRead,

    // Custom / plugin
    Custom(String),
}
```

### MVP subset (implement first)
Start with the smallest set that still yields a real “agent OS”:
- `FsRead`, `FsWrite`
- `ShellExec`
- `NetHttp`
- `JobsCreate`

Everything else can exist in the enum without being “enabled” by default.

---

## 3) Scope model (capability-specific)

Scopes bound what a capability can do. **No unscoped capability should exist** except in explicit “full control” contexts.

### 3.1 Filesystem
Applies to: `FsRead`, `FsWrite`, `FsList`

```rust
FsScope {
    paths: Vec<PathPattern>,  // e.g. "/workspace/**", "!/workspace/secrets/**"
}
```

### 3.2 Shell execution
Applies to: `ShellExec`

```rust
ShellScope {
    cwd: Option<PathPattern>,   // "/workspace/**"
    argv: Vec<ArgPattern>,      // e.g. ["git","*"]
    timeout_ms: u64,
    max_output_bytes: usize,
}
```

**Important rule:** shell execution should be **argv-based**, not a raw shell string.

### 3.3 Network
Applies to: `NetHttp`, `NetDns`

```rust
NetScope {
    domains: Vec<String>,       // allowlist
    methods: Vec<String>,       // e.g. GET/POST
    block_private_ips: bool,    // SSRF guard (recommended default true)
}
```

### 3.4 Jobs
Applies to: `JobsCreate`, `JobsRun`, `JobsModify`

```rust
JobScope {
    // What the job is allowed to do when it runs.
    run_capabilities: Vec<Capability>,
}
```

### Pattern types
Patterns must be conservative and explainable.

- `Exact("git")`
- `Glob("git")` / `Glob("/workspace/**")`
- `Prefix("/workspace/")`
- `Regex(...)` *(post-MVP; avoid regex in MVP unless necessary)*

---

## 4) Grants

Grants are explicit assignments.

```rust
Grant {
    principal: Principal,
    capability: Capability,
    scope: Scope,
    expires_at: Option<Timestamp>,
}
```

### Principals
Common principals:
- `user:<id>`
- `session:<uuid>`
- `run:<uuid>`
- `job:<uuid>`
- `subagent:<uuid>`

Rule of thumb:
- the more autonomous/persistent the principal (jobs), the tighter the scope.

---

## 5) Posture (approval behavior)

Posture is *not a capability*; it is how ALMS handles `approval_required` decisions.

- `guarded` (default)
  - policy may return `ApprovalRequired` and pause execution
- `full_control`
  - policy must resolve to `Allow`/`Deny` only; no approvals are emitted
  - audit still records everything

This is described in more detail in `docs/events-and-audit.md` and `docs/approvals-ux.md`.

---

## 6) Policy evaluation

Evaluation must be deterministic:

```rust
enum Decision {
    Allow,
    Deny,
    ApprovalRequired { reason: String }
}

fn evaluate(grants: &[Grant], request: &Request, posture: Posture) -> Decision
```

Recommended evaluation order:
1) Filter grants by principal
2) Filter by capability
3) For each candidate grant, check scope match
4) If any match ⇒ `Allow`
5) Else ⇒ `Deny`
6) If a match exists but is marked “approval_required” by heuristic rules and posture=guarded ⇒ `ApprovalRequired`

Emit `policy_decision` event for every tool attempt.

---

## 7) Progressive trust (how permissions evolve)

### Default user grants
- workspace read (maybe)
- no shell
- no network except LLM endpoints

### Session grants
- derived from user grants
- may be temporarily widened (approval once/session)

### Run grants
- strict subset of session grants
- ideally immutable once run starts

### Job grants
- explicit at job creation
- job principal (`job:<id>`) runs with the job’s scoped capabilities

### Subagent grants
- strict subset of parent run
- timeboxed

---

## 8) MVP table (what to implement + how it behaves)

| Capability | Scope | Typical approval trigger (guarded) | Notes |
|-----------|-------|-------------------------------------|------|
| `FsRead` | paths | outside workspace | - |
| `FsWrite` | paths | outside workspace / overwrite | - |
| `ShellExec` | cwd+argv+limits | risky argv / outside cwd | argv only |
| `NetHttp` | domains+methods | non-allowlisted domain | SSRF guard |
| `JobsCreate` | run_capabilities | always | persistent autonomy |

---

## 9) Integration points

### Events (required)
- `policy_decision` emitted before any privileged action.
- `approval_required` only in posture `guarded`.

### Audit (required)
- record every allow/deny/approval in append-only audit log.

### API (MVP)
- posture should be configurable at the session level.
- grants should be inspectable (post-MVP), but audit/events already provide observability.

---

## 10) Testing invariants

Golden tests should assert:
- grant expiry blocks execution
- scope mismatch ⇒ `deny`
- `full_control` posture ⇒ no approvals emitted
- job principal cannot exceed its `run_capabilities`
- subagent grant is strict subset of parent

---

## 11) Open questions

1) Pattern engine choice (globset?) and how to represent negation reliably.
2) Should approval heuristics be part of policy (pure) or a separate “risk engine”?
3) Should `SecretsRead` be treated as a separate secret store interface rather than a capability?

---

*Authored by Mesut (2026-02-11). Polished for clarity and MVP execution.*
