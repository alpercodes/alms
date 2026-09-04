// SPDX-License-Identifier: Apache-2.0

//! Redesigned shell tool inspired by Claude Code's Bash tool.
//!
//! Key improvements over the original `ShellExecTool` (since removed):
//! - **Command strings**: Interface is `"command": "ls -la"` via `bash -c`
//! - **Persistent cwd**: Working directory persists across calls
//! - **Output truncation**: 30KB max with head+tail line preservation
//! - **Background execution**: `run_in_background: true` spawns a task
//! - **Timeouts**: 120s default, 600s max, configurable via `timeout_ms`
//! - **Security**: env_clear, secret env vars, command denylist,
//!   Landlock filesystem sandboxing on Linux 5.13+
//!
//! The legacy `argv` mode has been removed. All commands go through `bash -c`
//! which ensures the denylist and security checks apply uniformly.

pub mod background;
pub mod classification;
pub mod exec;
pub mod output;
pub(crate) mod pathnorm;
pub mod permissions;
pub mod security;
pub mod spill;
pub mod types;

pub use exec::init_shell_resolution;

use crate::{SandboxError, Tool, error::SandboxResult};
use alms_core::config::{ShellClassificationMode as CoreClassificationMode, ShellPermissions};
use classification::ClassificationMode;
use exec::command_excerpt;
use permissions::CompiledPermissions;
use serde_json::Value;
use spill::ShellSpillPolicy;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, warn};
use types::{
    CwdRejection, CwdRevert, DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, ShellInput, ShellOutput,
    ShellState,
};
use uuid::Uuid;

/// Build the per-`ShellTool` PWD marker.
///
/// The marker is generated once at tool construction and embedded in every
/// command this `ShellTool` runs. Earlier versions used a fixed `__ALMS_PWD_MARKER__`
/// constant — if a user script ever printed that exact string, it would
/// corrupt cwd tracking. The per-instance UUID nonce avoids that class of
/// collision: even if a script intentionally tries to forge the marker, it
/// cannot guess the random suffix. Cloning a `ShellTool` (e.g. when the
/// runtime registers the same tool under both `shell` and `shell_exec`)
/// preserves the marker so cwd recovery semantics remain consistent.
fn new_pwd_marker() -> String {
    format!("__ALMS_PWD_{}__", Uuid::new_v4().simple())
}

/// Append a bracketed daemon marker line to captured stdout.
///
/// Markers are the shell tool's convention for telling the agent something
/// the command itself could not (`[full output spilled to: ...]`,
/// `[cwd unchanged: ...]`). They go on their own line, so a marker is never
/// glued to the tail of a partial line of real output.
fn append_marker_line(stdout: &str, marker: &str) -> String {
    if stdout.is_empty() || stdout.ends_with('\n') {
        format!("{stdout}{marker}")
    } else {
        format!("{stdout}\n{marker}")
    }
}

/// The tool name used for registration. Agents call it as `"shell"`.
pub const SHELL_TOOL_NAME: &str = "shell";

/// The wire name `"shell_exec"`, kept as a registration alias for `"shell"`.
///
/// **Live, operator-facing, and not dead code however unreferenced it looks.**
/// It is reachable from the `tools.enabled` allowlist and from any agent still
/// emitting the older name, so nothing about it is decided by counting Rust
/// call sites. `registry.rs` says so in code rather than prose: the alias is
/// registered via `register_as` alongside the canonical name, and the
/// "unknown tool in `tools.enabled`" warning carries an explicit
/// `name != SHELL_TOOL_ALIAS` arm — the registry already treats `shell_exec`
/// as a legitimate operator-supplied value rather than a typo.
///
/// Three lookalike names collect around this module and only two survive:
///
/// - `"shell"` — the canonical tool name [`ShellTool`] registers under.
/// - `"shell_exec"` (this const) — the backward-compatible alias for it.
/// - `ShellExecTool` — the pre-redesign *implementation*, and afterwards a
///   `pub type ShellExecTool = ShellTool` alias kept for source compatibility.
///   Both are gone: the implementation was replaced by [`ShellTool`], and the
///   alias was removed once nothing in the workspace referenced it. Prose
///   elsewhere in this module still names it, deliberately, as history.
///
/// The two are easy to conflate and the consequences are asymmetric: deleting
/// the type alias cost nothing, deleting this const would silently break every
/// config and agent still saying `shell_exec`.
pub const SHELL_TOOL_ALIAS: &str = "shell_exec";

/// Redesigned shell tool with persistent cwd, command strings, and
/// background execution support.
///
/// Replaced the original `ShellExecTool` (since removed). Registered under
/// the name `"shell"` with `"shell_exec"` ([`SHELL_TOOL_ALIAS`]) as a
/// backward-compatible alias.
#[derive(Debug)]
pub struct ShellTool {
    /// When Some, cwd is validated against this root in sandboxed mode.
    sandbox_root: Option<PathBuf>,
    /// When true, no cwd restriction is applied.
    unrestricted: bool,
    /// Default environment variables injected into spawned processes.
    default_env: HashMap<String, String>,
    /// Persistent shell state (cwd tracking, background tasks).
    state: ShellState,
    /// Compiled permission rules (allow/deny patterns for commands).
    permissions: CompiledPermissions,
    /// Built-in classifier policy (enforced after the permission check).
    classification_mode: ClassificationMode,
    /// Large-output spill-to-disk policy (issue #756). When active, bytes
    /// exceeding the truncation threshold are written to a per-run file
    /// under `{data_dir}/shell_output/{run_id}/` and the agent-visible tool
    /// result gains a `[full output spilled to: ...]` marker pointing at it.
    /// Stored in an `Arc` because cloning a [`ShellTool`] is common (e.g. the
    /// `shell_exec` alias registration) and we want every clone to observe
    /// the same config without copying the inner fields on each clone.
    spill_policy: Arc<ShellSpillPolicy>,
    /// Per-instance random PWD marker (generated by [`new_pwd_marker`]).
    /// Used to delimit the appended `pwd` output so we can recover the cwd
    /// across calls without colliding with arbitrary user-script output.
    pwd_marker: String,
}

/// Convert the serde-owned enum in `alms-core` into the internal sandbox
/// enum. Keeping two types avoids pulling `serde` into the hot path and lets
/// the sandbox crate extend its modes without breaking the config wire format.
fn map_classification_mode(m: CoreClassificationMode) -> ClassificationMode {
    match m {
        CoreClassificationMode::Off => ClassificationMode::Off,
        CoreClassificationMode::Warn => ClassificationMode::Warn,
        CoreClassificationMode::BlockDestructive => ClassificationMode::BlockDestructive,
        CoreClassificationMode::Strict => ClassificationMode::Strict,
    }
}

impl Clone for ShellTool {
    fn clone(&self) -> Self {
        // ShellState uses Arc internally, so cloning shares the state.
        // The pwd_marker is preserved so cloned instances (e.g. the
        // `shell_exec` alias registered alongside `shell`) recover the cwd
        // through the same marker the parent emitted.
        Self {
            sandbox_root: self.sandbox_root.clone(),
            unrestricted: self.unrestricted,
            default_env: self.default_env.clone(),
            state: self.state.clone(),
            permissions: self.permissions.clone(),
            classification_mode: self.classification_mode,
            spill_policy: Arc::clone(&self.spill_policy),
            pwd_marker: self.pwd_marker.clone(),
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            sandbox_root: None,
            unrestricted: true,
            default_env: HashMap::new(),
            state: ShellState::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            permissions: CompiledPermissions::compile(&ShellPermissions::default()),
            classification_mode: ClassificationMode::default(),
            spill_policy: Arc::new(ShellSpillPolicy::disabled()),
            pwd_marker: new_pwd_marker(),
        }
    }
}

impl ShellTool {
    /// Create an unrestricted shell tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed shell tool. The cwd is restricted to `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        let state = ShellState::new(root.clone());
        Self {
            sandbox_root: Some(root),
            unrestricted: false,
            default_env: HashMap::new(),
            state,
            permissions: CompiledPermissions::compile(&ShellPermissions::default()),
            classification_mode: ClassificationMode::default(),
            spill_policy: Arc::new(ShellSpillPolicy::disabled()),
            pwd_marker: new_pwd_marker(),
        }
    }

    /// Create with explicit policy (matching the removed `ShellExecTool::with_policy`).
    pub fn with_policy(sandbox_root: Option<PathBuf>, unrestricted: bool) -> Self {
        let initial_cwd = sandbox_root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let state = ShellState::new(initial_cwd);
        Self {
            sandbox_root,
            unrestricted,
            default_env: HashMap::new(),
            state,
            permissions: CompiledPermissions::compile(&ShellPermissions::default()),
            classification_mode: ClassificationMode::default(),
            spill_policy: Arc::new(ShellSpillPolicy::disabled()),
            pwd_marker: new_pwd_marker(),
        }
    }

    /// Returns this tool's per-instance PWD marker. Visible to tests so they
    /// can verify cwd-recovery behaviour without depending on a hardcoded
    /// constant.
    #[doc(hidden)]
    pub fn pwd_marker(&self) -> &str {
        &self.pwd_marker
    }

    /// Set shell command permissions (allow/deny patterns).
    ///
    /// The [`ShellPermissions`] are compiled into regex patterns once and
    /// reused for every command. Invalid patterns are logged and skipped.
    pub fn with_permissions(mut self, permissions: &ShellPermissions) -> Self {
        self.permissions = CompiledPermissions::compile(permissions);
        self
    }

    /// Set the built-in risk classification mode (block/warn/off).
    ///
    /// Layers **on top of** [`Self::with_permissions`]: the permission list
    /// is the user-configurable policy (regex allow/deny), while the
    /// classifier is built-in defense-in-depth for known-destructive patterns
    /// (`rm -rf /`, `sudo`, `mkfs`, `curl|sh`, etc.).
    pub fn with_classification_mode(mut self, mode: CoreClassificationMode) -> Self {
        self.classification_mode = map_classification_mode(mode);
        self
    }

    /// Set the default working directory for commands.
    ///
    /// This replaces the persistent cwd. Useful when attaching an agent workspace.
    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        // #1255: normalise both sides before comparing. The raw
        // `starts_with` this replaced warned whenever the configured
        // default cwd and the sandbox root were spelled in different but
        // equivalent forms (relative vs absolute, `\\?\`-prefixed, or
        // differently cased on Windows).
        if let Some(ref root) = self.sandbox_root
            && !pathnorm::is_within(
                &pathnorm::canonical_for_comparison(root),
                &pathnorm::canonical_for_comparison(&cwd),
            )
        {
            warn!(
                default_cwd = %cwd.display(),
                sandbox_root = %root.display(),
                "default_cwd is outside sandbox_root"
            );
        }
        // Replace the state with a fresh instance rooted at the new cwd.
        // Note: this creates a new ShellState, so any in-flight background
        // tasks from the previous state will no longer be queryable.
        self.state = ShellState::new(cwd);
        self
    }

    /// Set default environment variables for spawned processes.
    ///
    /// These are injected after `env_clear()` so they don't leak the daemon's
    /// secrets. The tool call's `env` parameter overrides defaults on conflict.
    pub fn with_default_env(mut self, env: HashMap<String, String>) -> Self {
        self.default_env = env;
        self
    }

    /// Set the large-output spill policy (issue #756).
    ///
    /// When active, bytes exceeding the head+tail truncation threshold are
    /// written to a per-run file and a `[full output spilled to: ...]`
    /// marker is appended to the tool result so the agent can `fs_read` the
    /// full capture with offset/limit.
    ///
    /// Wired by `alms-runtime` at run start (once the run_id is known); unit
    /// tests and construction paths that don't have a run_id simply leave
    /// this at the default `disabled` policy.
    pub fn with_spill_policy(mut self, policy: ShellSpillPolicy) -> Self {
        self.spill_policy = Arc::new(policy);
        self
    }

    /// Parse JSON parameters into a `ShellInput`.
    fn parse_input(&self, params: &Value) -> SandboxResult<ShellInput> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);

        let Some(command) = command else {
            return Err(SandboxError::InvalidParameters(
                "'command' parameter is required".to_string(),
            ));
        };

        if command.trim().is_empty() {
            return Err(SandboxError::InvalidParameters(
                "'command' must not be empty".to_string(),
            ));
        }

        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Timeout: prefer timeout_ms, fall back to timeout_secs (deprecated)
        let timeout_ms = if let Some(ms) = params.get("timeout_ms").and_then(|v| v.as_u64()) {
            ms.min(MAX_TIMEOUT_MS)
        } else if let Some(secs) = params.get("timeout_secs").and_then(|v| v.as_u64()) {
            (secs * 1000).min(MAX_TIMEOUT_MS)
        } else {
            DEFAULT_TIMEOUT_MS
        };

        let run_in_background = params
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(ShellInput {
            command,
            description,
            timeout_ms,
            run_in_background,
        })
    }

    /// Append the agent-visible `[full output spilled to: ...]` marker to
    /// `stdout` when a spill file was written for this command. Returns the
    /// stdout unchanged when no spill occurred.
    ///
    /// Shared between the foreground execution path and `handle_check_task`
    /// so background-task results surface the same recovery convention as
    /// foreground ones (issue #811). The path is rendered relative to the
    /// workspace root (the shell tool's `sandbox_root`) when possible so the
    /// agent can paste it straight into `fs_read`.
    fn append_spill_marker(&self, stdout: &str, spill_path: Option<&PathBuf>) -> String {
        match spill_path {
            Some(path) => {
                let workspace_root = self.sandbox_root.as_deref();
                let rel = spill::relative_spill_path(path, workspace_root);
                append_marker_line(stdout, &format!("[full output spilled to: {rel}]"))
            }
            None => stdout.to_string(),
        }
    }

    /// Append the agent-visible `[cwd unchanged: ...]` notice to `stdout`
    /// when a command's final working directory failed containment and the
    /// persistent cwd was therefore kept where it was (issue #1262). Returns
    /// the stdout unchanged when no revert happened — which is every command
    /// in an unsandboxed run, where containment does not apply at all.
    ///
    /// The notice rides in the stdout body, the same channel as
    /// `append_spill_marker`, because that is the one field every consumer
    /// already reads: the agent (the whole result JSON is stringified into
    /// the tool message, but stdout is where it looks for what happened) and
    /// the web UI, whose shell renderer only surfaces `exit_code` / `stdout`
    /// / `stderr` and drops unknown fields into the raw-JSON toggle. A new
    /// top-level field would reach the model but would be invisible to the
    /// human watching the run without a frontend change.
    ///
    /// Not stderr: a reverted `cd` is not the command's own diagnostic
    /// output, and agents routinely treat a non-empty stderr as a failure
    /// signal even when `exit_code` is 0.
    fn append_cwd_revert_notice(&self, stdout: &str, revert: Option<&CwdRevert>) -> String {
        match revert {
            Some(revert) => {
                // State only what the containment check actually determined.
                // A rejection is not by itself evidence the path was outside
                // the root: `pathnorm::normalise` falls back to its raw input
                // when canonicalisation fails, so an unresolvable cwd is
                // rejected on the very same branch as a real escape — and
                // under Windows Git Bash's `/tmp` mount (#1266) that branch
                // is *every* command. A confident but wrong explanation
                // delivered every turn would be worse for the agent loop than
                // none at all, which is the failure this notice exists to fix
                // one level up.
                let verdict = match revert.reason {
                    CwdRejection::OutsideRoot => "is outside the sandbox root",
                    CwdRejection::NotVerifiable => "could not be confirmed inside the sandbox root",
                };
                append_marker_line(
                    stdout,
                    &format!(
                        "[cwd unchanged: '{}' {verdict}; \
                         subsequent commands still run in '{}']",
                        // `attempted` stays in the form the shell reported
                        // it, so it matches what the agent's own `pwd`
                        // printed. `kept` is a daemon-held path that may
                        // still carry the `\\?\` prefix `canonicalize` adds
                        // on Windows — strip it so the agent sees a spelling
                        // it can actually use.
                        revert.attempted.display(),
                        pathnorm::strip_verbatim_prefix(&revert.kept).display()
                    ),
                )
            }
            None => stdout.to_string(),
        }
    }

    /// Whether cwd containment applies to this instance. Mirrors the
    /// `!unrestricted && let Some(root) = sandbox_root` guard in
    /// `exec::execute_command` — both a root to contain against and a
    /// non-`unrestricted` policy are required, and the description the agent
    /// reads must agree with the rule the daemon enforces.
    fn cwd_is_contained(&self) -> bool {
        !self.unrestricted && self.sandbox_root.is_some()
    }

    /// Apply every agent-visible stdout decoration to a completed execution.
    ///
    /// Single entry point so the foreground path and `handle_check_task`
    /// cannot drift apart on which markers a result carries — the spill
    /// marker had to be retrofitted onto the background path once already
    /// (issue #811).
    fn decorate_stdout(&self, output: &ShellOutput) -> String {
        let with_spill = self.append_spill_marker(&output.stdout, output.spill_path.as_ref());
        self.append_cwd_revert_notice(&with_spill, output.cwd_revert.as_ref())
    }

    /// Handle a request to check background task status.
    async fn handle_check_task(&self, task_id: &str) -> SandboxResult<Value> {
        match background::check_background_task(&self.state, task_id).await {
            Some(result) => {
                if let Some(ref output) = result.output {
                    // Mirror the foreground marker contract: background-task
                    // large outputs are spilled to disk too and the agent
                    // needs the path to recover them via fs_read (issue
                    // #811), and a background command's `cd` is contained by
                    // exactly the same rule as a foreground one — it runs
                    // against the shared `ShellState` (issue #1262).
                    let stdout_with_markers = self.decorate_stdout(output);
                    Ok(serde_json::json!({
                        "task_id": result.task_id,
                        "status": "completed",
                        "command": result.command,
                        "exit_code": output.exit_code,
                        "stdout": stdout_with_markers,
                        "stderr": output.stderr,
                    }))
                } else if let Some(ref error) = result.error {
                    Ok(serde_json::json!({
                        "task_id": result.task_id,
                        "status": "failed",
                        "command": result.command,
                        "error": error,
                    }))
                } else {
                    Ok(serde_json::json!({
                        "task_id": result.task_id,
                        "status": "unknown",
                    }))
                }
            }
            None => Ok(serde_json::json!({
                "task_id": task_id,
                "status": "not_found_or_still_running",
            })),
        }
    }
}

/// The mode-independent body of the shell tool description.
///
/// A `macro_rules!` rather than a `const` so `concat!` can splice it into the
/// sandboxed variant below at compile time. `Tool::description` returns a
/// borrow, so both variants have to be `&'static str` — this keeps them so
/// without the shared prose existing in two places that can drift apart.
macro_rules! shell_description_base {
    () => {
        "Execute a shell command (via bash -c) and return its output. \
         The working directory persists between calls. \
         Supports background execution and configurable timeouts. \
         Commands run with the daemon's filesystem access; on Linux 5.13+, \
         Landlock restricts filesystem access to the sandbox root when enabled."
    };
}

/// Description for an instance with no sandbox root, or one running
/// unrestricted (`[security].allow_full_os_access`). Containment is skipped
/// outright in that mode (`exec.rs`), so the confinement sentence below would
/// be a lie here — the point of splitting the description in two.
const SHELL_DESCRIPTION_UNRESTRICTED: &str = shell_description_base!();

/// Description for a sandboxed instance — the shape every agent workspace
/// gets. The cwd confinement is the one restriction the agent is guaranteed
/// to hit, and unlike Landlock it applies on every platform, so it is stated
/// unconditionally here rather than hedged (#1262).
const SHELL_DESCRIPTION_SANDBOXED: &str = concat!(
    shell_description_base!(),
    " The working directory is confined to the sandbox root: a command that \
      ends up outside it (e.g. `cd /etc`) leaves the persistent working \
      directory where it was, and the result says so with a \
      '[cwd unchanged: ...]' line."
);

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        SHELL_TOOL_NAME
    }

    fn description(&self) -> &str {
        // Branch on the live fields rather than caching a string at
        // construction time. `sandbox_root` / `unrestricted` are set once by
        // the constructors and no builder mutates them today, but a cached
        // copy would be one new builder away from silently lying to the
        // agent about which mode it is in.
        if self.cwd_is_contained() {
            SHELL_DESCRIPTION_SANDBOXED
        } else {
            SHELL_DESCRIPTION_UNRESTRICTED
        }
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (via bash -c)."
                },
                "description": {
                    "type": "string",
                    "description": "Brief description of what this command does (for audit logging)."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default 120000, max 600000)."
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Deprecated: use timeout_ms instead. Timeout in seconds (default 120, max 600)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the command in the background. Returns a task_id immediately.",
                    "default": false
                },
                "env": {
                    "type": "object",
                    "description": "Extra environment variables as key-value pairs.",
                    "additionalProperties": { "type": "string" }
                },
                "check_task": {
                    "type": "string",
                    "description": "Check the result of a background task by its task_id. Mutually exclusive with 'command' — when present, all other parameters are ignored."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // Special case: checking background task status
        if let Some(task_id) = params.get("check_task").and_then(|v| v.as_str()) {
            return self.handle_check_task(task_id).await;
        }

        let input = self.parse_input(&params)?;

        // --- Defence chain (issue #745) ------------------------------------
        //
        // Order:
        //   1. Permissions admission gate (allow/deny regex, operator policy).
        //   2. Classifier-override check (operator opt-out, narrow regex).
        //   3. Risk classifier (non-bypassable floor for destructive findings).
        //   4. Hardcoded destructive denylist + Landlock (in `exec.rs`).
        //
        // The classifier is a *floor*, not a gate that operators can disable
        // by loosening their allowlist. With the pre-#745 chain, a permissive
        // `allowed_commands = [".*"]` combined with `ClassificationMode::Warn`
        // (or a `ClassificationMode::Off` opt-out) silently degraded the
        // defence to nothing. Today the classifier's destructive verdicts
        // always apply unless the operator has **explicitly** enumerated a
        // command in `[tools.shell_permissions].classifier_overrides`.

        // 1. Permissions admission gate.
        if let Err(e) = self.permissions.check_command(&input.command) {
            warn!(
                tool = "shell",
                reason = "permission_denied",
                command_excerpt = %command_excerpt(&input.command),
                error = %e,
                "Shell command blocked by permission policy"
            );
            return Err(e);
        }

        // 2. Operator-only classifier override. A match short-circuits the
        // classifier entirely (step 3). Overrides cannot weaken the deny list
        // (step 1) or the OS-level sandbox (step 4). Every override hit is
        // logged so operators can audit which exemption patterns are being
        // exercised in production.
        let override_pattern = self
            .permissions
            .matching_classifier_override(&input.command);
        if let Some(pattern) = override_pattern {
            warn!(
                tool = "shell",
                reason = "classifier_override_hit",
                override_pattern = %pattern,
                command_excerpt = %command_excerpt(&input.command),
                "Shell classifier bypassed by operator override"
            );
        } else {
            // 3. Risk classifier — non-bypassable floor for destructive findings.
            match classification::enforce(&input.command, self.classification_mode) {
                Ok(classification) => {
                    // Issue #745 observability: if the allowlist accepted a
                    // command the classifier flags at moderate-or-worse,
                    // surface the divergence. Operators who see this warn
                    // repeatedly may want to tighten their allowlist or add
                    // an explicit `classifier_overrides` entry (making intent
                    // auditable) rather than leaving classification-borderline
                    // commands to a permissive `.*` pattern.
                    if !self.permissions.is_empty() && classification.is_moderate_or_worse() {
                        warn!(
                            tool = "shell",
                            reason = "allowlist_classifier_divergence",
                            classifier_level = %classification.level,
                            classifier_finding_count = classification.findings.len(),
                            command_excerpt = %command_excerpt(&input.command),
                            "Shell command admitted by permissions policy but flagged by built-in risk classifier"
                        );
                    }
                }
                Err(e) => {
                    // Surface the structured classifier target (#758) as a
                    // dedicated tracing field so log pipelines can index on it
                    // rather than regexing the message body.
                    let classifier_target = match &e {
                        SandboxError::ShellBlocked { target, .. } => target.as_deref(),
                        _ => None,
                    };
                    error!(
                        tool = "shell",
                        reason = "classifier_denied",
                        command_excerpt = %command_excerpt(&input.command),
                        classifier_target = ?classifier_target,
                        error = %e,
                        "Shell command blocked by built-in risk classifier"
                    );
                    return Err(e);
                }
            }
        }

        // Merge tool-call env params into default_env (tool-call overrides defaults)
        let mut merged_env = self.default_env.clone();
        if let Some(env_obj) = params.get("env").and_then(|v| v.as_object()) {
            for (k, v) in env_obj {
                if security::is_secret_env_var(k) {
                    warn!(env_var = %k, "Blocked secret env var from tool-call env injection");
                    continue;
                }
                if let Some(val) = v.as_str() {
                    merged_env.insert(k.clone(), val.to_owned());
                }
            }
        }

        // Background execution
        if input.run_in_background {
            let task_id = background::submit_background_task(
                input,
                &self.state,
                self.sandbox_root.clone(),
                self.unrestricted,
                merged_env,
                self.pwd_marker.clone(),
                (*self.spill_policy).clone(),
            )
            .await?;

            return Ok(serde_json::json!({
                "task_id": task_id,
                "status": "submitted",
                "message": "Command submitted for background execution. Use check_task to retrieve results."
            }));
        }

        // Per-invocation ID used both as the spill filename fragment and for
        // structured logs. The Tool trait doesn't surface the LLM-provided
        // tool_call_id to the tool impl, so we generate a fresh UUID here —
        // what matters for the spec is a unique, grep-able filename, which a
        // UUID satisfies.
        let tool_call_id = Uuid::new_v4().simple().to_string();

        // Foreground execution
        let output = exec::execute_command(
            &input,
            &self.state,
            self.sandbox_root.as_deref(),
            self.unrestricted,
            &merged_env,
            &self.pwd_marker,
            &self.spill_policy,
            &tool_call_id,
        )
        .await?;

        // Append the agent-visible marker lines to stdout so the LLM sees
        // them in the normal tool result body: the spill path it can
        // `fs_read` (issue #811) and any cwd revert it would otherwise never
        // learn about (issue #1262). Background-task results go through the
        // same helper from `handle_check_task`.
        let stdout_with_markers = self.decorate_stdout(&output);

        Ok(serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": stdout_with_markers,
            "stderr": output.stderr,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_tool_name() {
        let tool = ShellTool::new();
        assert_eq!(tool.name(), "shell");
    }

    // ── PWD marker nonce (issue #743 item 6) ─────────────────────────────

    #[test]
    fn test_pwd_marker_is_unique_per_instance() {
        // Each freshly constructed ShellTool must have a distinct marker.
        // A stable, hardcoded constant would let a user script that prints
        // the marker corrupt cwd recovery on a different agent's instance.
        let a = ShellTool::new();
        let b = ShellTool::new();
        assert_ne!(
            a.pwd_marker(),
            b.pwd_marker(),
            "PWD markers should differ between fresh ShellTool instances"
        );
    }

    #[test]
    fn test_pwd_marker_format_has_random_suffix() {
        // The marker format is `__ALMS_PWD_<uuid_simple>__` so it remains
        // recognisable to any future tooling but uses a per-instance random
        // suffix so it cannot be guessed or accidentally collided with.
        let tool = ShellTool::new();
        let marker = tool.pwd_marker();
        assert!(marker.starts_with("__ALMS_PWD_"));
        assert!(marker.ends_with("__"));
        // The UUID simple form is 32 hex chars; with the prefix/suffix the
        // marker length is exactly 32 + 11 + 2 = 45.
        assert_eq!(marker.len(), 45);
    }

    #[test]
    fn test_pwd_marker_preserved_across_clone() {
        // Cloning a ShellTool (e.g. when registering it under both `shell`
        // and `shell_exec`) must preserve the marker so the clone recovers
        // the cwd through the same emitted token.
        let a = ShellTool::new();
        let b = a.clone();
        assert_eq!(a.pwd_marker(), b.pwd_marker());
    }

    #[test]
    fn test_pwd_marker_propagates_through_constructors() {
        // sandboxed() and with_policy() must also generate fresh markers.
        let dir = tempfile::tempdir().unwrap();
        let a = ShellTool::sandboxed(dir.path().to_path_buf());
        let b = ShellTool::with_policy(Some(dir.path().to_path_buf()), false);
        assert_ne!(a.pwd_marker(), b.pwd_marker());
        assert!(a.pwd_marker().starts_with("__ALMS_PWD_"));
        assert!(b.pwd_marker().starts_with("__ALMS_PWD_"));
    }

    #[test]
    fn test_shell_tool_description_not_empty() {
        let tool = ShellTool::new();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_shell_tool_is_builtin() {
        let tool = ShellTool::new();
        assert!(tool.is_builtin());
    }

    #[test]
    fn test_shell_tool_parameters_schema() {
        let tool = ShellTool::new();
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("command"));
        assert!(!props.contains_key("argv"), "argv mode has been removed");
        assert!(props.contains_key("description"));
        assert!(props.contains_key("timeout_ms"));
        assert!(props.contains_key("timeout_secs"));
        assert!(props.contains_key("run_in_background"));
        assert!(props.contains_key("env"));
        assert!(props.contains_key("check_task"));
        // `required` is empty at schema level: `check_task` is an alternative to
        // `command`. Runtime validation in `parse_input()` enforces `command` when
        // `check_task` is not provided.
        let required = params["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "required should be empty — check_task is mutually exclusive with command"
        );
    }

    #[test]
    fn test_parse_input_command() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "ls -la"});
        let input = tool.parse_input(&params).unwrap();
        assert_eq!(input.command, "ls -la");
        assert_eq!(input.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert!(!input.run_in_background);
    }

    #[test]
    fn test_parse_input_missing_command() {
        let tool = ShellTool::new();
        let params = serde_json::json!({});
        assert!(tool.parse_input(&params).is_err());
    }

    #[test]
    fn test_parse_input_empty_command() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "   "});
        assert!(tool.parse_input(&params).is_err());
    }

    #[test]
    fn test_parse_input_timeout_ms() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "ls", "timeout_ms": 5000});
        let input = tool.parse_input(&params).unwrap();
        assert_eq!(input.timeout_ms, 5000);
    }

    #[test]
    fn test_parse_input_timeout_secs_fallback() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "ls", "timeout_secs": 60});
        let input = tool.parse_input(&params).unwrap();
        assert_eq!(input.timeout_ms, 60_000);
    }

    #[test]
    fn test_parse_input_timeout_clamped() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "ls", "timeout_ms": 9_999_999});
        let input = tool.parse_input(&params).unwrap();
        assert_eq!(input.timeout_ms, MAX_TIMEOUT_MS);
    }

    #[test]
    fn test_parse_input_background() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "sleep 10", "run_in_background": true});
        let input = tool.parse_input(&params).unwrap();
        assert!(input.run_in_background);
    }

    #[test]
    fn test_parse_input_description() {
        let tool = ShellTool::new();
        let params = serde_json::json!({"command": "ls", "description": "List files"});
        let input = tool.parse_input(&params).unwrap();
        assert_eq!(input.description.as_deref(), Some("List files"));
    }

    // ── Integration tests (Unix only) ─────────────────────────────────────

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_command_echo() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_persistent_cwd() {
        let tool = ShellTool::new();

        // Change directory
        let result = tool
            .execute(serde_json::json!({"command": "cd /tmp && pwd"}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("/tmp"));

        // Next command should start in /tmp
        let result = tool
            .execute(serde_json::json!({"command": "pwd"}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("/tmp"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_env_cleared() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "env"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            !stdout.contains("OPENROUTER_API_KEY"),
            "API keys must not leak into spawned processes"
        );
        assert!(
            stdout.contains("PATH="),
            "PATH should be re-injected after env_clear()"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_default_env() {
        let mut env = HashMap::new();
        env.insert("ALMS_DATA_DIR".to_string(), "/tmp/alms-test".to_string());
        let tool = ShellTool::new().with_default_env(env);

        let result = tool
            .execute(serde_json::json!({"command": "env"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("ALMS_DATA_DIR=/tmp/alms-test"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_env_override() {
        let mut env = HashMap::new();
        env.insert("ALMS_DATA_DIR".to_string(), "/default".to_string());
        let tool = ShellTool::new().with_default_env(env);

        let result = tool
            .execute(serde_json::json!({
                "command": "env",
                "env": {"ALMS_DATA_DIR": "/override"}
            }))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("ALMS_DATA_DIR=/override"));
        assert!(!stdout.contains("ALMS_DATA_DIR=/default"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_blocks_secret_env() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({
                "command": "env",
                "env": {"OPENAI_API_KEY": "stolen", "SAFE_VAR": "ok"}
            }))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(!stdout.contains("OPENAI_API_KEY"));
        assert!(stdout.contains("SAFE_VAR=ok"));
    }

    /// Regression guard for #744: the hardcoded denied-filename substring
    /// check was removed because a single-entry list (`secrets.json`) was
    /// not user-configurable, did not scale, and was trivially bypassable.
    /// Users who need file-level protection should use
    /// `[tools.shell_permissions]` (configurable regex allow/deny) or
    /// Landlock (kernel-level path enforcement on Linux).
    ///
    /// This test documents that the removed policy no longer fires: a
    /// shell command mentioning `secrets.json` is no longer blocked at
    /// the shell-tool layer for that reason alone. (The command itself
    /// may still fail for unrelated reasons — missing file, permissions,
    /// etc. — so we only assert that it is *not* blocked with the legacy
    /// "denied file" message.)
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_no_hardcoded_denied_filename() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo secrets.json"}))
            .await;
        // Must succeed now: the hardcoded denied-filename check is gone.
        // `echo` does not touch the filesystem and has no reason to fail.
        let ok = result.expect("echo of a string should not be blocked");
        assert_eq!(ok["exit_code"], 0);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_denied_pattern() {
        // With the classifier in place, `rm -rf /` is now blocked by the
        // built-in risk classifier before the legacy denylist ever fires.
        // The user-facing error reveals only the level, not the heuristic.
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "rm -rf /"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("blocked") || err.contains("denied"),
            "expected blocked-or-denied message, got: {err}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_sandboxed_cwd_stays_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = ShellTool::sandboxed(root.clone());

        // The command changes to /etc in a subshell, but the persistent cwd
        // should NOT be updated to /etc because it is outside the sandbox root.
        let result = tool
            .execute(serde_json::json!({"command": "cd /etc && ls"}))
            .await;
        // The command itself should succeed (it runs in a subshell)
        assert!(result.is_ok(), "command should execute successfully");

        // Verify the persistent cwd is still inside the sandbox root
        let cwd_result = tool
            .execute(serde_json::json!({"command": "pwd"}))
            .await
            .unwrap();
        let stdout = cwd_result["stdout"].as_str().unwrap();
        let cwd_path = std::path::Path::new(stdout.trim());
        assert!(
            std::fs::canonicalize(cwd_path)
                .unwrap_or_default()
                .starts_with(&root),
            "persistent cwd '{}' should remain inside sandbox root '{}'",
            stdout.trim(),
            root.display()
        );

        // #1262: and the agent has to be *told*. Before the revert notice
        // this test passed while the agent's only evidence was `exit_code: 0`
        // plus a real listing of /etc.
        let reverted_stdout = result.unwrap()["stdout"].as_str().unwrap().to_string();
        assert!(
            reverted_stdout.contains("[cwd unchanged:"),
            "the reverted `cd /etc` must be surfaced to the agent; got: {reverted_stdout:?}"
        );
    }

    /// A holder directory for tests that run a real shell and care about the
    /// cwd it reports back.
    ///
    /// Deliberately **not** `tempfile::tempdir()`: Windows Git Bash reports
    /// `%TEMP%` through its `/tmp` mount point, and `/tmp/...` is neither a
    /// usable Windows path nor a drive path `pathnorm::msys_to_windows` can
    /// rewrite — so a sandbox root under the system temp dir reads as
    /// out-of-root on *every* command there (a pre-existing resolution gap,
    /// sibling of #1261). Rooting these fixtures under the workspace
    /// `target/` dir keeps them on a path both shells can round-trip, so the
    /// test measures the containment rule instead of the mount table.
    fn shell_cwd_test_dir() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("shell-cwd-tests");
        std::fs::create_dir_all(&base).expect("create the shell-cwd test base dir");
        tempfile::Builder::new()
            .prefix("case-")
            .tempdir_in(&base)
            .expect("create a shell-cwd test dir")
    }

    /// #1262 — a reverted `cd` must reach the agent through the tool result,
    /// not just the daemon's `warn!` log. Covers the in-root case too: a
    /// notice on every result would be noise the agent learns to ignore.
    #[tokio::test]
    async fn test_shell_tool_reverted_cwd_is_visible_to_the_agent() {
        let dir = shell_cwd_test_dir();
        let outside = std::fs::canonicalize(dir.path()).unwrap();
        let root = outside.join("alms-cwd-revert-root");
        std::fs::create_dir_all(&root).unwrap();
        let tool = ShellTool::sandboxed(root.clone());

        let result = tool
            .execute(serde_json::json!({"command": "cd .. && pwd"}))
            .await
            .unwrap();

        // The command itself succeeds — which is exactly why `exit_code`
        // can never be the agent's signal that the `cd` was undone.
        assert_eq!(result["exit_code"], 0);

        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("[cwd unchanged:"),
            "the revert must be surfaced in the agent-visible stdout; got: {stdout:?}"
        );
        assert!(
            stdout.contains("outside the sandbox root"),
            "the notice must say why the cd was undone; got: {stdout:?}"
        );
        assert!(
            stdout.contains("alms-cwd-revert-root"),
            "the notice must name the cwd the next command actually runs in; got: {stdout:?}"
        );

        // ...and a command that stays inside the root is left alone. A
        // notice on every result would be noise the agent learns to ignore.
        let inside = tool
            .execute(serde_json::json!({"command": "pwd"}))
            .await
            .unwrap();
        let inside_stdout = inside["stdout"].as_str().unwrap();
        assert!(
            !inside_stdout.contains("[cwd unchanged:"),
            "an in-root command must not carry the revert notice; got: {inside_stdout:?}"
        );
    }

    /// #1262 — the same, end to end through the background path: submit,
    /// poll, and read the notice out of the `check_task` result.
    #[tokio::test]
    async fn test_shell_tool_background_reverted_cwd_is_visible_to_the_agent() {
        let dir = shell_cwd_test_dir();
        let outside = std::fs::canonicalize(dir.path()).unwrap();
        let root = outside.join("alms-bg-revert-root");
        std::fs::create_dir_all(&root).unwrap();
        let tool = ShellTool::sandboxed(root.clone());

        let submitted = tool
            .execute(serde_json::json!({
                "command": "cd .. && pwd",
                "run_in_background": true
            }))
            .await
            .unwrap();
        assert_eq!(submitted["status"], "submitted");
        let task_id = submitted["task_id"].as_str().unwrap().to_string();

        // Poll instead of sleeping a fixed interval: spawning a shell can be
        // slow (Git Bash on Windows especially) and a timing guess would
        // make this flaky rather than wrong.
        let mut checked = serde_json::Value::Null;
        for _ in 0..50 {
            checked = tool
                .execute(serde_json::json!({"check_task": task_id}))
                .await
                .unwrap();
            if checked["status"] != "not_found_or_still_running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            checked["status"], "completed",
            "background task should have completed; got: {checked}"
        );

        let stdout = checked["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("[cwd unchanged:"),
            "background results must carry the revert notice too; got: {stdout:?}"
        );
        assert!(
            stdout.contains("alms-bg-revert-root"),
            "the notice must name the kept cwd; got: {stdout:?}"
        );
    }

    /// A completed execution carrying a cwd revert, with no shell involved.
    /// `reason` is explicit at every call site because it is the one input
    /// that changes what the notice claims (#1262).
    fn reverted_output(stdout: &str, reason: CwdRejection) -> ShellOutput {
        ShellOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
            spill_path: None,
            cwd_revert: Some(CwdRevert {
                attempted: PathBuf::from("/etc"),
                kept: PathBuf::from("/work/agent-root"),
                reason,
            }),
        }
    }

    /// #1262 — the foreground result body is `decorate_stdout`'s output.
    /// The end-to-end tests above prove the notice fires; this one pins what
    /// it *says*, without depending on a shell to produce it.
    #[test]
    fn test_decorate_stdout_carries_the_cwd_revert_notice() {
        let tool = ShellTool::sandboxed(PathBuf::from("/work/agent-root"));
        let decorated =
            tool.decorate_stdout(&reverted_output("etc-listing\n", CwdRejection::OutsideRoot));

        assert!(
            decorated.starts_with("etc-listing\n"),
            "the command's own output must survive intact; got: {decorated:?}"
        );
        assert!(
            decorated.contains("[cwd unchanged:"),
            "the revert must be surfaced to the agent; got: {decorated:?}"
        );
        assert!(
            decorated.contains("'/etc' is outside the sandbox root"),
            "the notice must name the directory that was refused; got: {decorated:?}"
        );
        assert!(
            decorated.contains("still run in '/work/agent-root'"),
            "the notice must name the cwd the next command runs in; got: {decorated:?}"
        );
    }

    /// #1262 — the two rejection causes must not be conflated. `check_cwd`
    /// fails closed on a path it could not canonicalise, which is the same
    /// arm a genuine escape takes, so the notice may only claim "outside the
    /// sandbox root" where the check actually established that. Under
    /// Windows Git Bash's `/tmp` mount (#1266) the unverifiable case is
    /// *every* command, so a wrong claim here is not a rare edge — it is a
    /// false explanation repeated every turn, aimed at the component that
    /// reasons about it.
    #[test]
    fn test_unverifiable_cwd_notice_does_not_claim_the_path_was_outside() {
        let tool = ShellTool::sandboxed(PathBuf::from("/work/agent-root"));
        let decorated =
            tool.decorate_stdout(&reverted_output("listing\n", CwdRejection::NotVerifiable));

        assert!(
            decorated.contains("'/etc' could not be confirmed inside the sandbox root"),
            "an unresolvable cwd must be reported as unconfirmed; got: {decorated:?}"
        );
        assert!(
            !decorated.contains("is outside the sandbox root"),
            "the notice must not assert a cause the check never established; \
             got: {decorated:?}"
        );
        assert!(
            decorated.contains("still run in '/work/agent-root'"),
            "the actionable half must survive either way; got: {decorated:?}"
        );
    }

    /// The notice goes on its own line even when the command's output does
    /// not end in a newline — otherwise it is glued to the tail of real
    /// output and reads as part of it.
    #[test]
    fn test_cwd_revert_notice_is_never_glued_to_partial_output() {
        let tool = ShellTool::sandboxed(PathBuf::from("/work/agent-root"));
        let decorated = tool.decorate_stdout(&reverted_output(
            "no trailing newline",
            CwdRejection::OutsideRoot,
        ));
        assert!(
            decorated.contains("no trailing newline\n[cwd unchanged:"),
            "expected a line break before the notice; got: {decorated:?}"
        );
    }

    /// No revert, no decoration.
    #[test]
    fn test_decorate_stdout_leaves_a_clean_run_alone() {
        let tool = ShellTool::sandboxed(PathBuf::from("/work/agent-root"));
        let output = ShellOutput {
            exit_code: 0,
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            spill_path: None,
            cwd_revert: None,
        };
        assert_eq!(tool.decorate_stdout(&output), "hello\n");
    }

    /// #1262 — background results are built by `handle_check_task`, a
    /// separate serialisation path that had to have the spill marker
    /// retrofitted onto it once already (#811). Injecting a completed task
    /// straight into the state exercises that JSON shape on every platform.
    #[tokio::test]
    async fn test_check_task_result_carries_the_cwd_revert_notice() {
        let tool = ShellTool::sandboxed(PathBuf::from("/work/agent-root"));
        tool.state.background_tasks.lock().await.insert(
            "bg_1".to_string(),
            types::BackgroundTaskResult {
                task_id: "bg_1".to_string(),
                command: "cd /etc && ls".to_string(),
                output: Some(reverted_output("etc-listing\n", CwdRejection::OutsideRoot)),
                error: None,
            },
        );

        let result = tool
            .execute(serde_json::json!({"check_task": "bg_1"}))
            .await
            .unwrap();

        assert_eq!(result["status"], "completed");
        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("[cwd unchanged:"),
            "background results must carry the revert notice too; got: {stdout:?}"
        );
        assert!(
            stdout.contains("still run in '/work/agent-root'"),
            "the notice must name the kept cwd; got: {stdout:?}"
        );
    }

    /// #1262 — the description an agent reads must state the one restriction
    /// it is guaranteed to hit in sandboxed mode.
    #[test]
    fn test_sandboxed_description_states_cwd_confinement() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellTool::sandboxed(dir.path().to_path_buf());
        let desc = tool.description();
        assert!(
            desc.contains("confined to the sandbox root"),
            "sandboxed description must state the cwd confinement; got: {desc}"
        );
        assert!(
            desc.contains("[cwd unchanged: ...]"),
            "the description must name the marker the agent will actually see; got: {desc}"
        );
        // The two variants share one body by construction (`concat!`), so
        // prose added to the base can never reach only one of them.
        assert!(
            SHELL_DESCRIPTION_SANDBOXED.starts_with(SHELL_DESCRIPTION_UNRESTRICTED),
            "the sandboxed description must be the base description plus the confinement sentence"
        );
    }

    /// #1262 — and must not claim confinement where `exec.rs` skips it.
    /// Trading the old omission for a false claim would be no better.
    #[test]
    fn test_unrestricted_description_makes_no_confinement_claim() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("no sandbox root", ShellTool::new()),
            ("explicit no root", ShellTool::with_policy(None, true)),
            // The `[security].allow_full_os_access` shape: a root is still
            // configured, but `unrestricted` skips the containment check.
            (
                "root plus unrestricted",
                ShellTool::with_policy(Some(dir.path().to_path_buf()), true),
            ),
        ];
        for (label, tool) in cases {
            let desc = tool.description();
            assert!(
                !desc.contains("confined to the sandbox root"),
                "{label}: containment does not apply here, so the description must not claim it; got: {desc}"
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_background_execution() {
        let tool = ShellTool::new();

        // Submit background task
        let result = tool
            .execute(serde_json::json!({
                "command": "echo background_test",
                "run_in_background": true
            }))
            .await
            .unwrap();
        assert_eq!(result["status"], "submitted");
        let task_id = result["task_id"].as_str().unwrap();

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Check task result
        let result = tool
            .execute(serde_json::json!({"check_task": task_id}))
            .await
            .unwrap();
        assert_eq!(result["status"], "completed");
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .contains("background_test")
        );
    }

    #[tokio::test]
    async fn test_shell_tool_missing_command() {
        let tool = ShellTool::new();
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_tool_argv_rejected() {
        // argv mode has been removed; passing argv without command should fail
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"argv": ["echo", "hello"]}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_default_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspace");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("marker.txt"), "home").unwrap();

        let tool = ShellTool::new().with_default_cwd(ws_dir);
        let result = tool
            .execute(serde_json::json!({"command": "cat marker.txt"}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("home"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_tool_check_nonexistent_task() {
        let tool = ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"check_task": "bg_999"}))
            .await
            .unwrap();
        assert_eq!(result["status"], "not_found_or_still_running");
    }

    // ── Integration: permission check wired into execute() ───────────────

    /// Verify that `check_command()` is actually called from `ShellTool::execute()`.
    ///
    /// This catches regressions if someone accidentally removes or reorders
    /// the permission check in the execute path. The test does not require a
    /// real shell because the permission deny fires before any OS-level
    /// command execution.
    #[tokio::test]
    async fn test_shell_tool_respects_deny_permission() {
        let perms = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![r"^forbidden\b".to_string()],
            classifier_overrides: vec![],
        };
        let tool = ShellTool::new().with_permissions(&perms);

        // A denied command must fail through the full execute() path.
        let result = tool
            .execute(serde_json::json!({"command": "forbidden --do-evil"}))
            .await;
        assert!(
            result.is_err(),
            "denied command should be blocked by execute()"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Command blocked by security policy"),
            "error should come from the permission check, got: {err_msg}"
        );
    }

    /// Verify that allowlist-only mode is enforced through `execute()`.
    #[tokio::test]
    async fn test_shell_tool_respects_allow_permission() {
        let perms = ShellPermissions {
            allowed_commands: vec![r"^echo\b".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        let tool = ShellTool::new().with_permissions(&perms);

        // A command not in the allowlist must fail through the full execute() path.
        let result = tool
            .execute(serde_json::json!({"command": "curl http://evil.com"}))
            .await;
        assert!(
            result.is_err(),
            "command not in allowlist should be blocked by execute()"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("does not match any allowed command pattern"),
            "error should come from the allowlist check, got: {err_msg}"
        );
    }

    // ── Issue #745: classifier as non-bypassable floor ──────────────

    /// Precedence: a permissive allowlist (`.*`) does NOT bypass the
    /// classifier. `rm -rf /` is admitted by `check_command` but the
    /// classifier-floor still blocks it. This is the core of #745: the
    /// classifier is not a gate operators can disable by loosening allowlists.
    #[tokio::test]
    async fn test_permissive_allowlist_does_not_bypass_classifier() {
        let perms = ShellPermissions {
            allowed_commands: vec![r".*".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        // Warn mode: *before* #745 this was "log everything, block nothing".
        // After #745 it must still block destructive findings.
        let tool = ShellTool::new()
            .with_permissions(&perms)
            .with_classification_mode(CoreClassificationMode::Warn);

        let result = tool
            .execute(serde_json::json!({"command": "rm -rf /"}))
            .await;
        assert!(
            result.is_err(),
            "permissive allowlist + warn mode must not bypass classifier"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("risk classifier"),
            "error should come from the classifier floor, got: {msg}"
        );
    }

    /// Precedence: `ClassificationMode::Warn` now blocks destructive (issue
    /// #745). Previously it logged everything without blocking.
    #[tokio::test]
    async fn test_warn_mode_blocks_destructive() {
        let tool = ShellTool::new().with_classification_mode(CoreClassificationMode::Warn);
        let result = tool
            .execute(serde_json::json!({"command": "sudo rm -rf /"}))
            .await;
        assert!(result.is_err(), "Warn mode must block destructive findings");
    }

    /// Precedence: `ClassificationMode::Off` remains the genuine opt-out.
    /// Operators who explicitly disable the classifier get the pre-#745
    /// behaviour — no findings, no logs, no enforcement from the classifier
    /// layer. The hardcoded denylist in `exec.rs` still applies — so we use
    /// a destructive command (`shutdown -h now`) that the classifier flags
    /// but that the hardcoded denylist does not cover.
    #[tokio::test]
    async fn test_off_mode_is_true_opt_out_at_classifier_layer() {
        let perms = ShellPermissions {
            allowed_commands: vec![r".*".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![],
        };
        let tool = ShellTool::new()
            .with_permissions(&perms)
            .with_classification_mode(CoreClassificationMode::Off);

        let result = tool
            .execute(serde_json::json!({"command": "shutdown -h now"}))
            .await;
        // Command may fail at exec level (no privs, not installed, Windows, etc.)
        // but the classifier layer must not have been the cause.
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("risk classifier"),
                "Off mode must not block at classifier layer, got: {msg}"
            );
        }
    }

    /// Precedence: an explicit `classifier_overrides` entry bypasses the
    /// classifier for the matching command only. Operators use this for
    /// legitimate exemptions (e.g. a known-safe `sudo apt-get update` in a
    /// trusted deploy script).
    #[tokio::test]
    async fn test_classifier_override_bypasses_classifier() {
        let perms = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![],
            // Exempt a very specific command.
            classifier_overrides: vec![r"^sudo echo classifier-test$".to_string()],
        };
        let tool = ShellTool::new()
            .with_permissions(&perms)
            .with_classification_mode(CoreClassificationMode::BlockDestructive);

        // `sudo echo` without override is Destructive (PrivilegeEscalation).
        // With the explicit override, it must be permitted past the classifier.
        // We don't assert success at exec level (sudo may not be installed in CI),
        // just that the classifier did not fire.
        let result = tool
            .execute(serde_json::json!({"command": "sudo echo classifier-test"}))
            .await;
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("risk classifier"),
                "override should bypass classifier, got: {msg}"
            );
        }
    }

    /// Security: a classifier override does NOT weaken the deny list. An
    /// operator who overrides `^sudo\b` but also sets a deny on `sudo rm`
    /// still blocks `sudo rm`. Overrides are orthogonal to admission.
    #[tokio::test]
    async fn test_classifier_override_does_not_weaken_deny() {
        let perms = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![r"sudo\s+rm".to_string()],
            classifier_overrides: vec![r"^sudo\b".to_string()],
        };
        let tool = ShellTool::new().with_permissions(&perms);

        let result = tool
            .execute(serde_json::json!({"command": "sudo rm -rf /tmp/foo"}))
            .await;
        assert!(
            result.is_err(),
            "deny list must still apply even when override matches"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Command blocked by security policy"),
            "error must come from the deny list, not the classifier, got: {msg}"
        );
    }

    /// Security: a classifier override matches the command only — it does
    /// not silently widen the allowlist. Allowlist gating is orthogonal.
    #[tokio::test]
    async fn test_classifier_override_does_not_satisfy_allowlist() {
        let perms = ShellPermissions {
            allowed_commands: vec![r"^echo\b".to_string()],
            denied_commands: vec![],
            classifier_overrides: vec![r"^sudo\b".to_string()],
        };
        let tool = ShellTool::new().with_permissions(&perms);

        // `sudo` matches the override, but NOT the allowlist → must be
        // rejected at the allowlist layer (before the classifier even runs).
        let result = tool
            .execute(serde_json::json!({"command": "sudo whoami"}))
            .await;
        assert!(
            result.is_err(),
            "override must not substitute for allowlist match"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("does not match any allowed command pattern"),
            "error must come from the allowlist layer, got: {msg}"
        );
    }

    /// Integration: exercise the full `[tools.shell_permissions]` → ShellTool
    /// → outcome chain with an operator-configured override. End-to-end
    /// "does the config path actually wire up?" test.
    #[tokio::test]
    async fn test_full_config_to_outcome_with_override() {
        // Simulate operator config: empty allowlist (denylist-only mode),
        // one deny, one classifier override. A plausible shape for alms.toml.
        let config = ShellPermissions {
            allowed_commands: vec![],
            denied_commands: vec![r"rm\s+-rf\s+/".to_string()],
            classifier_overrides: vec![r"^sudo apt-get\b".to_string()],
        };
        let tool = ShellTool::new()
            .with_permissions(&config)
            .with_classification_mode(CoreClassificationMode::BlockDestructive);

        // 1. Deny list wins over everything.
        let r1 = tool
            .execute(serde_json::json!({"command": "rm -rf /"}))
            .await;
        assert!(r1.is_err(), "deny list must block");

        // 2. Override permits a classifier-flagged command past the classifier.
        let r2 = tool
            .execute(serde_json::json!({"command": "sudo apt-get update"}))
            .await;
        if let Err(e) = &r2 {
            let msg = e.to_string();
            assert!(
                !msg.contains("risk classifier"),
                "override must bypass classifier, got: {msg}"
            );
        }

        // 3. A destructive command NOT covered by the override still blocks.
        let r3 = tool
            .execute(serde_json::json!({"command": "sudo rm -rf /etc"}))
            .await;
        assert!(
            r3.is_err(),
            "non-overridden destructive command must still block"
        );
    }

    /// Regression guard: the `ShellTool` parameter schema does not expose any
    /// classifier-override surface to the model. `classifier_overrides` is
    /// operator-only (alms.toml) and must never reach the JSON input.
    #[test]
    fn test_classifier_override_not_in_tool_schema() {
        let tool = ShellTool::new();
        let schema = tool.parameters();
        let props = schema["properties"].as_object().unwrap();
        for key in props.keys() {
            let lower = key.to_ascii_lowercase();
            assert!(
                !lower.contains("override") && !lower.contains("classifier"),
                "tool input schema must not expose classifier-override surface to the model; found '{key}'"
            );
        }
    }

    // ── Large-output spill (issue #756) ───────────────────────────────────
    //
    // The spill path only fires when real captured bytes exceed
    // `MAX_OUTPUT_BYTES` (30 KB) so these tests spawn actual bash commands.
    // Gated on `#[cfg(unix)]` for the same reason other integration tests
    // here are: Unix-only `printf`/`yes` shapes keep the command short and
    // portable.

    /// Acceptance test #1: exceeding the truncation threshold triggers a
    /// spill file and the tool result body contains the
    /// `[full output spilled to: ...]` marker.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_spill_triggers_on_truncation() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-123");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        // ~40 KB of 'x' — well above MAX_OUTPUT_BYTES. `printf "%.0sx"` is
        // far faster and more portable than using `yes` or a Python loop.
        let n = MAX_OUTPUT_BYTES + 10_000;
        let cmd = format!("printf '%.0sx' $(seq 1 {n})");
        let result = tool
            .execute(serde_json::json!({"command": cmd}))
            .await
            .unwrap();

        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("[full output spilled to:"),
            "tool result must surface the spill marker; got (head): {}",
            &stdout[..stdout.len().min(200)]
        );

        // The spill directory must contain exactly one `shell_*.log` file.
        let entries: Vec<_> = std::fs::read_dir(&run_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one spill file");
        let spill_path = entries.into_iter().next().unwrap().unwrap().path();
        assert!(
            spill_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shell_"),
            "spill filename must match `shell_<id>.log`; got {}",
            spill_path.display()
        );
    }

    /// Acceptance test #2: the spilled file's bytes exactly equal the
    /// pre-truncation captured output — no truncation, no lossy UTF-8
    /// substitution mid-stream.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_spill_content_matches_input() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-content");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        // Produce a known deterministic byte stream: `n` 'a's followed by a
        // newline, with `n` chosen to blow past the threshold while still
        // being cheap on CI. The output must round-trip byte-for-byte.
        let n = MAX_OUTPUT_BYTES + 5_000;
        let cmd = format!("printf '%.0sa' $(seq 1 {n}); echo");
        let result = tool
            .execute(serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        let _ = result; // we care about the file on disk, not the tool body

        let entries: Vec<_> = std::fs::read_dir(&run_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one spill file");
        let spill_path = entries.into_iter().next().unwrap().unwrap().path();
        let spilled = std::fs::read(&spill_path).unwrap();

        // Expected output is `n` a's then a newline from the trailing `echo`.
        let mut expected = vec![b'a'; n];
        expected.push(b'\n');

        assert_eq!(
            spilled.len(),
            expected.len(),
            "spill file byte length must match the pre-truncation capture"
        );
        assert_eq!(
            spilled, expected,
            "spill file must contain the exact captured bytes"
        );
    }

    /// Regression test for #954 (complement to `test_shell_spill_content_matches_input`):
    /// when the user's command produces output WITHOUT a trailing newline, the spill
    /// must capture exactly that byte sequence — no over-stripping of the last byte.
    ///
    /// Before #954's fix, an unbounded backward walk in `strip_pwd_marker_bytes` ate
    /// the user's trailing newline. A naive "always strip the last byte" fix would
    /// instead lose the last byte when the user emits no trailing newline. This test
    /// pins the no-newline case so neither regression can sneak back in.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_spill_content_no_trailing_newline() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-no-newline");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        // `printf '%.0sa'` (no `; echo` afterwards) emits exactly n 'a' bytes with
        // no trailing newline. The single \n preceding the marker comes purely from
        // the wrapper's own `echo;` separator, and must be the only \n stripped.
        let n = MAX_OUTPUT_BYTES + 5_000;
        let cmd = format!("printf '%.0sa' $(seq 1 {n})");
        let result = tool
            .execute(serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        let _ = result;

        let entries: Vec<_> = std::fs::read_dir(&run_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one spill file");
        let spill_path = entries.into_iter().next().unwrap().unwrap().path();
        let spilled = std::fs::read(&spill_path).unwrap();

        // Expected output is exactly n 'a' bytes — no trailing newline.
        let expected = vec![b'a'; n];

        assert_eq!(
            spilled.len(),
            expected.len(),
            "spill file byte length must match the pre-truncation capture (no trailing newline)"
        );
        assert_eq!(
            spilled, expected,
            "spill file must contain exactly the user's bytes with no over-stripping"
        );
    }

    /// Regression test for #954: marker-stripping behavior itself must be preserved.
    /// The wrapper's PWD marker line and the trailing `pwd` line must not appear in
    /// the captured stdout, even though the user's output contains a real trailing
    /// newline. This guards the spill capture against a future "fix" that loosens
    /// the marker boundary in the other direction.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_spill_strips_pwd_marker_with_trailing_newline() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-marker-strip");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        // Big enough to spill, with a final `echo` that emits a user-owned trailing \n.
        let n = MAX_OUTPUT_BYTES + 1_000;
        let cmd = format!("printf '%.0sb' $(seq 1 {n}); echo");
        let result = tool
            .execute(serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        let _ = result;

        let entries: Vec<_> = std::fs::read_dir(&run_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one spill file");
        let spill_path = entries.into_iter().next().unwrap().unwrap().path();
        let spilled = std::fs::read(&spill_path).unwrap();

        // Marker token shape: __ALMS_PWD_<uuid simple>__ (see new_pwd_marker). Even
        // though each ShellTool gets a random nonce, the static prefix is what we
        // assert on — its presence in the spill would prove the marker line leaked.
        let marker_prefix = b"__ALMS_PWD_";
        assert!(
            !spilled
                .windows(marker_prefix.len())
                .any(|w| w == marker_prefix),
            "spill must not contain the PWD marker prefix — marker stripping regressed"
        );

        // And the user's bytes must be exactly n 'b's followed by a single '\n'.
        let mut expected = vec![b'b'; n];
        expected.push(b'\n');
        assert_eq!(
            spilled, expected,
            "spill must contain exactly the user's bytes (no over-strip, no under-strip)"
        );
    }

    /// `enabled: false` at the config layer must produce no spill file and
    /// no `[full output spilled to: ...]` marker in the tool result. This
    /// guards the opt-out path the operator gets when they set
    /// `[tools.shell.spill] enabled = false` in alms.toml.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_spill_disabled_by_config() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-disabled");
        // Explicitly disabled policy — `ShellSpillPolicy::disabled()` is
        // also the default, but we construct one with the run_dir set so
        // the test makes the intent unambiguous.
        let mut policy = ShellSpillPolicy::with_run_dir(run_dir.clone());
        policy.enabled = false;
        let tool = ShellTool::new().with_spill_policy(policy);

        let n = MAX_OUTPUT_BYTES + 5_000;
        let cmd = format!("printf '%.0sx' $(seq 1 {n})");
        let result = tool
            .execute(serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            !stdout.contains("[full output spilled to:"),
            "no spill marker should appear when spill is disabled"
        );
        assert!(
            !run_dir.exists() || std::fs::read_dir(&run_dir).unwrap().next().is_none(),
            "no spill files should be written when spill is disabled"
        );
    }

    /// Regression test for issue #811: background-task results must surface
    /// the same `[full output spilled to: ...]` marker as the foreground
    /// path when stdout exceeds the inline truncation budget. Without the
    /// marker, the agent can't recover the full output from the spill file.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_background_task_includes_spill_marker() {
        use crate::shell::spill::ShellSpillPolicy;
        use crate::shell::types::MAX_OUTPUT_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-bg-spill");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        let n = MAX_OUTPUT_BYTES + 5_000;
        let cmd = format!("printf '%.0sx' $(seq 1 {n})");

        // Submit as a background task.
        let submit = tool
            .execute(serde_json::json!({
                "command": cmd,
                "run_in_background": true,
            }))
            .await
            .unwrap();
        assert_eq!(submit["status"], "submitted");
        let task_id = submit["task_id"].as_str().unwrap().to_string();

        // Wait for completion. The command itself is fast — half a second
        // covers the spawn + write + finalize handshake even on slow CI.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let result = tool
            .execute(serde_json::json!({"check_task": task_id}))
            .await
            .unwrap();
        assert_eq!(result["status"], "completed");

        let stdout = result["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("[full output spilled to:"),
            "background-task stdout must surface the spill marker; got (head): {}",
            &stdout[..stdout.len().min(200)]
        );

        // The spill directory must contain exactly one `shell_*.log` file
        // matching the path embedded in the marker, and that file must hold
        // the pre-truncation bytes — proving the marker actually points at
        // a readable artifact the agent can fs_read.
        let entries: Vec<_> = std::fs::read_dir(&run_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one spill file");
        let spill_path = entries.into_iter().next().unwrap().unwrap().path();
        let spilled = std::fs::read(&spill_path).unwrap();
        assert_eq!(
            spilled.len(),
            n,
            "spill file must contain the full pre-truncation capture"
        );

        let file_name = spill_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            stdout.contains(&file_name),
            "marker must reference the on-disk spill file; stdout tail: {}",
            &stdout[stdout.len().saturating_sub(200)..]
        );
    }

    /// Companion to `test_shell_background_task_includes_spill_marker`:
    /// small-output background tasks must NOT gain a spurious spill marker.
    /// Guards the `None` arm of `append_spill_marker`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_background_task_no_marker_when_no_spill() {
        use crate::shell::spill::ShellSpillPolicy;

        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-bg-small");
        let tool =
            ShellTool::new().with_spill_policy(ShellSpillPolicy::with_run_dir(run_dir.clone()));

        let submit = tool
            .execute(serde_json::json!({
                "command": "echo small_bg_output",
                "run_in_background": true,
            }))
            .await
            .unwrap();
        let task_id = submit["task_id"].as_str().unwrap().to_string();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let result = tool
            .execute(serde_json::json!({"check_task": task_id}))
            .await
            .unwrap();
        assert_eq!(result["status"], "completed");
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("small_bg_output"));
        assert!(
            !stdout.contains("[full output spilled to:"),
            "small-output background tasks must not carry a spill marker; got: {stdout}"
        );
    }

    // ── Windows test parity (issue #746) ─────────────────────────────────
    //
    // Alper's daily driver is Windows, and the integration tests above are
    // gated on `#[cfg(unix)]` because Unix-style absolute paths (`/tmp`,
    // `/etc`) and Unix-only utilities (`env`) don't apply on Windows. This
    // module mirrors the parts of the Unix integration suite that belong on
    // Windows too: env inheritance (area 1) and permission-regex behaviour
    // on Windows-shaped command strings (area 4).
    //
    // All commands still go through `bash -c` because that is the only shell
    // the tool dispatches to — on Windows runners this requires Git Bash on
    // PATH, which is a default on GitHub-hosted `windows-latest` images. The
    // tests stay minimal and use `printenv` / `echo` rather than `env` so
    // they run identically under Git Bash.
    //
    // Areas 2 (CWD persistence round-trip) and 3 (background-task cleanup
    // on process handle drop) are deferred to follow-up issues — those
    // require more careful setup (Windows path canonicalisation / drive
    // letters, Windows process-group semantics) than fits in a pure
    // test-parity PR.
    #[cfg(windows)]
    mod windows_tests {
        use super::*;

        // ── Area 1: environment inheritance ─────────────────────────────

        /// On Windows, `env_clear()` followed by selective re-injection of
        /// `platform_critical_env_vars()` must still leave no API-key
        /// variables in the child environment. This mirrors the Unix
        /// `test_shell_tool_env_cleared` test.
        #[tokio::test]
        async fn test_shell_tool_env_cleared_windows() {
            let tool = ShellTool::new();
            // `printenv` is provided by Git Bash on Windows runners and is
            // the most portable way to enumerate the child environment —
            // Windows' native `set` only works in cmd.exe, not bash.
            let result = tool
                .execute(serde_json::json!({"command": "printenv || env"}))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap();
            assert!(
                !stdout.contains("OPENROUTER_API_KEY"),
                "API keys must not leak into spawned processes on Windows"
            );
            assert!(
                !stdout.contains("ANTHROPIC_API_KEY"),
                "Anthropic API key must not leak on Windows"
            );
            assert!(
                stdout.contains("PATH="),
                "PATH must be re-injected after env_clear() on Windows"
            );
        }

        /// `default_env` injected via `with_default_env` must reach the
        /// child process on Windows. Mirrors Unix `test_shell_tool_default_env`.
        #[tokio::test]
        async fn test_shell_tool_default_env_windows() {
            let mut env = HashMap::new();
            env.insert("ALMS_DATA_DIR".to_string(), "C:\\alms-test".to_string());
            let tool = ShellTool::new().with_default_env(env);

            let result = tool
                .execute(serde_json::json!({"command": "printenv ALMS_DATA_DIR"}))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap();
            assert!(
                stdout.contains("C:\\alms-test") || stdout.contains("C:/alms-test"),
                "default_env should propagate to child on Windows; got: {stdout:?}"
            );
        }

        /// Per-call `env` parameter must override `default_env` on Windows.
        /// Mirrors Unix `test_shell_tool_env_override`.
        #[tokio::test]
        async fn test_shell_tool_env_override_windows() {
            let mut env = HashMap::new();
            env.insert("ALMS_DATA_DIR".to_string(), "C:\\default".to_string());
            let tool = ShellTool::new().with_default_env(env);

            let result = tool
                .execute(serde_json::json!({
                    "command": "printenv ALMS_DATA_DIR",
                    "env": {"ALMS_DATA_DIR": "C:\\override"}
                }))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap();
            assert!(
                stdout.contains("C:\\override") || stdout.contains("C:/override"),
                "per-call env should override default_env on Windows; got: {stdout:?}"
            );
            assert!(
                !stdout.contains("C:\\default") && !stdout.contains("C:/default"),
                "default value should not appear when overridden; got: {stdout:?}"
            );
        }

        /// Secret-env filtering must apply on Windows too. Mirrors Unix
        /// `test_shell_tool_blocks_secret_env`.
        #[tokio::test]
        async fn test_shell_tool_blocks_secret_env_windows() {
            let tool = ShellTool::new();
            let result = tool
                .execute(serde_json::json!({
                    "command": "printenv",
                    "env": {
                        "OPENAI_API_KEY": "stolen",
                        "SAFE_VAR_WIN": "ok"
                    }
                }))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap();
            assert!(
                !stdout.contains("OPENAI_API_KEY"),
                "secret env var must be filtered on Windows"
            );
            assert!(
                stdout.contains("SAFE_VAR_WIN=ok"),
                "non-secret custom env var should be passed through on Windows"
            );
        }

        /// The per-tool PWD marker (issue #743) must also be generated on
        /// Windows — there was previously no Windows assertion for this and
        /// the marker is security-relevant.
        #[test]
        fn test_pwd_marker_generated_on_windows() {
            let tool = ShellTool::new();
            let marker = tool.pwd_marker();
            assert!(
                marker.starts_with("__ALMS_PWD_"),
                "marker prefix must be stable across platforms"
            );
            assert_eq!(
                marker.len(),
                45,
                "marker length (prefix + UUID simple + suffix) must match on Windows"
            );
        }

        /// Cloning a `ShellTool` on Windows must preserve the PWD marker so
        /// the `shell_exec` alias recovers the cwd through the same token.
        #[test]
        fn test_pwd_marker_preserved_across_clone_on_windows() {
            let a = ShellTool::new();
            let b = a.clone();
            assert_eq!(
                a.pwd_marker(),
                b.pwd_marker(),
                "cloned ShellTool must share the marker on Windows"
            );
        }

        /// Platform-critical env vars on Windows include `SystemRoot`,
        /// `PATHEXT`, `COMSPEC`, `PATH`. When these are present in the
        /// daemon process they must be re-injected; this prevents `bash`
        /// and basic utilities from failing.
        ///
        /// Note: Git Bash on Windows exposes env-var names in uppercase
        /// through `printenv` (e.g. `SYSTEMROOT` rather than `SystemRoot`).
        /// Windows env-var names are case-insensitive on the kernel side,
        /// so we match case-insensitively too.
        #[tokio::test]
        async fn test_shell_tool_windows_platform_env_present() {
            let tool = ShellTool::new();
            let result = tool
                .execute(serde_json::json!({"command": "printenv"}))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap();
            let upper = stdout.to_ascii_uppercase();
            // PATH is always re-injected.
            assert!(
                upper.contains("PATH="),
                "PATH must be present in child env on Windows; got: {stdout:?}"
            );
            // SystemRoot is set on any sane Windows host. If the daemon had
            // it, the child must too.
            if std::env::var_os("SystemRoot").is_some() {
                assert!(
                    upper.contains("SYSTEMROOT="),
                    "SystemRoot must be re-injected on Windows when present; got: {stdout:?}"
                );
            }
        }

        // ── Area 4: permission-regex matching on Windows command shapes ─

        /// Permission regex matching is a pure-string operation so the
        /// behaviour should be identical to Unix. This test exercises a
        /// small but Windows-shaped sample to guard against someone
        /// accidentally making the matcher platform-dependent in the
        /// future (e.g. by splitting on shell tokens or calling into
        /// OS-level path APIs).
        #[test]
        fn test_permissions_match_windows_command_strings() {
            let perms = ShellPermissions {
                allowed_commands: vec![],
                denied_commands: vec![
                    // Windows-style invocation that a prompt-injection
                    // attacker might try on Windows:
                    r"^cmd(\.exe)?\s+/c\b".to_string(),
                    r"^powershell(\.exe)?\b".to_string(),
                ],
                classifier_overrides: vec![],
            };
            let compiled = CompiledPermissions::compile(&perms);

            assert!(compiled.check_command("cmd /c del /f C:\\Windows").is_err());
            assert!(
                compiled
                    .check_command("cmd.exe /c rmdir /s /q C:\\Users")
                    .is_err()
            );
            assert!(
                compiled
                    .check_command("powershell -Command \"Remove-Item -Recurse\"")
                    .is_err()
            );
            assert!(
                compiled
                    .check_command("powershell.exe -EncodedCommand foo")
                    .is_err()
            );
            // Legitimate bash commands still pass the Windows-shaped denylist.
            assert!(compiled.check_command("echo hello").is_ok());
            assert!(compiled.check_command("ls -la").is_ok());
        }

        /// Windows paths contain backslashes; regex patterns authored by
        /// the operator may or may not escape them. The matcher must treat
        /// the command string as an opaque byte sequence — no
        /// interpretation of path separators.
        #[test]
        fn test_permissions_respect_backslash_paths_on_windows() {
            let perms = ShellPermissions {
                allowed_commands: vec![],
                // Block any command that touches C:\Windows (escaped
                // backslash in the regex — matches literal `C:\Windows`).
                denied_commands: vec![r"C:\\Windows".to_string()],
                classifier_overrides: vec![],
            };
            let compiled = CompiledPermissions::compile(&perms);

            assert!(
                compiled
                    .check_command("type C:\\Windows\\System32\\drivers\\etc\\hosts")
                    .is_err(),
                "backslash-path denylist must match Windows command strings"
            );
            // Forward-slash variant must NOT match — the user wrote the
            // pattern for literal backslashes. This guards against anyone
            // silently normalising separators in the matcher.
            assert!(
                compiled
                    .check_command("cat C:/Windows/System32/drivers/etc/hosts")
                    .is_ok(),
                "forward-slash path must not match a backslash-only pattern"
            );
        }

        /// Allowlist-only mode on Windows: the same regex behaviour must
        /// apply as on Unix.
        #[test]
        fn test_permissions_allowlist_on_windows_command_strings() {
            let perms = ShellPermissions {
                // Only allow invocations of `git` and `echo`.
                allowed_commands: vec![r"^git\b".to_string(), r"^echo\b".to_string()],
                denied_commands: vec![],
                classifier_overrides: vec![],
            };
            let compiled = CompiledPermissions::compile(&perms);

            assert!(compiled.check_command("git status").is_ok());
            assert!(compiled.check_command("echo hello").is_ok());
            // Windows-shaped outsider must still be blocked by allowlist.
            assert!(compiled.check_command("cmd /c dir").is_err());
            assert!(compiled.check_command("powershell -File foo.ps1").is_err());
        }

        /// Deny-wins semantics on Windows-shaped command strings: the
        /// combined allow+deny evaluation order must not depend on
        /// platform.
        #[test]
        fn test_permissions_deny_wins_on_windows() {
            let perms = ShellPermissions {
                allowed_commands: vec![r"^git\b".to_string()],
                // Even among allowed git commands, never let a force-push
                // through. This mirrors the Unix `deny_takes_precedence_over_allow`
                // test but using a Windows-style remote path in the assertion.
                denied_commands: vec![r"git\s+push\s+.*--force".to_string()],
                classifier_overrides: vec![],
            };
            let compiled = CompiledPermissions::compile(&perms);

            assert!(compiled.check_command("git status").is_ok());
            assert!(compiled.check_command("git push origin main").is_ok());
            assert!(
                compiled
                    .check_command("git push --force origin main")
                    .is_err()
            );
            assert!(
                compiled
                    .check_command("git push \\\\server\\share main --force")
                    .is_err(),
                "deny should fire even with Windows UNC-style path args"
            );
        }

        /// Wiring check: the permission gate in `execute()` must fire on
        /// Windows too. This catches regressions if a future refactor
        /// accidentally `#[cfg(unix)]`-gates the permission check.
        #[tokio::test]
        async fn test_shell_tool_respects_deny_permission_on_windows() {
            let perms = ShellPermissions {
                allowed_commands: vec![],
                denied_commands: vec![r"^forbidden\b".to_string()],
                classifier_overrides: vec![],
            };
            let tool = ShellTool::new().with_permissions(&perms);

            let result = tool
                .execute(serde_json::json!({"command": "forbidden --do-evil"}))
                .await;
            assert!(
                result.is_err(),
                "denied command should be blocked by execute() on Windows"
            );
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("Command blocked by security policy"),
                "error should come from the permission check on Windows, got: {err_msg}"
            );
        }
    }
}
