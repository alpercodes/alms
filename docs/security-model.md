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

### Auto-approved tools
Certain inherently safe, read-only tools bypass the approval gate entirely — even in Guarded posture. These tools implement `is_auto_approved() -> true` in the `Tool` trait. The current auto-approved set is: `datetime`, `echo`, `list_agents`, `list_my_sessions`, `read_session`, `read_messages`, `read_subagent_session`. All other tools (including `shell`, `fs_write`, `invoke_agent`, etc.) still require approval in Guarded posture.

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

### 4.2 Minimal contract for `shell` (renamed from `shell_exec`)

The shell tool's interface is command strings executed via `bash -c`.

Request fields:
- `command`: shell command string, executed via `bash -c` (both Unix and Windows via Git Bash / WSL)
- `description`: brief description of what the command does (for audit logging)
- `timeout_ms`: timeout in milliseconds (default 120000, max 600000)
- `run_in_background`: when `true`, returns a task_id immediately; use `check_task` to poll results
- `check_task`: check the result of a background task by its task_id
- `env`: extra key-value pairs (daemon env is cleared via `env_clear()` — secrets never leak)

The working directory persists across calls. After each command, the tool
appends a `pwd` marker to detect cwd changes; the new cwd is stored in
the shell state for the next invocation.

A best-effort command denylist blocks obviously destructive patterns
(`rm -rf /`, `mkfs.`, fork bombs, etc.). This is substring-based and
fundamentally bypassable; it is defense-in-depth, not a security boundary.

Response fields:
- `exit_code`
- `stdout` (truncated to 30KB with head+tail line preservation)
- `stderr` (truncated to 30KB with head+tail line preservation)

### 4.3 Configurable shell permissions (`shell_permissions`)

Operators can attach a configurable policy gate in front of every shell
invocation via `[tools.shell_permissions]` in `alms.toml`:

```toml
[tools.shell_permissions]
allowed_commands = ["^(git|cargo|npm)\\b"]
denied_commands  = ["git\\s+push\\s+.*--force", "^rm\\s+-rf\\s+/"]
```

Patterns are regex strings matched against the full command string (no
anchoring unless the pattern includes `^`/`$`). Invalid regex patterns
are logged as warnings and skipped; empty/whitespace patterns are
silently skipped. An empty `ShellPermissions` (the default) is a no-op
and preserves backward compatibility.

**Evaluation order (inside `CompiledPermissions::check_command`):**
1. **Deny wins.** If any `denied_commands` pattern matches, the command
   is blocked with a generic `"Command blocked by security policy"`
   error (the specific pattern is only logged server-side, to avoid
   leaking regex hints to a potentially adversarial LLM).
2. **Allowlist mode.** If `allowed_commands` is non-empty, the command
   must also match at least one allow pattern; otherwise it is rejected
   with `"does not match any allowed command pattern"`.
3. **Denylist-only mode.** If `allowed_commands` is empty, any command
   that did not match a deny pattern is permitted.

**Relationship to the hardcoded denylist.** The `shell_permissions`
check runs inside `ShellTool::execute()` *before* the hardcoded
destructive-command denylist in `alms-sandbox/src/shell/security.rs`.
That hardcoded list (blocking `rm -rf /`, `mkfs.`, fork bombs, etc.)
is unconditional and always applied — it remains as defense-in-depth
behind the configurable policy. Operators who supply `denied_commands`
extend the built-in list; they cannot weaken it.

**Scope and lifetime.** Permissions are compiled once (at gateway
startup, agent creation, and the `with_workspace` / `with_shell_default_env`
re-registration paths) into a `CompiledPermissions` struct baked into
the `ShellTool` instance. They are **not** mutable via `PATCH /settings`
— restart the gateway to pick up new patterns. See `docs/api.md`
§ 10.2 for the API contract.

**Scope today: global only.** `[tools.shell_permissions]` is configured
once in `alms.toml` and inherited unchanged by every agent. There is no
per-agent or per-subagent override surface at the moment — `AgentRecord`,
`CreateAgentRequest`, `UpdateAgentRequest`, and `SubagentRecordConfig`
do not carry a `shell_permissions` field, and no TOML or registry path
feeds per-agent permissions into the runtime. A merge helper
(`ShellPermissions::merge_with` in `alms-core/src/config/types.rs`)
exists for future use but is not reachable from production code paths
today.

**Subagent inheritance.** When the coordinator builds a subagent
config, it clones the parent's raw `ShellPermissions` config into the
child's `AgentConfig` (see `crates/alms-coordinator/src/lib.rs`). The
child then re-compiles those patterns into its own
`CompiledPermissions` during tool registration. The net effect is that
subagents run under the same policy as their parent; recursive
invocations cannot escape it.

### 4.4 Filesystem sandboxing (implemented)

**Config** (`alms.toml` or env vars):
- `tools.sandbox_root` (default `"."` = cwd) — all `fs_read`/`fs_write`/`fs_list`/`fs_edit`/`fs_grep`/`fs_glob` paths must resolve within this directory after symlink resolution. Set to `""` for unrestricted access. **Fail-closed:** if the configured path cannot be resolved (typo, missing directory), the runtime refuses to start rather than silently widening access.
- `tools.shell_policy` (default `"sandboxed"`) — controls `shell_exec` cwd restriction:
  - `"sandboxed"`: cwd forced to `sandbox_root`; explicit `cwd` param validated against it.
  - `"unrestricted"`: no cwd restriction (power-user / full root access).

**How it works:**
1. Relative paths are joined to `sandbox_root`, absolute paths are checked directly.
2. `std::fs::canonicalize()` follows symlinks to get the real path.
3. The canonical path must `starts_with(sandbox_root)` — rejects symlink escapes, `..` traversal, and absolute paths outside the root.
4. For new files (fs_write), the nearest existing ancestor is canonicalized and remaining components are appended.

**UNC path blocking**: All file tools (`fs_read`, `fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob`) reject Windows UNC paths (`\\server\share`), extended-length UNC paths (`\\?\UNC\server\share`), and URI-style equivalents (`//server/share`) before any filesystem I/O. This prevents NTLM credential theft via SMB auto-authentication. The check runs on all platforms (not just Windows) because the daemon may be accessed from a Windows client, and forward-slash UNC paths are valid on some Linux SMB configurations. Device namespace paths (`\\.\`) are also blocked by the same check.

**Device path blocking**: All file I/O tools (`fs_read`, `fs_write`, `fs_edit`) block known system device paths (`/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/tty`, `/dev/console`, `/proc/self/fd/0-2` on Unix; `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9` on Windows) and reject non-regular files via `is_file()` check. Both raw and canonicalized paths are checked to prevent symlink-based bypasses.

**Read-before-write guard (`FileStateCache`):** `fs_write` and `fs_edit` enforce a read-before-write policy via a per-run `FileStateCache`. Before mutating an existing file, the tool verifies that the agent has previously read it via `fs_read` within the same run. If the file has not been read, the write is rejected with a descriptive error. If the file was read but has been externally modified since (detected via mtime comparison with content-hash fallback), the write is also rejected so the agent re-reads the current version. This prevents agents from blindly overwriting files they have not inspected, reducing the risk of data loss from hallucinated content. New file creation (file does not exist on disk) bypasses the guard. After a successful write or edit, the cache is updated so subsequent mutations pass without requiring a re-read.

**Read-only sibling workspace access (#242):** When a named agent is attached via `with_workspace()`, its `fs_read`/`fs_list`/`fs_grep`/`fs_glob` tools gain an additional read-only root at the parent of the agent's workspace directory — i.e. the top-level `workspace_dir` that contains every named agent's workspace as a sibling. This lets a parent agent read another agent's `personality.md`/`goals.md`/`memories.md` without being able to modify them. `fs_write`/`fs_edit`/`workspace_write` do NOT receive the extra root, so the write boundary stays tight. The deny-list (`secrets.json`) and UNC/device-path blocks still apply everywhere; symlinks are canonicalized before the allow-list check, so a symlink inside a sibling workspace cannot escape to `/etc/passwd` etc. Recursive walkers (`fs_grep`, `fs_glob`) run with `follow_links(false)` so symlinks are filtered before file collection.

The current implementation does not track parent/child invocation relationships, so *any* named agent attached via `with_workspace()` can read *any* other named agent's workspace files — the trust model is "all named agents share a read-only view of each other," consistent with the existing agent registry (named agents are already shared across parents). Narrowing this to direct-invocation only would require dynamic extras tracking; the current plumbing supports shrinking `sibling_workspaces_root` in the future without breaking the API.

**Ephemeral subagent isolation:** Ephemeral subagents live at `{workspace_dir}/.ephemeral/{task_id}/` and receive `{workspace_dir}/.ephemeral/` as their extra read root (the parent of their own workspace), so they cannot see top-level named-agent workspaces. Note the asymmetry: named agents' extra root is `{workspace_dir}/`, which includes `{workspace_dir}/.ephemeral/`, so a named agent *can* read into the ephemeral tree of an in-flight subagent. Task-ids are UUIDs, so enumeration is not a practical exfiltration path, and ephemeral workspaces are cleaned up when the subagent completes, but operators should be aware that the boundary is asymmetric: ephemeral cannot see named, but named can see ephemeral.

**Known limitation (non-Linux):** On platforms without Landlock support (Windows, macOS, older Linux kernels), shell sandboxing only restricts the cwd. The executed command itself (e.g. `cat /etc/passwd`) can still access any file the process user can read. Application-level command denylists are fundamentally bypassable. On Linux 5.13+, Landlock filesystem restrictions are applied to child processes (see section 4.5).

### 4.5 Isolation roadmap

**Current:** `bash -c` command strings + `env_clear()` + best-effort command denylist + `sandbox_root` path prefix enforcement for fs tools + persistent cwd restriction for shell (validated against sandbox root on each invocation) + **Landlock filesystem sandboxing on Linux 5.13+** (fail-closed: if Landlock is supported but enforcement fails, the command is aborted; only gracefully degrades on kernels without Landlock support).

**Next — additional OS-level isolation:**
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
- Shell interface is `bash -c` command strings — **implemented**: `shell` tool wraps commands with `bash -c`; Landlock filesystem restrictions applied on Linux 5.13+
- Shell command denylist (best-effort) — **implemented**: substring-based denylist blocks `rm -rf /`, `mkfs.`, fork bombs, etc.; bypassable, defense-in-depth only
- Configurable shell allow/deny permissions — **implemented**: `tools.shell_permissions` in `alms.toml` provides regex-based `allowed_commands` / `denied_commands` gates evaluated before the hardcoded denylist (deny wins; empty allowlist = denylist-only mode); startup-only, not mutable via `PATCH /settings`. See § 4.3.
- Shell env cleared — **implemented**: `env_clear()` prevents secret leakage to child processes
- Shell cwd restricted — **implemented**: `shell_policy = "sandboxed"` restricts cwd to sandbox_root; persistent cwd validated against sandbox on each invocation
- Strict output truncation — **implemented**: 30KB stdout/stderr cap with head+tail line preservation, fs_read line-based limits (default 2000 lines, 512KB output budget, 256KB file size guard), UTF-8 safe truncation
- No `sudo` — not yet enforced (command denylist not implemented; use OS-level restrictions)
- Network allowlist empty by default — not yet implemented
- Auto-approved tools skip approval in Guarded posture — **implemented**: `datetime`, `echo`, `list_agents`, `list_my_sessions`, `read_session`, `read_messages`, `read_subagent_session` return `is_auto_approved() = true`; all other tools still require user approval
- Cronjob creation requires approval — implemented via Guarded posture
  - **Exception:** When a run is system-triggered — peer-to-peer DMs (via `send_message`), notification runs (e.g., `ConversationEnded`), subagent completions, and scheduled jobs — Guarded posture is automatically overridden to Autonomous via the `is_system_triggered` flag, because there is no human in the loop to approve tool calls (the run would hang indefinitely otherwise). This means a system-triggered run on a Guarded agent can execute tools — including cronjob creation — without approval. The override is safe because `is_system_triggered` is set internally by the gateway's `enqueue_triggered_run` helper and `fire_job_run` function (not controllable via the HTTP `create_run` API), but operators should be aware of this trade-off when configuring agent postures.

---

## 10) Implementation checklist

P0 (before public use):
- [ ] Single capability model, used everywhere
- [ ] Tool registry enforces capability checks
- [x] Shell tool — `bash -c` command strings; best-effort command denylist; Landlock on Linux 5.13+
- [x] Audit log for tool runs + job runs — SQLite-backed audit events
- [ ] Job principal + capability scoping
- [x] Output truncation + sanitization — safe UTF-8 truncation on all tool outputs
- [x] Filesystem sandbox — `canonicalize()` + prefix check, configurable `sandbox_root`
- [x] Shell env isolation — `env_clear()` prevents secret leakage
- [x] Shell cwd restriction — sandboxed mode restricts cwd to `sandbox_root`

P1:
- [x] Landlock integration (Linux) — kernel-level filesystem restriction for shell commands (fail-closed)
- [ ] Restricted OS user deployment guide — document setup for sandboxed daemon user
- [ ] Container/jail execution for shell tools (`bubblewrap`/`nsjail`)
- [x] Secrets store — `.alms/secrets.json` with optional AES-256-GCM encryption via `ALMS_MASTER_KEY`
- [ ] Secrets redaction in transcripts/audit logs
- [ ] Per-domain network allowlists

P2:
- [ ] MicroVM execution for high-risk tools
- [ ] Signed tool/plugin bundles
- [ ] Platform-specific sandboxing (Windows Job Objects, macOS Sandbox profiles)

---

*Authored by Mesut (2026-02-10).*
