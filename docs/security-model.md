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
- `command`: shell command string, evaluated as bash syntax by a child process. Under the default `system-bash` engine this is `bash -c` (on Windows via Git Bash, resolved from well-known install locations or derived from `git.exe` on `PATH`; overridable with the config-file-only `[tools].shell_path` knob. WSL's `System32\bash.exe` launcher is hard-rejected — if no Git Bash is found the tool fails with an actionable error instead of silently executing under WSL). See [§ 4.2a](#42a-shell-engines-shell_engine) for the opt-in `builtin` engine
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

<a id="42a-shell-engines-shell_engine"></a>
### 4.2a Shell engines (`[tools].shell_engine`, #1143)

The config-file-only `shell_engine` knob selects *which* bash-syntax
interpreter evaluates the command. It is not mutable via
`PATCH /settings` and has no env-var override; the active engine is
logged at boot at `target = "alms.security"`.

- **`system-bash` (default)** — `bash -c <command>` against the system
  bash resolved as described in [§ 4.2](#42-minimal-contract-for-shell-renamed-from-shell_exec).
  Pre-#1143 behavior, unchanged.
- **`builtin` (opt-in, experimental)** — the daemon re-execs its **own
  binary** (`std::env::current_exe()`, zero `PATH` or install-location
  resolution) as the hidden `alms shell-host` subcommand, which
  evaluates the command via the embedded `brush_core` Rust bash
  interpreter. The command string travels over the child's **stdin**
  (read to EOF before evaluation), not argv, sidestepping platform
  quoting rules and command-line length limits. The host dispatches
  before any logging/config/DB initialization — it never loads
  `alms.toml`, never opens the database, and never touches auth state.
  Profile and rc loading are explicitly skipped, so behavior can never
  depend on operator dotfiles.

Security posture of `builtin` relative to `system-bash`:

- **Still a child process.** Landlock `pre_exec`, `kill_on_drop`, the
  operator timeout (a single deadline covering both the stdin write and
  the wait — a child that never drains its stdin is still killed at the
  deadline), env scrubbing (`env_clear()` + secret filter:
  `ALMS_AUTH_TOKEN`, `ALMS_MASTER_KEY`, and provider API keys never
  reach the child), the pwd-marker wrapper, the destructive-command
  classifier, and `shell_permissions` all apply unchanged — zero delta
  vs. `system-bash`.
- **Landlock grant on the ALMS binary (Linux).** For the re-exec'd
  child to start under Landlock, the ruleset adds a **file-scoped**
  `PathBeneath` grant (read + execute) on the ALMS binary itself — the
  binary's file fd, not its parent directory, with the same rights the
  ruleset already grants to all of `/usr`. On a standard
  `/usr/local/bin/alms` install the binary was *already* exec-able
  inside the sandbox under the `/usr` grant, so this is **not a new
  access class** — it is parity for layouts where the binary lives
  elsewhere (e.g. `target/debug`, `~/bin`). The grant is added only
  when `shell_engine = "builtin"`; under `system-bash` the ruleset is
  byte-identical to before. The binary is not setuid, and any nested
  ALMS invocation from inside the sandbox inherits the Landlock domain
  (Landlock propagates across fork/execve), so it stays fs-confined.
- **No bundled coreutils.** brush interprets bash *syntax* only —
  `grep`/`sed`/`awk`/`tail`/... remain external commands the shell
  spawns from `PATH` like any bash would. On Windows without Git's
  `usr/bin` on `PATH`, those externals may be missing entirely:
  `builtin` removes the bash-*resolution* dependency, not the coreutils
  dependency.
- **Exit code 126 = host failure.** When the shell host itself fails
  (cannot read the command from stdin, interpreter construction fails),
  it exits with 126 — the POSIX "command invoked cannot execute"
  convention — and an `alms shell-host:`-prefixed stderr diagnostic. A
  user command that itself exits 126 ("found but not executable") is
  distinguished by the absence of that stderr prefix. Host failure also
  emits no pwd marker, so the persistent cwd is never corrupted by a
  failed host.

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
the project directory by default. The file tools (`fs_read`,
`fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob`) *enforce* it:
a path must resolve under the root after symlink canonicalization or
the call is refused. The `shell` tool is **pinned** to it — that root
is its starting cwd, and a cwd that leaves it is reverted after the
fact — but the shell does not inspect paths inside the command. What
actually confines a shell child is Landlock on Linux 5.13+, and nothing
on Windows or macOS. That difference is the most important thing in
this section; see *Shell sandboxing platform asymmetry* below before
relying on it.

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

**Shell cwd containment (#1255):** the `shell` tool keeps a persistent
working directory across calls, re-reading it after every command from a
`pwd` marker appended to the command line. That reported string is *not*
in the same form as the pinned sandbox root: the Git-Bash engine reports
MSYS form (`/c/dev/ws`), the builtin engine reports native Windows form
(`C:\dev\ws`), and `std::fs::canonicalize` produces extended-length form
(`\\?\C:\dev\ws`). Both sides are therefore normalised — MSYS drive paths
rewritten to drive form, `\\?\` stripped, symlinks resolved — before a
component-wise containment test that folds ASCII case on Windows.
Containment has always been component-wise — `Path::starts_with` matches
whole components, so a sibling like `ws-evil` was never treated as inside
`ws`, and no prefix-matching vulnerability was ever shipped. What #1255
fixed is the *form* mismatch: the two sides reached that comparison in
incompatible spellings of the same directory, sharing no leading
components at all, so the sandbox rejected its own root. The MSYS
rewrite is applied only
under `cfg(windows)` and only when the rewritten path names a real
directory: on Unix `/c/...` is an ordinary absolute path and is never
reinterpreted. A cwd that fails containment is discarded and the previous
cwd retained, so a `cd` out of the sandbox cannot persist into the next
command. The revert is also **reported to the agent** (#1262): the tool
result's `stdout` gains a `[cwd unchanged: '<attempted>' <verdict>;
subsequent commands still run in '<kept>']` line, on both the foreground and
background (`check_task`) paths, and the sandboxed variant of the tool
description states the confinement up front. Containment itself is
unchanged — this is a correctness fix for the agent loop, which previously
saw `exit_code: 0` and no signal at all, and went on misreading every
relative path. The notice does not appear for `unrestricted` /
`[security].allow_full_os_access` instances, which skip containment
entirely.

`<verdict>` distinguishes the two ways containment can fail, because the
check itself does not prove the same thing in both. `canonical_for_comparison`
falls back to returning its input when `std::fs::canonicalize` fails, so a
path the daemon *could not resolve* is rejected by exactly the same
`is_within` comparison as a path that genuinely escaped. Both fail closed —
that part is deliberate — but only one of them establishes where the
directory actually is:

| Outcome | Condition | `<verdict>` |
|---|---|---|
| `OutsideRoot` | root **and** candidate both canonicalised, and the candidate is not under the root | `is outside the sandbox root` |
| `NotVerifiable` | either side failed to canonicalise, so the comparison decided nothing | `could not be confirmed inside the sandbox root` |

The distinction is load-bearing rather than pedantic: under Windows Git Bash,
a sandbox root reached through the `/tmp` mount is unresolvable on *every*
command (#1266), so collapsing the two cases would tell an agent its own
legitimate workspace was out of bounds, every turn, with a cause the daemon
never determined. A confidently wrong explanation delivered repeatedly is
worse for the agent loop than no explanation, which is the failure this
notice exists to fix. Making the rejection *behaviour* depend on the reason —
rather than only the wording — is tracked separately in #1261.

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

`workspace_read` (#1310) is pinned the same way, and the argument that
matters is the parity with its own sibling rather than the contrast with
`fs_read`: `workspace_write` was **already** tool-pinned to that one
metadata directory, and the read half is pinned by the same mechanism, so
adding it widened nothing. That form of the claim survives a deployment
where `fs_read` is disabled entirely, which the contrast does not.

The mechanism is that there is no caller-controlled component in the
path at all: the `file` parameter accepts only the four literals
`personality` / `goals` / `memories` / `user`, and the read joins a
construction-time workspace `dir` with a `&'static str` filename. So
`workspace_read` cannot reach a sibling's metadata even though `fs_read`
can — a tool addressed *by name* is strictly narrower than one addressed
by path.

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

**Force-true reversibility asymmetry.** If a `DELETE /agents` or a
`PATCH /agents` `mode: git → off` flip is invoked with `force = true`
and the underlying SQLite write subsequently fails, the gateway runs a
best-effort compensation that restores the agent's `alms/<name>` branch
and worktree directory at the SHA snapshotted before the remove (#1019,
#1022). Committed history is reversible. **Uncommitted working-copy
changes at the moment of the force-remove are not** — `git worktree
remove --force` discards them before the gateway ever sees the persist
failure, so there is nothing left to restore on the working-copy side.
Operators who care about uncommitted state should commit before issuing
a force DELETE / git→off flip, or omit `--force` so the remove refuses
on a dirty tree.

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

**A listed name is reachable by the model, not only by the operator.**
The subagent call site passes `request.subagent_name` to
`SecurityConfig::is_full_os_access_agent`, and that name originates as
the string the LLM supplied to `invoke_agent`. It is *folded* before it
gets there — `canonicalize_subagent_name` mutates the request field in
place, taking the registry spelling when a row exists and
`to_ascii_lowercase()` when none does — and the list is then matched
case-insensitively.

Folding does not narrow the grant. `validate_agent_name` admits only
1–64 characters of ASCII letters, digits and interior hyphens, and it
gates every path that can produce a name reaching the comparison — CLI
create, `POST /agents`, and the LLM-supplied `invoke_agent` name, which
is rejected outright when it fails. So every spelling an agent can
legally emit folds onto one lowercase form, and every entry that could
name a real agent is reached by it. Listing `deploy-bot` therefore does
not scope the grant to a registered agent called `deploy-bot` — **any**
agent that emits `invoke_agent(name: "Deploy-Bot")` gets an unsandboxed
subagent, registered or not.

Treat every entry on this list as a name any agent in the fleet can
claim, and pick a name a model is unlikely to guess. Two things will not
do that for you:

- **Unusual capitalisation.** Casing folds on both sides, so it buys
  nothing.
- **Characters outside the agent-name class.** `SecurityConfig::validate`
  rejects only empty and whitespace-only entries, so `deploy_bot`,
  `deploy.bot` or `Déploy` load without complaint — and can never match,
  because every name that reaches the comparison has been through
  `validate_agent_name`. That is a **dead entry**, not a narrower grant:
  the operator believes they granted full OS access and granted nothing.
  Keep entries inside the agent-name class.

#### Shell sandboxing platform asymmetry — be honest about this

The `fs_*` prefix check (the application-layer
`canonicalize() + starts_with` logic above) runs identically on every
platform — `check_sandbox_path` in
`crates/alms-sandbox/src/builtin/mod.rs` is not `cfg`-gated, so there is
no Windows/macOS weakening there. That is the boundary that *does* hold
everywhere.

The `shell` tool is a different story, and the asymmetry is narrower and
worse than "application-layer instead of kernel-level":

- **The `shell` tool does not check paths in the command, on any
  platform.** The last check that looked at command *arguments* was a
  hardcoded denied-filename list, removed in the old tracker's #744 with
  nothing to replace it. The module docs in
  `crates/alms-sandbox/src/shell/security.rs` record the removal and the
  reasoning: a substring scan over a shell command string is bypassed by
  a symlink, variable expansion, command substitution, an encoded name,
  or a renamed copy, so it projected a posture it could not deliver.
  Do not go looking for one — `command_references_denied_file`, cited by
  earlier revisions of this section, does not exist.
- **The only shell-side path logic is post-hoc cwd containment**
  (`crates/alms-sandbox/src/shell/pathnorm.rs`). After a command runs,
  the shell reports its final working directory; if that directory does
  not normalise to something under the sandbox root, the persistent cwd
  is **reverted** and a `[cwd unchanged: …]` notice is appended to
  stdout. It runs *after* the command. `cd /etc && ls` returns the
  `/etc` listing to the model and then resets the cwd — the test in
  `crates/alms-sandbox/src/shell/mod.rs` pins exactly that ("the
  command's own output must survive intact"). This is cwd-drift
  correction plus a signal to the agent, not a boundary.
- **Linux 5.13+** — Landlock LSM applies a kernel-level filesystem
  sandbox to every shell child process (see
  [§ 4.5](#45-isolation-roadmap)). The child cannot open files outside
  the configured allow-list regardless of what the command string says.
  This is the only platform where the shell has a filesystem boundary at
  all.
- **Windows and macOS** — there is **no filesystem boundary on `shell`**.
  A command can read and write anything the daemon's OS user can. What
  remains is the `[tools.shell_permissions]` regex list and the
  destructive-command classifier
  ([§ 4.3](#43-configurable-shell-permissions-shell_permissions)) — both
  operator policy over command *strings* (`rm -rf /`, `mkfs.`, `dd if=`),
  not path controls. Defeating the classifier is not what gets an agent
  out of the project root; nothing is holding it in.

**Landlock degrades open on an unsupported kernel.**
`apply_landlock_sandbox` (`crates/alms-sandbox/src/shell/exec.rs`) builds
its ruleset inside `pre_exec`. If the first step,
`Ruleset::default().handle_access(...)`, fails — an older kernel, or a
container/seccomp profile that blocks the Landlock syscalls — it prints
`[alms] Landlock not supported by kernel, running unsandboxed` and
returns `Ok(())`, and the command runs with no filesystem restriction.
Every *later* failure (ruleset creation, adding a rule, `restrict_self`,
a `NotEnforced` status) is a hard error that refuses to run the command,
so the degrade-open window is exactly "this kernel cannot do Landlock at
all". Two consequences an operator needs:

- **The 5.13+ floor is load-bearing.** On an older kernel, or in a
  container that blocks the syscall, a Linux deployment has
  Windows-grade shell containment — not a degraded version of the Linux
  one.
- **The notice is an `eprintln!` to the child's stderr, not a
  `tracing::warn!`.** It does not reach structured logs, so a log
  scanner will never see it. Verify Landlock by hand on an unfamiliar
  kernel or inside a container.

For real shell isolation on Windows / macOS — and on Linux below 5.13 —
run the daemon as a **low-privilege OS user with filesystem ACLs** that
limit it to the project root. That is currently the only containment
available for `shell` on those platforms. See
[§ 4.5 Isolation roadmap](#45-isolation-roadmap) for the longer-term
plan (`bubblewrap`/`nsjail` on Linux, OS-user-based isolation as the
universal answer).

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

### Subagent session readback (#1181 / PR #1185)
- The `subagent_` session `context_id` prefix is **coordinator-reserved**: only
  `derive_subagent_identity` (alms-coordinator) may mint contexts with this
  prefix, and the access rule treats the shape as a trusted ownership record.
  Both shapes embed the spawning parent's agent id —
  `subagent_{parent_agent_id}_{name}` (named, #1051) and
  `subagent_{parent_agent_id}_{task_id}` (ephemeral, #1181/#1185).
  - Reserved **by convention, not by enforcement**: `POST /sessions` and
    `GET /sessions/{agent_id}/{context_id}` `get_or_create` on a client-supplied
    `context_id` with no prefix validation, so a context like `subagent_notes`
    is creatable today. Since #1298 such a context is `UnrecognizedShape` and
    therefore denied to its own agent, where it previously read back via
    `read_session`'s `agent_id` match. That is fail-closed and consistent with
    the rest of the product — `classify_session_type` already calls any
    `subagent_` prefix a subagent, so the session is already internal, excluded
    from `list_sessions` and read-only in the UI. Stated here because this is
    the bullet that asserts "reserved": the prefix is now also *denied*, which
    is the enforcement the mint side still lacks.
- The reserved shape now has a **second consumer**: `parse_subagent_context`
  (alms-core, #1277) recovers the session's display owner from it for the
  `agent_name` field on session envelopes. A change to the context format must
  visit both it and the access rule — one decides who may read the transcript,
  the other decides whose name is rendered on it. The parser's named arm is
  `validate_agent_name`-gated, which also bounds the model-supplied name before
  it reaches the UI as a label.
- **The access rule is stated once**, in `alms_core::subagent_session_access`
  (#1298): *a subagent session belongs to the parent named in its `context_id`,
  never to the agent whose id it happens to be filed under.* Both
  `read_subagent_session` and `read_session` call it, and `delete_agent`'s
  cascade reads the same ownership out of `parse_subagent_parent`. Before
  #1298 the two tools each carried their own copy of the belief and had
  drifted into opposite answers about the same bytes (see the #1278 bullet
  below); a new consumer must call the shared rule rather than re-derive it.
- Ephemeral subagent transcript reads by `session_id` are **ownership-checked
  by parent id, not bearer-capability**: knowing the session UUID is NOT
  sufficient. The UUID intentionally leaks beyond the spawning parent (it
  appears in parent-visible `invoke_agent` results / completion notifications
  and, for DM-triggered invocations, on the shared DM session where the peer
  sees it), and `read_subagent_session` is registered auto-approved for every
  agent — so the tool only serves a session whose embedded parent id equals
  the calling agent's id. Legacy ephemeral contexts without the embedded
  parent id (`subagent_{task_id}`, pre-v0.2.4 hardening) are denied outright.
- Since #1278 a **named** subagent session's `agent_id` is the *invoked*
  agent's registry id, not the invoking parent's. It is deliberately **not**
  an authorization input: ownership is read only out of the `context_id`,
  which still embeds the spawning parent. Authorizing on `session.agent_id`
  would grant the transcript to the agent the work was delegated *to* rather
  than *by* — and, since every parent invoking the same **registered** named
  subagent files under that one registry id, it would grant that agent *every*
  parent's delegations, not only its own. The grant is to the delegate; other
  parents are reached only if the delegate relays. The embedded parent is the
  only field that separates two parents' delegations to the same agent.
  - On the other two arms `session.agent_id` names nobody at all — an
    unregistered name files under `AgentId::deterministic(parent, name)` and an
    ephemeral subagent under a fresh `AgentId::new()`, ids no agent holds — so
    an `agent_id` check grants no one there, the parent included. The
    pre-#1298 over-grant therefore existed on the **registered-name arm only**,
    which bounds what was exposed.
  - `read_session` authorized on `session.agent_id` and so did admit the
    invoked agent to its own subagent transcripts. Latent rather than open, on
    two counts: `list_my_sessions` filters `subagent` contexts out, so there
    was no supported way to learn the id; and the delegate holds `read_session`
    only while running a **gateway** run — the coordinator's subagent runtime
    registers none of the agent tools (`alms-coordinator`'s only
    `register_tool` call is inside `mod tests`), so a subagent run could never
    read anything back. It needed the delegate to *also* be an agent someone
    chats with, crons, or DMs. #1298 routes `read_session` through
    `subagent_session_access` instead, so both tools now grant the spawning
    parent and refuse everyone else.
  - One consequence for operators: the two tools are now a single access
    surface under two names. `ToolRegistry`'s `enabled_filter` applies to every
    registration, dynamic ones included, and the two names are separately
    listable in `tools.enabled` — so an allowlist naming `read_session` but not
    `read_subagent_session` used to leave the parent with no door onto a
    subagent transcript and now provides one. The check is identical either
    way; only reachability changed. The default (`tools.enabled` empty = all
    enabled) is unaffected.
- The same move puts the invoked agent's registry id on the subagent run's
  **context build**, which is a different question from tool authorization
  because the context builder has no per-agent boundary at all. Episodic
  summaries are therefore **not loaded on any subagent run** (#1278): without
  that gate, `load_session_summaries(agent_id)` — which filters on `agent_id`
  alone — would inject the invoked agent's summaries of its own operator
  chats, Telegram threads, DMs and scheduled jobs into a context whose output
  returns verbatim to the invoking parent as the `invoke_agent` result, with
  no tool call involved. The gate restores symmetry with the write side, which
  has never produced a `session_summaries` row for a `subagent_` context
  (`derive_source_label` returns `None` and both writers early-return).
- Ownership for **deletion** reads out of the same place authorization does.
  `delete_agent`'s cascade selects a subagent session by the parent embedded
  in its `context_id`, never by the `agent_id` column it is filed under, so
  deleting the invoked agent cannot destroy the invoking parent's transcripts,
  runs or audit events — and deleting the parent takes them even though they
  are filed elsewhere. `DELETE /agents/{id_or_name}` is a repeatable runtime
  operation, so this is not covered by #1278's accepted one-time keying break.

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
- [~] Secrets redaction in transcripts/audit logs — audit-log path done for the `SubagentLlmError` provider-response-body leak class (#911 / #997 / PR #995 / PR #1006); raw provider bodies are now categorised to a status-class label (`Subagent LLM request rejected` etc.) before reaching `tool_decided` audit rows, and `FailedWithToolCalls`-wrapped sources redact recursively. Transcript / session-history path is broader (any tool output that echoes a secret, model output containing pasted credentials, prompt fragments returned by a provider error, etc.) and remains open
- [ ] Per-domain network allowlists

P2:
- [ ] MicroVM execution for high-risk tools
- [ ] Signed tool/plugin bundles
- [ ] Platform-specific sandboxing (Windows Job Objects, macOS Sandbox profiles)

---

*Authored by Mesut (2026-02-10).*
