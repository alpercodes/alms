# ALMS Security Model (proposed)

ALMS will run **autonomous agents** that can (sometimes) execute tools with real impact: shell commands, file writes, network calls, scheduling jobs, spawning subagents.

This document defines a security model that makes those capabilities:
- **explicit** (capabilities and scopes)
- **auditable** (append-only log)
- **revocable** (grants expire)
- **bounded** (resource limits, sandboxes)

> Goal: ALMS should feel powerful like OpenClaw, but be designed so “power” doesn’t mean “unsafe by default”.

---

## 0) Threat model (what we’re defending against)

### Accidental harm
- Agents running the wrong shell command
- Overwriting files, deleting data
- Infinite loops / runaway costs
- Cronjobs that keep firing unexpectedly

### Malicious inputs
- Prompt injection via web content / user messages
- Tool output injection (LLM reads tool output and gets manipulated)
- Untrusted plugins/tools

### System risks
- Secrets exfiltration (SSH keys, tokens)
- Lateral movement (LAN scanning)
- Persistence via cronjobs / startup scripts

---

## 1) Core concepts

### 1.1 Principals
Everything is authorized relative to a **principal**:
- user identity (e.g., Telegram user id, local OS user)
- agent identity (main agent id, subagent id)
- job identity (cron job id)

### 1.2 Capabilities
Capabilities are **named permissions**. Keep them few and composable.

Example capability set (v1):
- `fs.read`, `fs.write`
- `shell.exec`
- `net.http`, `net.dns`
- `git.exec`
- `process.spawn`
- `jobs.create`, `jobs.run`, `jobs.modify`
- `agents.spawn_subagent`
- `secrets.read` (special: should be rare)

> Important: unify to **one capability model** across coordinator/runtime/tools.

### 1.3 Scopes
Every capability grant should have a **scope**, not just “allowed/denied”.

Scope dimensions:
- **resource scope**
  - filesystem: path prefixes (workspace-only vs full-disk)
  - network: allowlist domains / CIDRs
  - shell: allowlist commands or denylist patterns
- **time scope**: expires_at / TTL
- **rate limits**: per minute / per job run
- **context scope**: which session/job can use it

### 1.4 Policy engine
A simple, explicit policy evaluation function:

```
allow?(principal, capability, scope, request) -> decision
```

Decision includes:
- allow / deny
- allow-with-approval
- redacted output (for secrets)

Keep policy deterministic and testable.

---

## 2) Approval model (human-in-the-loop)

ALMS should support multiple security postures.

### Modes
- **Safe (default)**: destructive or broad actions require approval
- **Developer**: approvals off or reduced (local dev only)
- **Locked-down**: most tools disabled, allowlist-only

### When to require approval (recommended)
- `shell.exec` outside workspace
- `fs.write` outside workspace
- any `rm`, `chmod`, `chown`, `sudo`, package installs
- network calls to non-allowlisted domains
- creating/modifying cronjobs
- reading secrets

### Approval UI/UX
Approvals must show:
- exact command / action
- cwd + env notes
- files affected (best effort)
- why the agent wants it (LLM-provided explanation)
- “allow once” vs “allow for session” vs “allow always (rule)”

---

## 3) Tool execution security

### 3.1 Tool interface
Tools should have:
- input JSON schema (validation)
- output schema (optional)
- declared required capabilities
- declared risk level

### 3.2 Resource limits
Per tool invocation:
- timeout
- memory cap
- output size cap
- concurrency cap

### 3.3 Output handling / injection resistance
Tool output is untrusted input to the model.

Mitigations:
- wrap tool outputs with clear delimiters
- strip/escape control sequences
- for web fetches: reduce to extracted text, no scripts
- optionally run a “tool-output sanitizer” step

---

## 4) Shell access model (do this carefully)

### 4.1 Do not expose raw bash as a default
Expose a `shell_exec` tool that is policy-gated.

### 4.2 Minimal contract for `shell_exec`
Request fields:
- `argv`: array of strings (no shell interpolation), e.g. `["git","status"]`
- `cwd`: workspace-relative path (validated against sandbox_root in sandboxed mode)
- `timeout_secs`
- `env`: extra key-value pairs (daemon env is cleared via `env_clear()` — secrets never leak)

Response fields:
- `exit_code`
- `stdout` (truncated to 8KB)
- `stderr` (truncated to 8KB)

### 4.3 Filesystem sandboxing (implemented)

**Config** (`alms.toml` or env vars):
- `tools.sandbox_root` (default `"."` = cwd) — all `fs_read`/`fs_write`/`fs_list` paths must resolve within this directory after symlink resolution. Set to `""` for unrestricted access. **Fail-closed:** if the configured path cannot be resolved (typo, missing directory), the runtime refuses to start rather than silently widening access.
- `tools.shell_policy` (default `"sandboxed"`) — controls `shell_exec` cwd restriction:
  - `"sandboxed"`: cwd forced to `sandbox_root`; explicit `cwd` param validated against it.
  - `"unrestricted"`: no cwd restriction (power-user / full root access).

**How it works:**
1. Relative paths are joined to `sandbox_root`, absolute paths are checked directly.
2. `std::fs::canonicalize()` follows symlinks to get the real path.
3. The canonical path must `starts_with(sandbox_root)` — rejects symlink escapes, `..` traversal, and absolute paths outside the root.
4. For new files (fs_write), the nearest existing ancestor is canonicalized and remaining components are appended.

**Known limitation:** `shell_exec` sandboxing only restricts the cwd. The executed command itself (e.g. `cat /etc/passwd`) can still access any file the process user can read. Application-level command denylists are fundamentally bypassable — there are infinite ways to read files or exfiltrate data. True shell isolation requires OS-level mechanisms (see §4.4).

### 4.4 Isolation roadmap

**Current (MVP):** `Command` + argv array (no shell injection) + `env_clear()` + `sandbox_root` path prefix enforcement for fs tools + cwd restriction for shell.

**Next — OS-level isolation:**
- **Landlock** (Linux 5.13+): kernel LSM that lets an unprivileged process restrict its own filesystem access before exec. The `landlock` crate makes this ~30 lines of Rust. Would make `shell_exec` truly sandboxed on Linux. No root required. Cross-platform story: Linux-only; Windows/macOS need alternative approaches.
- **Restricted OS user**: run the daemon (or just tool execution) as a low-privilege OS user with filesystem ACLs limiting access to the workspace. Battle-tested, simple, works on all platforms. Requires deployment-time setup (create user, set permissions).
- **`bubblewrap`/`nsjail`**: lightweight containers — new mount namespace with only the workspace visible. Linux-only, external dependency.

**Later:**
- Per-session temp workspaces (ephemeral sandboxes) — **partially implemented**: ephemeral subagents now get a disposable workspace at `{workspace_dir}/.ephemeral/{task_id}/` that scopes their `fs_*` tools and is cleaned up after the subagent completes. Not yet extended to top-level sessions.
- MicroVMs for high-risk environments
- Platform-specific alternatives: Windows Job Objects, macOS Sandbox profiles

---

## 5) WASM tool security

### 5.1 Why WASM
- reduces risk of arbitrary native code execution
- enforces memory safety and offers deterministic metering

### 5.2 Still not a complete sandbox
If WASM tools can call host functions, the host must enforce:
- capability checks on every host call
- bounds checks and rate limits

### 5.3 ABI requirements
Define one stable ABI:
- how params are passed
- how results are returned
- how tools allocate memory
- error semantics

---

## 6) Cronjobs / autonomy safety

Cron = persistence. Treat it as privileged.

Rules:
- Creating/modifying jobs should usually require approval.
- Jobs run with a dedicated principal: `job:<id>`
- Jobs inherit only the minimum capabilities required.
- Every run is recorded with full audit trail.

Job safety:
- max runtime per run
- max concurrency per job
- backoff on repeated failures
- easy kill switch (`disable job`)

---

## 7) Session state + secrets

### Secrets handling
- Never store secrets in plain text transcripts.
- Use a dedicated secrets store (later) or environment-injected secrets.
- Tool outputs that include secrets should be redacted before storing.

### Transcript privacy
- Separate “user-visible messages” from “internal debug traces”.
- Allow disabling debug traces in production.

---

## 8) Audit logging (non-negotiable)

Every sensitive action must be append-only logged:
- who requested it (principal)
- what was attempted
- what was executed
- result + exit code
- time + duration
- related session/run IDs

Suggested table/event fields:
- `event_id`, `ts`, `principal`, `session_id`, `run_id`
- `capability`, `scope`, `action_type`
- `request_json`, `result_json`
- `decision` (allow/deny/approved)

---

## 9) Secure defaults (v1)

Default posture recommendations:
- Workspace-only file access — **implemented**: `sandbox_root = "."` confines fs tools to cwd
- Shell commands use argv array, not raw shell — **implemented**: `shell_exec` uses `Command::new()` with args, no shell interpolation
- Shell env cleared — **implemented**: `env_clear()` prevents secret leakage to child processes
- Shell cwd restricted — **implemented**: `shell_policy = "sandboxed"` restricts cwd to sandbox_root
- Strict output truncation — **implemented**: 8KB stdout/stderr cap, 32KB fs_read cap, UTF-8 safe truncation
- No `sudo` — not yet enforced (command denylist not implemented; use OS-level restrictions)
- Network allowlist empty by default — not yet implemented
- Cronjob creation requires approval — implemented via Guarded posture
  - **Exception:** When an agent receives a peer-to-peer direct message (`send_message`), Guarded posture is automatically overridden to Autonomous for that run because there is no human in the loop to approve tool calls (the run would hang indefinitely otherwise). This means a DM-triggered run on a Guarded agent can execute tools — including cronjob creation — without approval. The override is safe because `is_peer_message` is set internally by the MessageBus (not controllable via the HTTP API), but operators should be aware of this trade-off when configuring agent postures.

---

## 10) Implementation checklist

P0 (before public use):
- [ ] Single capability model, used everywhere
- [ ] Tool registry enforces capability checks
- [x] Shell tool uses argv (no `bash -lc` by default) — `shell_exec` uses `Command::new()` + args
- [x] Audit log for tool runs + job runs — SQLite-backed audit events
- [ ] Job principal + capability scoping
- [x] Output truncation + sanitization — safe UTF-8 truncation on all tool outputs
- [x] Filesystem sandbox — `canonicalize()` + prefix check, configurable `sandbox_root`
- [x] Shell env isolation — `env_clear()` prevents secret leakage
- [x] Shell cwd restriction — sandboxed mode restricts cwd to `sandbox_root`

P1:
- [ ] Landlock integration (Linux) — kernel-level filesystem restriction for `shell_exec`
- [ ] Restricted OS user deployment guide — document setup for sandboxed daemon user
- [ ] Container/jail execution for shell tools (`bubblewrap`/`nsjail`)
- [x] Secrets store — `data/secrets.json` with optional AES-256-GCM encryption via `ALMS_MASTER_KEY`
- [ ] Secrets redaction in transcripts/audit logs
- [ ] Per-domain network allowlists

P2:
- [ ] MicroVM execution for high-risk tools
- [ ] Signed tool/plugin bundles
- [ ] Platform-specific sandboxing (Windows Job Objects, macOS Sandbox profiles)

---

*Authored by Mesut (2026-02-10).*
