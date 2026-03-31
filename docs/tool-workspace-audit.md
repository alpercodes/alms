# Tool Workspace/Sandbox Audit

**Date**: 2026-03-23
**Author**: Tim (automated review agent)
**Scope**: All run types -- sandbox_root, shell_exec cwd, workspace_write target, fs_* sandbox enforcement

---

## 1. Run Type Matrix

This table traces each run type through the code to document what sandbox_root, shell_exec cwd, workspace_write target, and fs_* sandbox root are set to.

### How sandbox_root flows through the code

1. **AlmsConfig.tools.sandbox_root** (default: ".") is loaded from alms.toml / env var ALMS_SANDBOX_ROOT.
2. **GatewayConfig.from_alms_config()** copies it into AgentConfig.sandbox_root.
3. **AgentRuntime::new()** canonicalizes it. Empty string = unrestricted (None). Non-empty = Some(canonicalized_path).
4. **Initial tool registration**: ToolRegistry::with_builtins_sandboxed(sandbox_root, ...) -- fs_read/fs_write/fs_list and shell_exec all get the canonicalized sandbox_root.
5. **with_workspace()** (if called): **re-registers** fs_read/fs_write/fs_list with ws_root (the canonicalized workspace directory) as their new sandbox_root. Also re-registers shell_exec with ws_root as default_cwd but **retains the original resolved_sandbox_root** for shell_exec sandbox check.

### Per-run-type breakdown

| Run Type | sandbox_root (initial) | fs_* sandbox after with_workspace() | shell_exec sandbox_root | shell_exec default cwd | workspace_write target | Agent name set? |
|---|---|---|---|---|---|---|
| **Web chat** (POST /runs) | AlmsConfig.tools.sandbox_root (default: cwd) | Agent workspace dir | Original sandbox_root (from config) | Agent workspace dir | Agent own workspace | Yes (from registry) |
| **Telegram** | Same as web chat | Same as web chat | Same | Same | Same | Yes |
| **DM-triggered** (peer message) | Same as web chat | Same as web chat | Same | Same | **Recipient agent** workspace | Yes (recipient) |
| **Subagent (named)** | Inherited from parent base_agent_config | Subagent workspace dir | Inherited sandbox_root | Subagent workspace dir | Subagent own workspace | Yes |
| **Subagent (ephemeral)** | Inherited from parent base_agent_config | **NOT re-registered** (no workspace attached) | Inherited sandbox_root | Sandbox root (fallback) | **NOT AVAILABLE** (no workspace) | No |
| **Scheduled job** | Same as web chat | Same as web chat | Same | Same | Same | Yes |
| **Completion notification** | Same as web chat | Same as web chat | Same | Same | Same | Yes |

---

## 2. Finding: fs_* sandbox root changes when workspace is attached

### Behavior

When with_workspace() is called, lines 254-271 of agent.rs re-register fs_read, fs_write, and fs_list with the **workspace directory** as their sandbox root, replacing the original project-level sandbox root.

This means:
- **Before with_workspace()**: fs_read/fs_write/fs_list are sandboxed to tools.sandbox_root (default: project cwd).
- **After with_workspace()**: fs_read/fs_write/fs_list are sandboxed to {workspace_dir}/{agent_name}/.

### Implication

An agent with a workspace attached **cannot read or write files outside its own workspace directory** via fs_read/fs_write, even if tools.sandbox_root was set to a broader directory. This is actually more restrictive than the initial config suggests.

However, shell_exec retains the **original** resolved_sandbox_root for its cwd validation (agent.rs line 277: self.resolved_sandbox_root.clone()), while getting the workspace dir as default_cwd. This means:
- The agent default cwd for shell commands is the workspace dir.
- But if the agent passes an explicit cwd parameter, it is validated against the **original** sandbox_root (project root), not the workspace dir.
- The executed command itself can still access any file on the system (the cwd restriction is a speed bump, not a true sandbox).

### Severity: LOW (by design, documented in CLAUDE.md known issues)

---

## 3. Finding: DM-triggered runs use the correct recipient workspace

### Code path

1. MessageBus.send() writes the message to the shared DM session and emits a RunTrigger with agent_id = recipient_agent_id.
2. run_trigger_loop() receives the trigger and calls enqueue_triggered_run() with is_peer_message: true.
3. execute_run() resolves the **recipient agent** config via resolve_agent_config(agent_id, ...).
4. The workspace is attached for the **recipient**: AgentWorkspace::new(workspace_dir, name) where name is the recipient name.
5. workspace_write is bound to the recipient workspace.
6. fs_read/fs_write/fs_list are re-sandboxed to the recipient workspace dir.

### Verdict: CORRECT

The recipient agent tools correctly operate within the recipient own workspace. Agent A sending a DM to Agent B cannot cause Agent B tools to write to Agent A workspace.

---

## 4. Finding: Subagent workspace isolation is correct

### Named subagents

In run_agent_loop() (coordinator, lines 901-906), the named subagent gets {workspace_dir}/{subagent_name}/ as its workspace. This is distinct from the parent agent workspace. The with_workspace() call re-sandboxes fs_* tools to this directory.

### Ephemeral subagents

Ephemeral subagents (subagent_name: None) do NOT get a workspace attached (attach_workspace = false). This means:
- No workspace_write tool is registered.
- fs_read/fs_write/fs_list retain the parent initial sandbox_root (typically project cwd).
- shell_exec retains the parent sandbox_root as both sandbox boundary and default cwd.

### Verdict: CORRECT (but see Bug #3 below)

---

## 5. Bug #1 (MEDIUM): Stray data/alms.db -- shell_exec cwd allows data dir creation

### Issue

Issue #300 reported that a subagent created a stray data/alms.db inside its workspace directory. This is explained by the shell_exec environment:

1. Named subagents get ALMS_DATA_DIR injected into their shell_exec env (coordinator line 894).
2. The ALMS_DATA_DIR is set to the gateway data directory (absolute path), which is correct.
3. However, if the subagent runs alms CLI commands that interpret ALMS_DATA_DIR relatively, or if the agent explicitly creates files at a relative data/ path via shell_exec, those files end up in the workspace dir (because shell_exec cwd = workspace dir).

The root cause is likely that some code path creates a data/ directory relative to cwd rather than using ALMS_DATA_DIR. This needs investigation outside this audit -- it is not a sandbox violation but a data leakage/confusion issue.

### Recommendation

Verify that all alms CLI subcommands respect ALMS_DATA_DIR as an absolute path and never fall back to a relative ./data/ when the env var is set.

---

## 6. Bug #2 (LOW): shell_exec sandbox_root vs default_cwd inconsistency

### Issue

When with_workspace() is called, shell_exec is configured with:
- sandbox_root = original config sandbox root (e.g., project cwd)
- default_cwd = workspace dir

If the workspace dir is outside the sandbox root, the with_default_cwd() method logs a warning (builtin.rs lines 547-554) but proceeds anyway.

This means agents run their shell commands in a directory that is outside the declared sandbox boundary, which is confusing but not a security violation (shell_exec commands can access any file regardless -- the sandbox only restricts cwd).

### When does this happen?

In practice, workspace_dir is typically {data_dir}/workspace (e.g., ./data/workspace/agent_name/) and sandbox_root defaults to "." (project root). Since data/workspace/agent_name/ is a subdirectory of the project root, starts_with should usually pass.

However, if ALMS_WORKSPACE_DIR is set to a path outside the project root (e.g., /tmp/workspaces/), this warning fires and agents run outside their sandbox boundary by default.

### Severity: LOW (operational warning, not a security bug since shell_exec sandbox is already documented as incomplete)

---

## 7. Bug #3 (HIGH): Ephemeral subagents inherit project-root sandbox, not workspace-scoped

### Issue

Ephemeral subagents (no subagent_name) do NOT get with_workspace() called. Their fs_read/fs_write/fs_list tools are sandboxed to the original base_agent_config.sandbox_root, which defaults to "." (project root).

This means an ephemeral subagent can read/write any file within the project root via fs_read/fs_write, including:
- Other agents workspace files (data/workspace/other_agent/*)
- The SQLite database (data/alms.db)
- Configuration files
- Source code (if the project root is a repo)

This is much broader than the named subagent sandbox (which is scoped to {workspace_dir}/{name}/).

### Impact

Since ephemeral subagents are spawned by the LLM via invoke_agent without a name parameter, any agent that can call invoke_agent can spawn an ephemeral subagent with project-root filesystem access. The agent could instruct the ephemeral subagent to read another agent memories or modify shared files.

### Recommendation

Consider either:
1. Creating a temp directory per ephemeral subagent and sandboxing fs_* to it, or
2. Not registering fs_read/fs_write/fs_list for ephemeral subagents at all (they are meant for short tasks), or
3. Sandboxing ephemeral subagents to a shared {workspace_dir}/_ephemeral/ directory.

---

## 8. Security Concern: secrets.json readable within default sandbox

### Behavior

shell_exec calls cmd.env_clear() (builtin.rs line 675) to strip the daemon environment, then re-injects:
1. Platform-critical vars (PATH, SystemRoot, etc.) -- via platform_critical_env_vars()
2. default_env (ALMS_DATA_DIR, ALMS_WORKSPACE_DIR)
3. Tool-call env parameter (agent-controlled)

### Analysis

- env_clear() correctly strips API keys (OPENAI_API_KEY, ANTHROPIC_API_KEY, TELEGRAM_BOT_TOKEN, etc.).
- The re-injected platform vars do not contain secrets.
- ALMS_DATA_DIR and ALMS_WORKSPACE_DIR are paths, not secrets.
- The agent can inject arbitrary env vars via the env parameter, but this is by design.

### Related: Issue #303

Issue #303 concerns API keys in env vars. The env_clear() approach is the mitigation -- it is working correctly. The remaining risk is that agents could discover keys through other means (reading data/secrets.json via fs_read if the data dir is within the sandbox).

### Severity: MEDIUM

If the sandbox_root includes the data/ directory (which it does by default since sandbox_root = "." and data_dir defaults to "./data"), an agent can fs_read("data/secrets.json") and extract API keys. This applies to ephemeral subagents (Bug #3 above) which retain the project-root sandbox. For agents with workspaces attached, the fs_* sandbox is narrowed to the workspace dir, so secrets.json is NOT accessible via fs_read -- but it IS accessible via shell_exec (cat data/secrets.json) since shell_exec only restricts cwd, not file access.

### Recommendation

Either:
1. Move data/ outside the sandbox_root, or
2. Add secrets.json to a denylist in fs_read, or
3. Ensure ALMS_MASTER_KEY encryption is enforced (the feature exists but may not be enabled by default).

---

## 9. Tool registration differences across run types

### Tools registered in ALL run types

All runs go through AgentRuntime::new() which registers:
- echo, math, http_get, shell_exec, fs_read, fs_write, fs_list (filtered by enabled_tools)

### Tools registered conditionally

| Tool | Web chat | Telegram | DM | Subagent | Scheduled job |
|---|---|---|---|---|---|
| workspace_write | Yes (if workspace_dir + agent_name) | Yes | Yes | Yes (named only) | Yes |
| invoke_agent | Yes | No | Yes | No | Yes |
| get_task_result | Yes | No | Yes | No | Yes |
| read_subagent_session | Yes | No | Yes | No | Yes |
| send_message | Yes (if agent_name) | No | Yes | No | Yes |
| list_agents | Yes (if agent_name) | No | Yes | No | Yes |
| read_messages | Yes (if agent_name) | No | Yes | No | Yes |
| ignore_message | Yes (if agent_name) | No | Yes | No | Yes |

### Key observation: Telegram runs miss multi-agent tools

Telegram runs are handled in gateway.rs::run_until_shutdown() which creates an AgentRuntime directly without registering invoke_agent, get_task_result, send_message, etc. This means agents operating via Telegram cannot spawn subagents or send peer messages.

This is likely intentional for the MVP (Telegram is a simpler channel), but it should be documented.

### Tool re-registration warnings

The "Tool X already registered, replacing" log message is benign. It occurs because:
1. AgentRuntime::new() registers all builtins (including shell_exec, fs_read, fs_write, fs_list).
2. with_shell_default_env() re-registers shell_exec with env vars.
3. with_workspace() re-registers fs_read, fs_write, fs_list, and shell_exec with workspace-scoped settings.

Each re-registration replaces the previous instance in the DashMap. The final state is correct.

---

## 10. Windows path handling in sandbox check

### Analysis of check_sandbox_path()

The function (builtin.rs lines 18-50):
1. Canonicalizes the sandbox_root (resolves to UNC path on Windows).
2. Resolves relative paths by joining to the canonical root.
3. Canonicalizes the result.
4. Uses starts_with() to verify containment.

### Windows-specific concerns

- canonicalize() on Windows produces UNC-prefixed paths. Both the root and the target get this prefix, so starts_with() should work consistently.
- The canonicalize_best_effort() function (lines 68-102) handles non-existent paths by walking components. On Windows, Component::Prefix is handled correctly (line 81).
- Issue #273 (referenced in agent.rs line 223) specifically dealt with UNC prefix mismatches. The fix ensures workspace dirs are created before canonicalization.

### Verdict: ADEQUATE (the #273 fix addressed the main Windows issue)

---

## 11. Summary of findings

| # | Severity | Finding | Recommendation |
|---|---|---|---|
| 1 | MEDIUM | Stray data/alms.db in subagent workspace (issue #300) | Verify CLI respects ALMS_DATA_DIR absolutely |
| 2 | LOW | shell_exec default_cwd can be outside sandbox_root | Document behavior; no code change needed |
| 3 | HIGH | Ephemeral subagents have project-root fs access | Scope ephemeral subagent fs_* to temp or shared dir |
| 4 | MEDIUM | secrets.json readable via fs_read within default sandbox | Move data/ out of sandbox or add denylist |
| 5 | INFO | Telegram runs lack multi-agent tools | Document intentional limitation |
| 6 | INFO | Tool re-registration warnings are benign | No action needed |

### Positive findings

- DM-triggered runs correctly use the recipient workspace (not the sender).
- Named subagent workspace isolation is correct.
- workspace_write always targets the owning agent workspace.
- env_clear() in shell_exec correctly strips API keys.
- Windows path handling is adequate after the #273 fix.
- config-to-runtime threading for sandbox_root, shell_policy, and enabled_tools is consistent across all run types.

---

## 12. Documentation staleness check

### CLAUDE.md

The known issues section states that fs_* tools are sandboxed via canonicalize() + prefix check against tools.sandbox_root (default: cwd).

This is **partially stale**: it does not mention that with_workspace() narrows the fs_* sandbox to the workspace dir (which is more restrictive than the stated tools.sandbox_root). The documentation gives the impression that fs_* tools are always sandboxed to tools.sandbox_root, when in practice they are narrowed to the workspace dir for agents with workspaces.

### Recommended update to CLAUDE.md known issues

Add: "When a workspace is attached, fs_read/fs_write/fs_list are re-sandboxed to the agent workspace directory, which is typically more restrictive than tools.sandbox_root. Ephemeral subagents (without a workspace) retain the broader tools.sandbox_root scope."

### docs/security-model.md

Should be updated to document the sandbox narrowing behavior and the ephemeral subagent exception.
