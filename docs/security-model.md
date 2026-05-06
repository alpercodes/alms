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

<a id="43-configurable-shell-permissions-shell_permissions"></a>
### 4.3 Configurable shell permissions (`shell_permissions`)

Operators can attach a configurable policy gate in front of every shell
invocation via `[tools.shell_permissions]` in `alms.toml`:

```toml
[tools.shell_permissions]
allowed_commands = ["^(git|cargo|npm)\\b"]
denied_commands  = ["git\\s+push\\s+.*--force", "^rm\\s+-rf\\s+/"]
# Operator-only: narrow regex that bypass the built-in risk classifier.
# Use sparingly — overrides are audited via `classifier_override_hit` logs.
classifier_overrides = ["^sudo apt-get update$"]
```

Patterns are regex strings matched against the full command string (no
anchoring unless the pattern includes `^`/`$`). Invalid regex patterns
are logged as warnings and skipped; empty/whitespace patterns are
silently skipped. An empty `ShellPermissions` (the default) is a no-op
and preserves backward compatibility.

**Defence chain (issue #745).** Each shell invocation passes through
four gates, in order. The classifier is a non-bypassable *floor* for
destructive findings; a permissive allowlist does not silently disable
it.

1. **Permissions admission gate** — `CompiledPermissions::check_command`:
   1. **Deny wins.** If any `denied_commands` pattern matches, the
      command is blocked with a generic
      `"Command blocked by security policy"` error. The specific pattern
      is only logged server-side to avoid leaking regex hints to a
      potentially adversarial LLM.
   2. **Allowlist mode.** If `allowed_commands` is non-empty, the
      command must match at least one allow pattern; otherwise it is
      rejected with `"does not match any allowed command pattern"`.
   3. **Denylist-only mode.** If `allowed_commands` is empty, any
      command that did not match a deny pattern is admitted.
2. **Classifier override check** —
   `CompiledPermissions::matching_classifier_override`: if the command
   matches any `classifier_overrides` pattern, step 3 is skipped. Every
   hit is logged with `reason=classifier_override_hit`. Overrides
   **cannot** weaken the deny list (step 1) or the OS-level sandbox
   (step 4).
3. **Built-in risk classifier** — `classification::enforce`:
   destructive findings (`rm -rf /`, `mkfs`, `sudo`, `curl | sh`, fork
   bombs, etc.) block in every mode **except** `ClassificationMode::Off`.
   Moderate findings are controlled by mode:
   - `Off` — classifier never blocks. True opt-out (use only when
     another layer such as Landlock or a restricted OS user provides a
     hard boundary). `classify()` still runs so findings emit `debug!`
     lines and the `allowlist_classifier_divergence` `warn!` described
     below still fires — operators keep visibility into
     classifier/allowlist disagreement even when the classifier itself
     is non-blocking.
   - `Warn` — moderate findings logged, not blocked; destructive
     findings still blocked. **Behaviour tightened in #745** (prior to
     v0.2.2, `Warn` blocked nothing; deployments that relied on
     `Warn` + permissive allowlist to run destructive commands must
     either switch to `Off` or enumerate the commands in
     `classifier_overrides`).
   - `BlockDestructive` (default) — destructive blocked, moderate
     logged.
   - `Strict` — both destructive and moderate blocked.
4. **Hardcoded destructive denylist + Landlock** — `exec.rs`:
   unconditional substring denylist (`rm -rf /`, `mkfs.`, fork bombs)
   plus Linux 5.13+ Landlock filesystem sandbox. Operators who supply
   `denied_commands` extend this built-in list; they cannot weaken it.

**Observability.** When an allowlist accepts a command that the
classifier flags at moderate-or-worse (i.e. step 1 admits a command
that step 3 would flag), the tool emits a structured
`tracing::warn!` with `reason=allowlist_classifier_divergence`,
`classifier_level`, and `classifier_finding_count`. Operators who see
repeated divergence warnings may want to tighten their allowlist or
enumerate specific commands in `classifier_overrides` (making operator
intent auditable) rather than leaving classification-borderline
commands to a permissive `.*` pattern.

**What the classifier floor is *not*.** It is not a security boundary
— the classifier is substring/heuristic-based and can be bypassed by a
sufficiently motivated attacker (writing a script to disk + `chmod +x`
+ exec in two steps, base64-decoded shell exec, etc.). The floor is
defence-in-depth that catches the common cases a permissive operator
config would otherwise miss. Real isolation requires Landlock on Linux,
a dedicated OS user, or a container boundary — application-level
denylists are fundamentally bypassable.

**Scope and lifetime.** Permissions (including `classifier_overrides`)
are compiled once (at gateway startup, agent creation, and the
`with_project_root` / `with_unrestricted_filesystem` /
`with_shell_default_env` re-registration paths) into a
`CompiledPermissions` struct baked into the `ShellTool` instance. They
are **not** mutable via `PATCH /settings` — restart the gateway to
pick up new patterns. The `classifier_overrides` field is operator-only
(TOML-only) and is never exposed in any request/response schema or
JSON tool-call parameter. See
[`docs/api.md` § 10.2](api.md#102-update-server-settings) for the API
contract.

**Scope today: global only.** `[tools.shell_permissions]` is configured
once in `alms.toml` and inherited unchanged by every agent. There is no
per-agent or per-subagent override surface at the moment — `AgentRecord`,
`CreateAgentRequest`, `UpdateAgentRequest`, and `SubagentRecordConfig`
do not carry a `shell_permissions` field, and no TOML or registry path
feeds per-agent permissions into the runtime. A merge helper
(`ShellPermissions::merge_with` in `alms-core/src/config/types.rs`)
exists for future use but is not reachable from production code paths
today; it applies union semantics to `denied_commands` and
`classifier_overrides` and replace semantics to `allowed_commands`.

**Subagent inheritance.** When the coordinator builds a subagent
config, it clones the parent's raw `ShellPermissions` config into the
child's `AgentConfig` (see `crates/alms-coordinator/src/lib.rs`). The
child then re-compiles those patterns into its own
`CompiledPermissions` during tool registration. The net effect is that
subagents run under the same policy as their parent; recursive
invocations cannot escape it.

<a id="filesystem-sandboxing"></a>
### 4.4 Filesystem sandboxing (implemented)

#### Single sandbox root — the project root

Every agent runs with **one** filesystem sandbox root, and that root is
the project directory by default. Both file tools (`fs_read`,
`fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob`) and the `shell`
tool enforce against this same root: paths must resolve under it after
symlink canonicalization, and the shell's persistent cwd defaults to
it.

This is a deliberate change from the pre-#945 layout, where the agent's
metadata directory under `workspace_dir/<agent>/` was the sandbox root
and the project tree was effectively off-limits. Agents now operate on
the project the way an operator does: their primary workspace is the
project, and their identity files (`personality.md`, `goals.md`,
`memories.md`, `user.md`) live at
`<project_root>/.alms/agents/<name>/` — naturally inside the sandbox,
so `fs_read('.alms/agents/<sibling>/personality.md')` resolves under
the primary root by construction. There is no separate sibling-reads
extras list to maintain.

The project root is resolved at startup with this precedence: CLI
`--project` flag, `ALMS_PROJECT_ROOT` env var, then
`std::env::current_dir()` as the fallback. The path is canonicalized
before being pinned so Windows `\\?\` prefix mismatches do not trip
the `starts_with` comparison. **Fail-soft:** if canonicalization fails
the as-is path is used with a `WARN`; `with_project_root` never
silently widens the sandbox to "none."

The legacy `tools.sandbox_root` and `tools.shell_policy` config knobs
are still parsed (they configure the `AgentRuntime::new` initial fs/
shell registration), but the gateway's run lifecycle calls
`with_project_root(project_root)` immediately after — so the effective
sandbox boundary every run sees is always the project root unless
[`[security].allow_full_os_access`](#operator-escape-hatch-allow_full_os_access)
or the agent's per-agent
[worktree mode](#opt-in-worktree-mode) overrides it. In other words,
in normal operation these two legacy knobs are effectively no-ops:
whatever value they hold in `alms.toml` is overwritten on every run,
so an operator scanning their config should treat them as inert.

**How the prefix check works:**
1. Relative paths are joined to the project root, absolute paths are checked directly.
2. `std::fs::canonicalize()` follows symlinks to get the real path.
3. The canonical path must `starts_with(project_root)` — rejects symlink escapes, `..` traversal, and absolute paths outside the root.
4. For new files (`fs_write`), the nearest existing ancestor is canonicalized and remaining components are appended.

**UNC path blocking**: All file tools (`fs_read`, `fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob`) reject Windows UNC paths (`\\server\share`), extended-length UNC paths (`\\?\UNC\server\share`), and URI-style equivalents (`//server/share`) before any filesystem I/O. This prevents NTLM credential theft via SMB auto-authentication. The check runs on all platforms (not just Windows) because the daemon may be accessed from a Windows client, and forward-slash UNC paths are valid on some Linux SMB configurations. Device namespace paths (`\\.\`) are also blocked by the same check.

**Device path blocking**: All file I/O tools (`fs_read`, `fs_write`, `fs_edit`) block known system device paths (`/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/tty`, `/dev/console`, `/proc/self/fd/0-2` on Unix; `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9` on Windows) and reject non-regular files via `is_file()` check. Both raw and canonicalized paths are checked to prevent symlink-based bypasses.

**Read-before-write guard (`FileStateCache`):** `fs_write` and `fs_edit` enforce a read-before-write policy via a per-run `FileStateCache`. Before mutating an existing file, the tool verifies that the agent has previously read it via `fs_read` within the same run. If the file has not been read, the write is rejected with a descriptive error. If the file was read but has been externally modified since (detected via mtime comparison with content-hash fallback), the write is also rejected so the agent re-reads the current version. This prevents agents from blindly overwriting files they have not inspected, reducing the risk of data loss from hallucinated content. New file creation (file does not exist on disk) bypasses the guard. After a successful write or edit, the cache is updated so subsequent mutations pass without requiring a re-read.

#### Sibling-workspace reads (#242)

The single project-root sandbox naturally subsumes the sibling-reads
behaviour from #242: every agent's metadata directory at
`<project_root>/.alms/agents/<name>/` is inside the primary sandbox,
so a parent agent can already `fs_read('.alms/agents/<sibling>/personality.md')`
through normal `fs_read`/`fs_list`/`fs_grep`/`fs_glob` calls — no
separate extras list, no sibling-workspaces root, no asymmetric
ephemeral-subagent rules to track. `fs_write`/`fs_edit`/`workspace_write`
land in the same root, so a write to a sibling's metadata file is
syntactically possible but only ever via the agent's own sandboxed
write path — and `workspace_write` itself is hard-pinned to the agent's
own metadata directory by the tool, not by the sandbox.

In [worktree mode](#opt-in-worktree-mode) the primary root narrows to
the worktree path; the gateway widens the read-family fs_* tools'
extras list with `<project_root>/.alms/agents/` so cross-agent reads
keep working from inside the worktree.

<a id="opt-in-worktree-mode"></a>
#### Opt-in worktree mode (#946)

Worktree mode is a **per-agent** setting stored on the agent's
registry record (`worktree_mode: "off" | "git"`, default `"off"`).
There is no `[agent.worktree]` block in `alms.toml` — operators set it
at agent-create time and toggle it via PATCH:

```bash
# Set at create time
alms agent create my-agent --worktree-mode git

# Flip later
alms agent config my-agent --worktree-mode git
alms agent config my-agent --worktree-mode off
```

When the mode is `git`, the gateway provisions a dedicated git
worktree at `<project_root>/.alms/worktrees/<name>/` on branch
`alms/<name>` and re-pins the sandbox root at the worktree path
instead of the project root. Every `fs_*` and `shell` call from that
agent resolves under the worktree, and the persistent shell cwd
defaults to the worktree.

The worktree is provisioned at agent-create time (or on a `mode: off → git`
PATCH). On non-git projects, both create and PATCH return
`400 WORKTREE_REQUIRES_GIT` and refuse to persist the agent record —
there is no silent fallback to project-root mode. Worktree creation is
idempotent: when the directory and branch already exist (e.g. the
operator nuked the agent and re-created it), the gateway treats the
existing layout as the desired layout. The path is also added to the
parent repo's `.git/info/exclude` so `git status` on the project root
does not flag the worktree as untracked.

To keep cross-agent metadata reads working from inside the worktree,
the gateway pushes `<project_root>/.alms/agents/` onto the agent's
`extra_fs_read_roots` list before re-pinning the sandbox at the
worktree. That list is read-only — the agent can `fs_read` a sibling's
`personality.md` from outside its worktree, but not write to it. The
project root *outside* `.alms/agents/` is not in the extras list, so
worktree-mode agents do NOT have read access to the rest of the
project tree from inside the worktree — that is the whole point of the
mode.

Removal at agent-delete time runs `git worktree remove` followed by
`git branch -D alms/<name>`. The remove refuses on uncommitted changes
unless the operator passes `--force` (CLI) or
`force_worktree_remove: true` (HTTP); force discards both the working
tree and the branch. A `mode: git → off` PATCH is just a remove with
the same semantics.

`[security].allow_full_os_access` takes precedence over worktree mode
(below). The worktree itself stays on disk so the operator can flip
the security knob off later without re-running `git worktree add` —
only the run-time sandbox attachment is bypassed.

**Ephemeral subagents** are not eligible for worktree mode: they have
no registry record and therefore no `worktree_mode` field. They
inherit the parent's effective sandbox root for their `fs_*` tools and
receive a disposable workspace at `{agents_dir}/.ephemeral/{task_id}/`
that is cleaned up after the subagent completes.

<a id="operator-escape-hatch-allow_full_os_access"></a>
#### Operator escape hatch — `[security].allow_full_os_access` (#947)

Operators can opt specific named agents out of the filesystem sandbox
entirely by listing them under `[security].allow_full_os_access` in
`alms.toml`:

```toml
[security]
allow_full_os_access = ["operator-shell", "deploy-bot"]
```

A listed agent's `fs_*` and `shell` tools run with **no path prefix to
enforce** — `fs_read /etc/passwd` works, `shell ls /` returns the
real root. Listed agents are subject only to the OS-level permissions
of the daemon process. Be honest about what this means: listed agents
are unsandboxed.

The two independent operator-policy gates **still apply** to listed
agents:

- **`[tools.shell_permissions]`** allow / deny / classifier_overrides
  (#717, [§ 4.3](#43-configurable-shell-permissions-shell_permissions)).
- **`[tools.shell_classification_mode]`** destructive-command floor
  (#745, same section).

These are layered defense-in-depth. They are independent operator
policy, not part of the filesystem sandbox boundary, and they survive
`allow_full_os_access` precisely because operators sometimes want
"unsandboxed fs but no `rm -rf /`."

**Precedence with worktree mode:** when an agent is listed in
`allow_full_os_access` AND has `worktree_mode = "git"`, the security
list wins — the run executes without any filesystem sandbox even
though the worktree directory remains on disk (the worktree is not
re-provisioned mid-run, the runtime simply does not attach the
sandbox to it). A boot-time WARN fires for every overlapping agent so
the precedence is visible in the daemon log.

**Config-file-only — non-PATCH-mutable.** `PATCH /settings` rejects
any payload referencing the `security` key (including `{ "security": {} }`
and `{ "security": null }`) with `400 SECURITY_KNOB_NOT_PATCHABLE`.
Mixed payloads `{ "llm": {...}, "security": {...} }` reject the entire
request — no partial application. Operators edit the TOML and restart
the gateway. PATCH-mutability would let a compromised auth token
silently widen the blast radius of an existing agent. See
[`docs/api.md` § 10.2](api.md#102-update-server-settings) for the wire
contract.

**Audit signal.** A boot-time WARN fires once per listed agent at
gateway startup, and a per-run WARN fires at every `run_started` for a
listed agent on the HTTP, Telegram, and subagent paths
(`target = "alms.security"`, structured fields `agent_name` +
`allow_full_os_access = true`, plus `worktree_mode` for the overlap
case). Listed agents are auditable from logs alone; an operator
scanning for unsandboxed runs does not have to correlate against the
TOML file.

Ephemeral subagents cannot match the list — they have no name — and
always inherit the parent's effective sandbox root.

#### Shell sandboxing platform asymmetry — be honest about this

The filesystem prefix check (the application-layer
`canonicalize() + starts_with` logic above) runs identically on every
platform. The `shell` tool's filesystem isolation does **not**.

- **Linux 5.13+** — Landlock LSM applies a kernel-level filesystem
  sandbox to every shell child process (see
  [§ 4.5](#45-isolation-roadmap)). The child cannot open files outside
  the configured allow-list of paths regardless of what the command
  string says. This is the only platform where the shell sandbox is an
  OS-enforced boundary.
- **Windows and macOS** — there is no equivalent kernel-level shell
  sandbox. The `shell` tool's path enforcement is **application-layer
  only**: a substring scanner in `shell_exec` looks at the command
  string and rejects invocations that reference paths outside the
  sandbox root, but anything that hides the path (variable
  substitution, base64 / hex decode, a script that the agent first
  writes inside the sandbox and then `bash <script>`s, redirection
  through stdin) bypasses the scanner. Same caveat as the existing
  `command_references_denied_file` check: it catches the obvious
  cases, not a motivated attacker.

For real shell isolation on Windows / macOS, operators should run the
daemon as a low-privilege OS user with filesystem ACLs that limit
access to the project root. See
[§ 4.5 Isolation roadmap](#45-isolation-roadmap) for the longer-term
plan (`bubblewrap`/`nsjail` on Linux, OS-user-based isolation as the
universal answer).

`fs_*` tools have no equivalent platform asymmetry — they go through
the same `canonicalize() + starts_with` check on every OS, and
substring tricks in the path string are pre-empted by canonicalization.

### 4.5a Tool output handling — truncation + spill files (#756 + #851)

Two layered caps prevent oversized tool output from blowing the LLM's
context window, saturating the audit-log SQLite database, or flooding
the SSE stream.

**Layer A — `shell` tool internal spill (#756).** Inside the `shell`
tool, `stdout` / `stderr` longer than 30 KB are truncated to a head + tail
preview and the full pre-truncation bytes are written to
`{data_dir}/shell_output/{run_id}/shell_<call_id>.txt`. The shell tool
returns the *already-truncated* JSON result to the agent loop. The agent
loop sees a 30 KB-bounded JSON value, never the full bytes. The spill
file is readable by the agent's own `fs_read` / `fs_grep` because the
gateway widens the agent's `extra_read_roots` to include the per-run
shell-spill directory.

Configured under `[tools.shell_spill]` in `alms.toml`. Subagents inherit
the policy and write to `{data_dir}/shell_output/sub-{task_id}/` so the
parent's startup retention sweep collects subagent spills the same way
it collects parent spills.

**Layer B — shared in-loop tool-output truncation (#851).** Every
tool's result — `fs_read`, `http_get`, `read_session`, etc., not just
`shell` — is routed through `tool_output_truncate::truncate` before it
lands in:

- the agent loop's live `Vec<LlmMessage>` (so the LLM sees a bounded
  preview)
- the session DB (so context rebuild on the next turn sees the same
  bounded preview)
- the audit log (so the SQLite `audit_events.result` column never
  ingests megabyte-scale rows)
- the `ToolEnd` SSE event (so the SSE stream never ships untruncated
  bytes — the original 32-KB cap covered context-window protection but
  left the SSE/audit paths uncapped pre-#921 review)

Caps oversized outputs at **32 KB / 2000 lines** (whichever fires
first) and writes the full pre-truncation bytes to
`{data_dir}/tool-output/{run_id}/tool_<call_id>.txt`. The agent's
recovery path is byte-perfect — the spill file is the *exact* original
JSON-stringified output. The LLM-visible preview ends with a hint
pointing at the spill file:

```
[The tool output was truncated to 32 KB. Full output saved to:
`tool-output/<run_id>/tool_<call_id>.txt` (...). Use `fs_grep` to
search the full content or `fs_read` with `offset`/`limit` to view
specific sections.]
```

Configured under `[tools.tool_output_truncate]` in `alms.toml`:

```toml
[tools.tool_output_truncate]
enabled = true       # Default: true
max_bytes = 32768    # Default: 32 KB
max_lines = 2000     # Default: 2000
retention_days = 7   # Default: 7
```

**Trust model:**

- Spill files for agent A's tool calls are **only** readable by agent A.
  The gateway widens A's `fs_read`/`fs_list`/`fs_grep`/`fs_glob`
  `extra_read_roots` to include the per-run spill directory, NOT the
  whole `tool-output/` tree. Agent B cannot `fs_read` agent A's spilled
  output because B's `extra_read_roots` only contains B's own per-run
  spill directory.
- Subagents under task ID T live at `tool-output/sub-{T}/` and receive
  that subdirectory as their extra read root. Sibling subagents under
  the same parent cannot read each other's spills.
- The `tool_call_id` is sanitized into the spill filename
  (non-alphanumeric chars replaced with `_`) before path construction,
  so a malicious `tool_call_id` like `../../etc/passwd` cannot escape
  the per-run directory.
- Spill files inherit the daemon process's umask. Operators running on
  a shared host should ensure `{data_dir}` itself is mode `0700` (or
  similar) — `fs_*` tools cannot read outside the agent's allowed roots
  in any case, but the spill files contain raw tool output and should
  not be readable by unrelated OS users.

**Retention:** Both layers run a single retention sweep at gateway
startup that walks `{data_dir}/{shell_output|tool-output}/`, deletes
any file with filesystem `mtime` older than `retention_days`, and
removes empty per-run subdirectories. There is no background ticker —
the sweep is a one-shot cost paid at boot.

When a spill file expires after persistence, the context builder's
`session_msg_to_llm` detects the missing path on the next context
rebuild and rewrites the trailing recovery hint to a `[...retention
period has expired. Only this preview survives.]` notice so the agent
doesn't try to `fs_read` a non-existent path. The truncated head+tail
preview itself stays in the session message indefinitely — only the
recoverability hint is rewritten.

**Operator tuning:** Both `[tools.shell_spill]` and
`[tools.tool_output_truncate]` are config-file-only. They are
deliberately NOT exposed via `PATCH /settings` because tightening or
loosening these caps mid-flight could surprise running agents — operators
edit the TOML and restart the daemon. The same model is applied to
`[tools.shell_permissions]`.

<a id="45-isolation-roadmap"></a>
### 4.5 Isolation roadmap

**Current:** `bash -c` command strings + `env_clear()` + best-effort command denylist + project-root path prefix enforcement for fs tools (the [single sandbox root](#filesystem-sandboxing) pinned by `with_project_root`) + persistent cwd restriction for shell (validated against the same root on each invocation) + **Landlock filesystem sandboxing on Linux 5.13+** (fail-closed: if Landlock is supported but enforcement fails, the command is aborted; only gracefully degrades on kernels without Landlock support).

**Landlock read set — `/etc/passwd` excluded (#743 / #734 item 2):** The Linux Landlock policy grants the child process read access to a small allow-list of system paths (`/usr`, `/bin`, `/lib`, `/lib64`, the dynamic linker config under `/etc/ld.so.*`, `/etc/nsswitch.conf`, `/etc/resolv.conf`, `/etc/localtime`, `/dev/{null,urandom,zero}`, `/proc/self`). `/etc/passwd` is intentionally **not** in the read set: granting it would let a sandboxed agent enumerate every local user on a shared host, which is a valuable reconnaissance step for an attacker who has subverted the agent. The trade-off is that bash's `~user` tilde expansion to *other* users' homes no longer resolves (bash needs `getpwnam` to translate the name to a path), and `ls -l`/`whoami` may print numeric UIDs instead of names. `~/path` for the **current** user still works because bash uses the `$HOME` env var, which doesn't read `/etc/passwd`. Agents that legitimately need user enumeration (rare) can run unsandboxed by setting `tools.shell_unrestricted = true` in `alms.toml`.

**Next — additional OS-level isolation:**
- **Restricted OS user**: run the daemon (or just tool execution) as a low-privilege OS user with filesystem ACLs limiting access to the workspace. Battle-tested, simple, works on all platforms. Requires deployment-time setup (create user, set permissions).
- **`bubblewrap`/`nsjail`**: lightweight containers — new mount namespace with only the workspace visible. Linux-only, external dependency.

**Later:**
- Per-session temp workspaces (ephemeral sandboxes) — **partially implemented**: ephemeral subagents now get a disposable workspace at `<project_root>/.alms/agents/.ephemeral/{task_id}/` that scopes their `fs_*` tools and is cleaned up after the subagent completes. Not yet extended to top-level sessions.
- MicroVMs for high-risk environments
- Platform-specific alternatives: Windows Job Objects, macOS Sandbox profiles

---

## 5) Cronjobs / autonomy safety

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

## 6) Session state + secrets

### Secrets handling
- Never store secrets in plain text transcripts.
- Use a dedicated secrets store (later) or environment-injected secrets.
- Tool outputs that include secrets should be redacted before storing.

### Transcript privacy
- Separate “user-visible messages” from “internal debug traces”.
- Allow disabling debug traces in production.

---

## 7) Audit logging (non-negotiable)

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

## 8) Secure defaults (v1)

Default posture recommendations:
- Project-root file access — **implemented**: every agent runs with the project root as its single sandbox boundary for both `fs_*` and `shell` (#945, see [§ 4.4](#filesystem-sandboxing)). Operators may opt specific named agents out via [`[security].allow_full_os_access`](#operator-escape-hatch-allow_full_os_access) (#947) or narrow them to a per-agent git [worktree](#opt-in-worktree-mode) (#946).
- Shell interface is `bash -c` command strings — **implemented**: `shell` tool wraps commands with `bash -c`; Landlock filesystem restrictions applied on Linux 5.13+
- Shell command denylist (best-effort) — **implemented**: substring-based denylist blocks `rm -rf /`, `mkfs.`, fork bombs, etc.; bypassable, defense-in-depth only
- Configurable shell allow/deny permissions — **implemented**: `tools.shell_permissions` in `alms.toml` provides regex-based `allowed_commands` / `denied_commands` gates evaluated before the hardcoded denylist (deny wins; empty allowlist = denylist-only mode); startup-only, not mutable via `PATCH /settings`. See [§ 4.3](#43-configurable-shell-permissions-shell_permissions).
- Shell classifier as non-bypassable floor (#745) — **implemented**: destructive classifier findings block in every `ClassificationMode` except `Off`; `ClassificationMode::Warn` now blocks destructive (behavioural break from pre-v0.2.2). Operators may exempt specific commands via operator-only `classifier_overrides` regex. See [§ 4.3](#43-configurable-shell-permissions-shell_permissions).
- Shell env cleared — **implemented**: `env_clear()` prevents secret leakage to child processes
- Shell cwd restricted — **implemented**: shell's persistent cwd defaults to the [single sandbox root](#filesystem-sandboxing) (project root by default, worktree path under per-agent [worktree mode](#opt-in-worktree-mode)); explicit `cwd` params are validated against the same root
- Strict output truncation — **implemented**: 30KB stdout/stderr cap with head+tail line preservation; `fs_read` defaults to 2000 lines and a 64 KiB output budget (lowered from 512 KiB in #917 to match prevailing caps and reduce pre-truncation bloat now that the in-loop truncate caps at 32 KB anyway), with a 256 KiB whole-file size gate that fires only on parameter-less calls (passing `offset` or `limit` skips the whole-file gate and falls back to the output-byte budget — see #813 / #901); each line is independently allocation-capped at 256 KiB before being returned, with surplus bytes drained and an inline `[line truncated to N bytes; M bytes discarded]` marker plus a `line_truncated: true` flag on the response (#902 — bounds per-line allocation on pathological single-line inputs once the whole-file gate is bypassed); `fs_grep` shares the same 256 KiB per-line cap via the `builtin::line_cap` module (#913) and surfaces a `truncated_lines` counter on its response so agents can detect partial scans; UTF-8 safe truncation
- No `sudo` — not yet enforced (command denylist not implemented; use OS-level restrictions)
- Network allowlist empty by default — not yet implemented
- Auto-approved tools skip approval in Guarded posture — **implemented**: `datetime`, `echo`, `list_agents`, `list_my_sessions`, `read_session`, `read_messages`, `read_subagent_session` return `is_auto_approved() = true`; all other tools still require user approval
- Cronjob creation requires approval — implemented via Guarded posture
  - **Exception:** When a run is system-triggered — peer-to-peer DMs (via `send_message`), notification runs (e.g., `ConversationEnded`), subagent completions, and scheduled jobs — Guarded posture is automatically overridden to Autonomous via the `is_system_triggered` flag, because there is no human in the loop to approve tool calls (the run would hang indefinitely otherwise). This means a system-triggered run on a Guarded agent can execute tools — including cronjob creation — without approval. The override is safe because `is_system_triggered` is set internally by the gateway's `enqueue_triggered_run` helper and `fire_job_run` function (not controllable via the HTTP `create_run` API), but operators should be aware of this trade-off when configuring agent postures.

---

## 9) Implementation checklist

P0 (before public use):
- [ ] Single capability model, used everywhere
- [ ] Tool registry enforces capability checks
- [x] Shell tool — `bash -c` command strings; best-effort command denylist; Landlock on Linux 5.13+
- [x] Audit log for tool runs + job runs — SQLite-backed audit events
- [ ] Job principal + capability scoping
- [x] Output truncation + sanitization — safe UTF-8 truncation on all tool outputs
- [x] Filesystem sandbox — `canonicalize()` + prefix check, project-root by default with opt-in worktree mode and operator escape hatch (see [§ 4.4](#filesystem-sandboxing))
- [x] Shell env isolation — `env_clear()` prevents secret leakage
- [x] Shell cwd restriction — shell's persistent cwd defaults to the project root (or the agent's per-agent [worktree](#opt-in-worktree-mode) when `worktree_mode = "git"`)

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
