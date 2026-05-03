mod context;
pub(crate) mod dm;
pub(crate) mod helpers;
mod loop_impl;
pub mod types;

#[cfg(test)]
mod tests;

// Re-export public types so external callers see the same API.
pub use types::{AgentConfig, Posture, RunOutput, SystemPrompts};

use crate::events::{PHASE_BUILDING_CONTEXT, RuntimeEventSender};
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use crate::workspace::AgentWorkspace;
use crate::workspace_tool::WorkspaceWriteTool;
use alms_core::{AgentId, AlmsError, AlmsResult, sanitize_error_for_session};
use alms_session::{
    Content as SessionContent, Message as SessionMessage, Role as SessionRole, SessionManager,
};
use tokio_util::sync::CancellationToken;
use tracing::{Span, info, instrument, warn};

/// Agent runtime - executes agent loops
#[derive(Debug)]
pub struct AgentRuntime {
    pub(crate) agent_id: AgentId,
    pub(crate) config: AgentConfig,
    pub(crate) llm: LlmClient,
    /// Optional dedicated LLM client for in-loop sliding-summary generation
    /// (#866). When `Some`, `maybe_summarize` uses this client instead of
    /// `llm` so the summary task can target a different provider than the
    /// agent (e.g. agent on Anthropic, summary on OpenRouter). When `None`,
    /// summaries inherit the agent's `llm` (pre-#866 behaviour).
    pub(crate) summary_llm: Option<LlmClient>,
    pub(crate) tools: ToolRegistry,
    pub(crate) workspace: Option<AgentWorkspace>,
    /// Optional channel for emitting runtime events to the gateway layer.
    pub(crate) event_sender: Option<RuntimeEventSender>,
    /// Run ID for audit event correlation (set by gateway before execution).
    pub(crate) run_id: Option<alms_core::RunId>,
    /// Per-run cancellation token for cooperative cancellation.
    pub(crate) cancel_token: Option<CancellationToken>,
    /// Resolved sandbox root (canonicalized). Retained so `with_workspace()` can
    /// re-register the shell tool with the workspace dir as default cwd.
    pub(crate) resolved_sandbox_root: Option<std::path::PathBuf>,
    /// Whether shell commands bypass sandbox cwd restriction.
    pub(crate) shell_unrestricted: bool,
    /// Default env vars injected into shell processes (e.g. ALMS_DATA_DIR).
    /// Retained so `with_workspace()` can pass them to the re-registered shell tool.
    pub(crate) shell_default_env: std::collections::HashMap<String, String>,
    /// Permission-based allow/deny patterns for shell commands.
    /// Retained so re-registrations of the shell tool preserve the policy.
    pub(crate) shell_permissions: alms_core::config::ShellPermissions,
    /// Built-in risk classification mode for shell commands.
    /// Retained so re-registrations of the shell tool preserve the mode.
    pub(crate) shell_classification_mode: alms_core::config::ShellClassificationMode,
    /// Large-output spill-to-disk policy for the shell tool (issue #756).
    /// Defaults to disabled; the gateway wires in an active policy once the
    /// per-run spill directory (`{data_dir}/shell_output/{run_id}/`) is
    /// known, via [`Self::with_shell_spill`]. Retained so re-registrations
    /// of the shell tool (from `with_workspace`, `with_shell_default_env`,
    /// etc.) preserve the policy.
    pub(crate) shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy,
    /// Shared in-loop tool-output truncation policy (issue #851).
    ///
    /// Mirrors [`shell_spill_policy`][Self::shell_spill_policy] but applies
    /// to *every* tool's result, not just shell. Defaults to disabled; the
    /// gateway wires in an active policy via [`Self::with_tool_output_truncate`]
    /// once the per-run spill directory
    /// (`{data_dir}/tool-output/{run_id}/`) is known. The agent loop runs
    /// every tool's JSON result through
    /// [`tool_output_truncate::truncate`][crate::tool_output_truncate::truncate]
    /// before pushing it into the live `messages` vec or persisting to the
    /// session DB so a single oversized tool output cannot blow the LLM's
    /// context window.
    pub(crate) tool_output_truncate_policy: crate::tool_output_truncate::ToolOutputTruncatePolicy,
    /// Accumulator of additional read-only roots that the read-family `fs_*`
    /// tools (`fs_read`, `fs_list`, `fs_grep`, `fs_glob`) should be allowed
    /// to read from beyond `resolved_sandbox_root`.
    ///
    /// Populated by builder methods that introduce per-run spill directories
    /// outside the agent's primary sandbox root:
    /// - [`Self::with_shell_spill`] appends `{data_dir}/shell_output/{run_id}/`.
    /// - [`Self::with_tool_output_truncate`] appends `{data_dir}/tool-output/{run_id}/`.
    ///
    /// [`Self::with_workspace`] reads the full accumulated list when it
    /// re-registers the read-family fs tools, so the workspace registration
    /// (which is the *last* fs_* registration in the gateway lifecycle order)
    /// preserves every per-run spill dir as a read root. Without this single
    /// source of truth, the spill-builder methods would have to re-register
    /// fs_* tools themselves AND `with_workspace` would silently overwrite
    /// those extras — the dead-code / footgun pattern Tim flagged on #921
    /// review.
    pub(crate) extra_fs_read_roots: Vec<std::path::PathBuf>,
    /// Agent name for perspective mapping in DM sessions.
    /// When set and the context_id starts with "dm:", the context builder
    /// maps messages from this agent to `Role::Assistant` so the LLM sees
    /// them as its own previous responses.
    pub(crate) agent_name: Option<String>,
}

impl AgentRuntime {
    /// Create new agent runtime.
    ///
    /// Returns an error if `sandbox_root` is non-empty but cannot be resolved
    /// (fail-closed: refuses to widen the sandbox silently).
    pub fn new(agent_id: AgentId, config: AgentConfig, llm: LlmClient) -> AlmsResult<Self> {
        // Resolve sandbox root: empty string = unrestricted, otherwise canonicalize.
        let sandbox_root = if config.sandbox_root.is_empty() {
            None
        } else {
            let path = std::path::PathBuf::from(&config.sandbox_root);
            let canonical = std::fs::canonicalize(&path).map_err(|e| {
                AlmsError::InvalidConfig(format!(
                    "tools.sandbox_root '{}' cannot be resolved: {}. \
                     Set sandbox_root = \"\" to explicitly opt out of sandboxing.",
                    path.display(),
                    e,
                ))
            })?;
            info!(sandbox_root = %canonical.display(), "Filesystem sandbox active");

            // Platform-specific sandboxing information
            #[cfg(target_os = "linux")]
            if config.shell_policy == "sandboxed" {
                info!(
                    sandbox_root = %canonical.display(),
                    "Landlock filesystem sandbox will be applied to shell commands (Linux 5.13+)"
                );
            }
            #[cfg(not(target_os = "linux"))]
            if config.shell_policy == "sandboxed" {
                warn!(
                    sandbox_root = %canonical.display(),
                    "Shell sandbox restricts cwd only on this platform (non-Linux). \
                     Shell commands can access files outside the sandbox root at the OS level. \
                     For true filesystem isolation, deploy on Linux 5.13+ (Landlock) \
                     or run the daemon as a restricted OS user."
                );
            }

            Some(canonical)
        };
        let shell_unrestricted = config.shell_policy == "unrestricted";
        let tools = ToolRegistry::with_builtins_sandboxed_ex(
            sandbox_root.clone(),
            shell_unrestricted,
            &config.enabled_tools,
            config.fs_edit_fuzzy_match,
        );

        let shell_permissions = config.shell_permissions.clone();
        let shell_classification_mode = config.shell_classification_mode;

        let mut runtime = Self {
            agent_id,
            config,
            llm,
            summary_llm: None,
            tools,
            workspace: None,
            event_sender: None,
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: sandbox_root,
            shell_unrestricted,
            shell_default_env: std::collections::HashMap::new(),
            shell_permissions: shell_permissions.clone(),
            shell_classification_mode,
            shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
            tool_output_truncate_policy:
                crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
            extra_fs_read_roots: Vec::new(),
            agent_name: None,
        };

        // Apply shell permissions / classification to the initially registered
        // shell tool. The shell tool is re-registered whenever
        // with_workspace() or with_shell_default_env() is called, but we also
        // need it on the initial tool so unnamed agents get the policy.
        //
        // Always re-register so that a non-default classification mode
        // (e.g. Strict) takes effect even when no permission patterns are set.
        runtime.apply_shell_permissions();

        Ok(runtime)
    }

    /// Set the project root — the agent's filesystem sandbox boundary
    /// (#945, the workspace v2 redesign).
    ///
    /// This re-registers `fs_read`, `fs_write`, `fs_list`, `fs_edit`,
    /// `fs_grep`, `fs_glob`, and `shell` (plus its `shell_exec` alias) so
    /// every file-touching tool enforces against the same single root.
    /// `shell`'s persistent cwd is also rooted at `project_root` so the
    /// agent's mental model is "I am working on the project."
    ///
    /// Sibling-workspace reads (#242) keep working without an extras list
    /// here: every agent's metadata lives at
    /// `<project_root>/.alms/agents/<name>/`, which is naturally inside
    /// the project-root sandbox, so `fs_read('.alms/agents/<sibling>/personality.md')`
    /// resolves under the primary root by construction. The
    /// `extra_fs_read_roots` accumulator still threads through for
    /// per-run spill directories (`with_shell_spill`,
    /// `with_tool_output_truncate`) so the gateway can wire those *before*
    /// `with_project_root` and the read-family tools pick them up here.
    ///
    /// Canonicalises the path so Windows `\\?\` prefix mismatches do not
    /// trip `check_sandbox_path`'s `starts_with` comparison. Falls back to
    /// the as-is path with a warning if canonicalisation fails — same
    /// fail-soft behaviour `with_workspace` used pre-#945.
    pub fn with_project_root(mut self, project_root: std::path::PathBuf) -> Self {
        // Make sure the directory exists before canonicalising — the
        // gateway already calls `create_dir_all` on the project root, but
        // unit-test paths sometimes pass a tempdir-relative subpath.
        if let Err(e) = std::fs::create_dir_all(&project_root) {
            warn!(
                error = %e,
                project_root = %project_root.display(),
                "Failed to create project root — fs tools may not work correctly"
            );
        }

        let canonical_root = match std::fs::canonicalize(&project_root) {
            Ok(canonical) => canonical,
            Err(e) => {
                warn!(
                    error = %e,
                    project_root = %project_root.display(),
                    "Cannot canonicalize project root — using as-is"
                );
                project_root.clone()
            }
        };

        self.resolved_sandbox_root = Some(canonical_root.clone());

        // Re-register fs_* tools with the project root as the single
        // primary sandbox root. Per-run spill dirs threaded through
        // `extra_fs_read_roots` come along for the ride via
        // `compose_fs_extra_read_roots`.
        let extras = self.compose_fs_extra_read_roots(&[]);
        self.register_fs_tools(Some(canonical_root.clone()), &extras);

        // Re-register shell with the project root as both sandbox root
        // and default cwd. The tool is registered under both "shell"
        // (primary) and "shell_exec" (alias).
        let enabled = &self.config.enabled_tools;
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled {
            let mut shell_tool = alms_sandbox::ShellTool::with_policy(
                Some(canonical_root.clone()),
                self.shell_unrestricted,
            )
            .with_default_cwd(canonical_root)
            .with_permissions(&self.shell_permissions)
            .with_classification_mode(self.shell_classification_mode)
            .with_spill_policy(self.shell_spill_policy.clone());
            if !self.shell_default_env.is_empty() {
                shell_tool = shell_tool.with_default_env(self.shell_default_env.clone());
            }
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }

        self
    }

    /// Drop the agent's filesystem-sandbox root so `fs_*` and `shell` run
    /// without a path prefix to enforce (#947 — the
    /// `[security].allow_full_os_access` operator escape hatch).
    ///
    /// Replaces every read-family fs_* tool (`fs_read`, `fs_list`,
    /// `fs_grep`, `fs_glob`) and every write-family fs_* tool (`fs_write`,
    /// `fs_edit`) with their unsandboxed (`::new()`) variants, and
    /// re-registers `shell` (and its `shell_exec` alias) with no sandbox
    /// root and no default cwd. Subject to OS-level permissions of the
    /// daemon process — and to `shell_permissions` / the destructive-command
    /// classifier, which are independent operator policy and remain
    /// active. The accumulated `extra_fs_read_roots` (per-run spill
    /// directories) become irrelevant when there is no primary root to
    /// extend, so the field is cleared to keep the bookkeeping honest.
    ///
    /// Must be called *after* `AgentRuntime::new` (which installs the
    /// initial sandboxed tools from `config.sandbox_root`) and BEFORE
    /// `with_workspace` so the unrestricted fs_*/shell tools are what
    /// `with_workspace` sees when it (post-#945) re-registers
    /// `workspace_write` only — `with_workspace` no longer touches
    /// fs_*/shell registration, but the ordering invariant still holds
    /// for forward-compatibility. `with_shell_default_env` is the
    /// exception: it is called BEFORE `with_unrestricted_filesystem`
    /// at every call site (gateway HTTP path, Telegram path, coordinator
    /// subagent path) and the unrestricted shell registration reads
    /// `self.shell_default_env` directly, so the env vars are
    /// preserved across the re-registration. The gateway's run lifecycle
    /// wires the call between the spill / shell-env builders and the
    /// project-root / workspace builders for exactly this reason.
    ///
    /// Worktree-mode-git interaction: this method is the runtime
    /// equivalent of "the operator said this agent is unsandboxed". When
    /// worktree-mode is wired up later (#938), this method takes
    /// precedence — `allow_full_os_access` wins. The startup-time WARN
    /// fired from `Gateway::start` documents that precedence to operators.
    pub fn with_unrestricted_filesystem(mut self) -> Self {
        use alms_sandbox::{
            FsEditTool, FsGlobTool, FsGrepTool, FsListTool, FsReadTool, FsWriteTool,
        };

        self.resolved_sandbox_root = None;
        // Per-run spill dirs are still real paths the agent may want to
        // read, but with no primary root to enforce against `extras` is
        // a no-op — the unsandboxed fs_* tools accept absolute paths
        // anywhere. Clear the accumulator so later builders that consult
        // it (e.g. a re-call into `register_fs_tools` from
        // `with_workspace`) do not re-introduce a sandboxed registration.
        self.extra_fs_read_roots.clear();

        let cache = self.tools.file_state_cache().clone();
        let enabled = &self.config.enabled_tools;
        let tool_enabled = |name: &str| enabled.is_empty() || enabled.iter().any(|t| t == name);

        if tool_enabled("fs_read") {
            self.tools.register(std::sync::Arc::new(
                FsReadTool::new().with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_write") {
            self.tools.register(std::sync::Arc::new(
                FsWriteTool::new().with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_list") {
            self.tools.register(std::sync::Arc::new(FsListTool::new()));
        }
        if tool_enabled("fs_edit") {
            self.tools.register(std::sync::Arc::new(
                FsEditTool::new()
                    .with_cache(cache)
                    .with_fuzzy_match(self.config.fs_edit_fuzzy_match),
            ));
        }
        if tool_enabled("fs_grep") {
            self.tools.register(std::sync::Arc::new(FsGrepTool::new()));
        }
        if tool_enabled("fs_glob") {
            self.tools.register(std::sync::Arc::new(FsGlobTool::new()));
        }

        // Re-register the shell tool with no sandbox root and no default
        // cwd. `shell_permissions` and `shell_classification_mode` ride
        // along — they are independent operator policy.
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled {
            let mut shell_tool =
                alms_sandbox::ShellTool::with_policy(None, /* unrestricted */ true)
                    .with_permissions(&self.shell_permissions)
                    .with_classification_mode(self.shell_classification_mode)
                    .with_spill_policy(self.shell_spill_policy.clone());
            if !self.shell_default_env.is_empty() {
                shell_tool = shell_tool.with_default_env(self.shell_default_env.clone());
            }
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }
        // `shell_unrestricted` controls whether the shell tool's persistent
        // cwd may escape the sandbox. With no sandbox root at all this flag
        // is moot, but flipping it to `true` keeps the runtime invariants
        // tidy: `resolved_sandbox_root.is_none() ↔ shell_unrestricted`.
        self.shell_unrestricted = true;

        self
    }

    /// Attach an agent workspace for persistent identity files.
    ///
    /// Registers the `workspace_write` tool so the agent can update its
    /// `personality.md` / `goals.md` / `memories.md` / `user.md` during
    /// runs. Ensures the workspace directory exists.
    ///
    /// Pre-#945 this method also re-targeted the filesystem sandbox at
    /// the workspace dir; v2 collapses the two-root model so the
    /// project-root sandbox set by [`Self::with_project_root`] is the
    /// single source of truth and `with_workspace` no longer touches
    /// sandbox paths or default cwds. The agent's metadata lives at
    /// `<project_root>/.alms/agents/<name>/` — naturally inside the
    /// project-root sandbox — so the previous parent-dir
    /// `extra_read_roots` shim for sibling reads (#242) is also gone:
    /// `fs_read('.alms/agents/<sibling>/personality.md')` resolves
    /// directly under the primary root.
    ///
    /// **Note**: `workspace_write` registration goes through `ToolRegistry::register()`,
    /// which checks the `enabled_filter`. If the operator has set `tools.enabled`
    /// and `workspace_write` is not in the list, it will be silently skipped.
    /// This is intentional — the operator's allowlist should be the single
    /// source of truth for which tools are available.
    pub fn with_workspace(mut self, workspace: AgentWorkspace) -> Self {
        let tool = WorkspaceWriteTool::new(workspace.clone());
        // Subject to enabled_filter — see doc comment above.
        self.tools.register(std::sync::Arc::new(tool));

        // Ensure the workspace directory exists so `workspace_write` and
        // `read_file` calls don't trip on a missing directory. Failure is
        // non-fatal — the tool surfaces the IO error on first use.
        if let Err(e) = workspace.ensure_dir() {
            warn!(
                error = %e,
                workspace_dir = %workspace.dir().display(),
                "Failed to create workspace directory — workspace tools may fail at runtime"
            );
        }

        self.workspace = Some(workspace);
        self
    }

    /// Compose the full extra-read-roots list to pass to the read-family
    /// fs_* tools at registration time.
    ///
    /// Combines, in order:
    /// 1. The caller-supplied prefix (typically the workspace parent dir for
    ///    sibling-workspace reads — #242 — when `with_workspace` is the
    ///    caller; empty otherwise).
    /// 2. The accumulated [`Self::extra_fs_read_roots`] entries pushed by
    ///    builder methods like [`Self::with_shell_spill`] and
    ///    [`Self::with_tool_output_truncate`].
    ///
    /// Centralising this in one place ensures every fs_* re-registration
    /// site sees the same set of read roots regardless of the order in
    /// which builder methods were called — closing the dead-code / footgun
    /// pattern Tim flagged on the #921 review.
    fn compose_fs_extra_read_roots(
        &self,
        prefix: &[std::path::PathBuf],
    ) -> Vec<std::path::PathBuf> {
        let mut out: Vec<std::path::PathBuf> =
            Vec::with_capacity(prefix.len() + self.extra_fs_read_roots.len());
        out.extend(prefix.iter().cloned());
        out.extend(self.extra_fs_read_roots.iter().cloned());
        out
    }

    /// Re-register the read-family fs_* tools (`fs_read`, `fs_list`,
    /// `fs_grep`, `fs_glob`) and the write-family (`fs_write`, `fs_edit`)
    /// using `primary` as the primary sandbox root and `extras` as the
    /// extra read roots.
    ///
    /// The write-family tools deliberately do NOT receive `extras` —
    /// the read roots are read-only by design (#242). When `primary`
    /// is `None`, the agent has no primary sandbox root (unrestricted)
    /// and we leave the existing fs_* registrations as-is so the unrestricted
    /// agent retains full filesystem access.
    fn register_fs_tools(
        &mut self,
        primary: Option<std::path::PathBuf>,
        extras: &[std::path::PathBuf],
    ) {
        let Some(root) = primary else {
            return;
        };

        let enabled = &self.config.enabled_tools;
        let tool_enabled = |name: &str| enabled.is_empty() || enabled.iter().any(|t| t == name);

        let cache = self.tools.file_state_cache().clone();
        let extras_vec = extras.to_vec();

        if tool_enabled("fs_read") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsReadTool::sandboxed(root.clone())
                    .with_cache(cache.clone())
                    .with_extra_read_roots(extras_vec.clone()),
            ));
        }
        if tool_enabled("fs_write") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsWriteTool::sandboxed(root.clone()).with_cache(cache.clone()),
            ));
        }
        if tool_enabled("fs_list") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsListTool::sandboxed(root.clone())
                    .with_extra_read_roots(extras_vec.clone()),
            ));
        }
        if tool_enabled("fs_edit") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsEditTool::sandboxed(root.clone())
                    .with_cache(cache.clone())
                    .with_fuzzy_match(self.config.fs_edit_fuzzy_match),
            ));
        }
        if tool_enabled("fs_grep") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsGrepTool::sandboxed(root.clone())
                    .with_extra_read_roots(extras_vec.clone()),
            ));
        }
        if tool_enabled("fs_glob") {
            self.tools.register(std::sync::Arc::new(
                alms_sandbox::FsGlobTool::sandboxed(root).with_extra_read_roots(extras_vec),
            ));
        }
    }

    /// Re-register read-family fs_* tools when no workspace is attached.
    ///
    /// Used by [`Self::with_shell_spill`] and
    /// [`Self::with_tool_output_truncate`] to widen the agent's read roots
    /// to include the per-run spill directory at the moment the policy
    /// becomes active. When `with_workspace` is *also* called later, it
    /// re-registers the fs_* tools again with the workspace as the primary
    /// root, picking up the same accumulated extras via
    /// [`Self::compose_fs_extra_read_roots`] — so the call order does not
    /// matter, and the workspace registration cannot silently drop the
    /// spill-dir extras (#921 review).
    fn refresh_fs_tools_for_extras(&mut self) {
        if let Some(root) = self.resolved_sandbox_root.clone() {
            let extras = self.compose_fs_extra_read_roots(&[]);
            self.register_fs_tools(Some(root), &extras);
        }
    }

    /// Attach a runtime event sender so the gateway can observe tool events.
    pub fn with_event_sender(mut self, sender: RuntimeEventSender) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// Attach a dedicated LLM client for in-loop sliding-summary generation
    /// (#866).
    ///
    /// When set, the in-loop summarizer (`maybe_summarize`) uses this client
    /// instead of `self.llm`. The gateway constructs this client by cloning
    /// the agent's resolved `LlmClient` and re-applying a different provider
    /// via `with_provider_and_secrets` when
    /// [`ContextConfig::summary_provider`](alms_core::config::ContextConfig)
    /// is set. Callers that do not set a separate summary provider should
    /// leave this `None` so the summarizer transparently inherits the
    /// agent's provider (pre-#866 behaviour).
    pub fn with_summary_llm(mut self, llm: LlmClient) -> Self {
        self.summary_llm = Some(llm);
        self
    }

    /// Set the run ID for audit event correlation.
    pub fn with_run_id(mut self, run_id: alms_core::RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// Attach a cancellation token for cooperative run cancellation.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Set default environment variables for shell processes.
    ///
    /// These are injected into every process spawned by the shell tool after
    /// `env_clear()`, so the spawned CLI commands can discover the gateway's
    /// data directory (`ALMS_DATA_DIR`) and workspace directory
    /// (`ALMS_WORKSPACE_DIR`) even when cwd is sandboxed elsewhere.
    ///
    /// Re-registers the shell tool immediately so that unnamed agents
    /// (which skip `with_workspace()`) still receive the environment variables.
    pub fn with_shell_default_env(
        mut self,
        env: std::collections::HashMap<String, String>,
    ) -> Self {
        self.shell_default_env = env;

        // Re-register shell tool with the new default env so unnamed agents
        // (which never call with_workspace()) still get ALMS_DATA_DIR injected.
        let enabled = &self.config.enabled_tools;
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled && (self.tools.contains("shell") || self.tools.contains("shell_exec")) {
            let shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_permissions(&self.shell_permissions)
            .with_classification_mode(self.shell_classification_mode)
            .with_spill_policy(self.shell_spill_policy.clone())
            .with_default_env(self.shell_default_env.clone());
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }

        self
    }

    /// Set the agent name for perspective mapping in DM sessions.
    ///
    /// When the context_id starts with `"dm:"`, the context builder uses this
    /// name to map messages: messages where `from_agent == agent_name` become
    /// `Role::Assistant` (the LLM's own previous responses), while messages from
    /// other agents stay as `Role::User`.
    pub fn with_agent_name(mut self, name: String) -> Self {
        self.agent_name = Some(name);
        self
    }

    /// Activate the shell-output spill policy for this runtime (issue #756).
    ///
    /// `run_dir` is the per-run directory
    /// (`{data_dir}/shell_output/{run_id}/`) where spill files will be
    /// written when shell output exceeds the truncation threshold. The
    /// directory is created lazily on first spill.
    ///
    /// Re-registers the shell tool immediately so the updated policy takes
    /// effect, and widens `fs_read`/`fs_list`/`fs_grep`/`fs_glob`'s
    /// sandbox so the agent can `fs_read` the spilled file without operators
    /// having to grant extra filesystem permissions.
    ///
    /// Must be called *after* `with_run_id()` — otherwise the caller cannot
    /// compute the per-run directory path. Passing `enabled == false`
    /// leaves the runtime in the default "spill disabled" state.
    pub fn with_shell_spill(mut self, run_dir: std::path::PathBuf, enabled: bool) -> Self {
        use alms_sandbox::shell::spill::ShellSpillPolicy;

        let policy = if enabled {
            ShellSpillPolicy::with_run_dir(run_dir.clone())
        } else {
            ShellSpillPolicy::disabled()
        };
        self.shell_spill_policy = policy;

        // Re-register the shell tool under both its primary name and its
        // legacy alias so the new policy is observed on the next tool call.
        let enabled_tools = &self.config.enabled_tools;
        let shell_enabled = enabled_tools.is_empty()
            || enabled_tools
                .iter()
                .any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled && (self.tools.contains("shell") || self.tools.contains("shell_exec")) {
            let mut shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_permissions(&self.shell_permissions)
            .with_classification_mode(self.shell_classification_mode)
            .with_spill_policy(self.shell_spill_policy.clone());
            if !self.shell_default_env.is_empty() {
                shell_tool = shell_tool.with_default_env(self.shell_default_env.clone());
            }
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }

        // Push the per-run shell-spill dir onto the accumulated
        // `extra_fs_read_roots` so `with_workspace`'s later fs_* registration
        // also picks it up, and re-register the read-family fs_* tools now
        // for the unnamed-agent path (where `with_workspace` is never
        // called). See [`Self::extra_fs_read_roots`] for the rationale.
        if enabled {
            self.extra_fs_read_roots.push(run_dir);
            self.refresh_fs_tools_for_extras();
        }

        self
    }

    /// Activate the shared in-loop tool-output truncation policy
    /// (issue #851).
    ///
    /// `run_dir` is the per-run directory
    /// (`{data_dir}/tool-output/{run_id}/`) where spill files will be
    /// written when any tool's result exceeds either the byte cap or the
    /// line cap (defaults defined in [`crate::tool_output_truncate`]). The
    /// directory is created lazily on first spill.
    ///
    /// Like [`Self::with_shell_spill`], this widens the agent's
    /// `fs_read`/`fs_list`/`fs_grep`/`fs_glob` extra read roots so the
    /// agent can `fs_read` a spilled file without operators having to grant
    /// extra filesystem permissions.
    ///
    /// Must be called *after* `with_run_id()` — otherwise the caller cannot
    /// compute the per-run directory path. Passing `enabled == false`
    /// leaves the runtime in the default "no truncation" state.
    pub fn with_tool_output_truncate(
        mut self,
        run_dir: std::path::PathBuf,
        enabled: bool,
        max_bytes: usize,
        max_lines: usize,
    ) -> Self {
        use crate::tool_output_truncate::ToolOutputTruncatePolicy;

        let policy = if enabled {
            let mut p = ToolOutputTruncatePolicy::with_run_dir(run_dir.clone());
            p.max_bytes = max_bytes;
            p.max_lines = max_lines;
            p
        } else {
            ToolOutputTruncatePolicy::disabled()
        };
        self.tool_output_truncate_policy = policy;

        // Push the per-run tool-output spill dir onto the accumulated
        // `extra_fs_read_roots` so `with_workspace`'s later fs_*
        // re-registration also picks it up, and re-register the read-family
        // fs_* tools now for the unnamed-agent path. The accumulator is the
        // single source of truth for all per-run spill dirs — see
        // [`Self::extra_fs_read_roots`] for the rationale (#921 review fix).
        if enabled {
            self.extra_fs_read_roots.push(run_dir);
            self.refresh_fs_tools_for_extras();
        }

        self
    }

    /// Re-register the shell tool with the current `shell_permissions`.
    ///
    /// Used at construction time to apply permissions to the initial shell
    /// tool registration. Later registrations (via `with_workspace()`,
    /// `with_shell_default_env()`) also apply permissions explicitly.
    fn apply_shell_permissions(&mut self) {
        let enabled = &self.config.enabled_tools;
        let shell_enabled =
            enabled.is_empty() || enabled.iter().any(|t| t == "shell" || t == "shell_exec");
        if shell_enabled && (self.tools.contains("shell") || self.tools.contains("shell_exec")) {
            let shell_tool = alms_sandbox::ShellTool::with_policy(
                self.resolved_sandbox_root.clone(),
                self.shell_unrestricted,
            )
            .with_permissions(&self.shell_permissions)
            .with_classification_mode(self.shell_classification_mode)
            .with_spill_policy(self.shell_spill_policy.clone());
            let tool_arc: std::sync::Arc<dyn alms_sandbox::Tool> = std::sync::Arc::new(shell_tool);
            self.tools.register(std::sync::Arc::clone(&tool_arc));
            self.tools
                .register_arc_as(alms_sandbox::shell::SHELL_TOOL_ALIAS, tool_arc);
        }
    }

    /// Emit a status event to the gateway layer (best-effort, never fails).
    pub(crate) fn emit_status(&self, phase: &str, detail: Option<&str>) {
        if let Some(ref tx) = self.event_sender {
            let _ = tx.send(crate::events::RuntimeEvent::Status {
                phase: phase.to_string(),
                detail: detail.map(|d| d.to_string()),
            });
        }
    }

    /// Emit a `ContextDebug` event containing the full assembled context
    /// window that is about to be sent to the LLM.
    ///
    /// Only called when `debug_mode` is enabled. Best-effort: failures are
    /// silently dropped since debug events are informational only.
    fn emit_context_debug(&self, messages: &[LlmMessage]) {
        use crate::context::{estimate_llm_message_tokens, estimate_tokens};

        let Some(ref tx) = self.event_sender else {
            return;
        };

        // Compute token estimates per message for the debug payload.
        let total_tokens: usize = messages
            .iter()
            .map(|m| estimate_llm_message_tokens(m) + 4)
            .sum();

        // System prompt tokens (first message is always the system prompt).
        let system_tokens = messages
            .first()
            .filter(|m| m.role == "system")
            .map(|m| estimate_tokens(m.content_str()))
            .unwrap_or(0);

        // History message count: everything except the first system message
        // and the last user message (which is the current input).
        let history_message_count = messages
            .len()
            .saturating_sub(1) // system prompt
            .saturating_sub(
                messages
                    .last()
                    .filter(|m| m.role == "user")
                    .map(|_| 1)
                    .unwrap_or(0),
            );

        let tool_names: Vec<String> = self
            .tools
            .to_definitions()
            .into_iter()
            .map(|td| td.function.name)
            .collect();

        // Serialize messages for the debug payload. Use serde_json::to_value
        // which handles the full LlmMessage structure including tool_calls.
        let messages_json = serde_json::to_value(messages).unwrap_or_default();

        let _ = tx.send(crate::events::RuntimeEvent::ContextDebug {
            messages: messages_json,
            tool_names,
            total_tokens,
            system_tokens,
            history_message_count,
        });
    }

    /// Run the agent on a single input
    #[instrument(
        level = "info",
        skip(self, session_manager, context_id, input),
        fields(
            agent_id = %self.agent_id.0,
            context_id = %context_id.as_ref(),
        )
    )]
    pub async fn run(
        &self,
        session_manager: &SessionManager,
        context_id: impl AsRef<str>,
        input: impl Into<String>,
    ) -> AlmsResult<RunOutput> {
        let context_id = context_id.as_ref();
        let input = input.into();
        let span = Span::current();

        info!(
            target: "agent::run_start",
            agent_id = %self.agent_id.0,
            context_id = %context_id,
            input_len = input.len(),
            "Agent run started"
        );

        span.record("input_len", input.len());

        let session = session_manager.get_or_create(self.agent_id, context_id);

        // Build context first (reads history without current input to avoid double-counting),
        // then persist the user message so it survives agent loop failures.
        self.emit_status(PHASE_BUILDING_CONTEXT, None);
        let history = self
            .build_context(session_manager, &session.id, context_id, &input)
            .await;

        let user_msg = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: SessionRole::User,
            content: SessionContent::Text(input.clone()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        };
        session_manager.append_message(session.id, user_msg)?;

        self.finish_run(session_manager, session.id, context_id, history)
            .await
    }

    /// Run the agent on a pre-existing shared session (e.g. a DM session).
    ///
    /// Unlike `run()`, this method:
    /// - Looks up the session by `SessionId` directly instead of creating one
    ///   via `get_or_create(agent_id, context_id)`. This ensures the agent uses
    ///   the shared DM session created by `MessageBus`, not a new empty one.
    /// - Skips persisting the input message because the `MessageBus` already
    ///   wrote it to the session with `from_agent` metadata.
    #[instrument(
        level = "info",
        skip(self, session_manager, input),
        fields(
            agent_id = %self.agent_id.0,
            session_id = %session_id.0,
            context_id = %context_id,
        )
    )]
    pub async fn run_on_session(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        context_id: &str,
        input: &str,
    ) -> AlmsResult<RunOutput> {
        info!(
            target: "agent::run_on_session_start",
            agent_id = %self.agent_id.0,
            session_id = %session_id.0,
            context_id = %context_id,
            input_len = input.len(),
            "Agent run_on_session started (shared session, input already persisted)"
        );

        // Verify the session exists -- fail loudly if not.
        session_manager.get(session_id)?;

        // Build context: the input message is already in the session history
        // (written by MessageBus), so we pass an empty string as the current
        // input to avoid duplicating it in the context window.
        self.emit_status(PHASE_BUILDING_CONTEXT, None);
        let history = self
            .build_context(session_manager, &session_id, context_id, "")
            .await;

        // Do NOT persist the input message -- it is already in the session.

        self.finish_run(session_manager, session_id, context_id, history)
            .await
    }

    /// Shared tail for `run()` and `run_on_session()`: executes the agent loop
    /// and persists the result (assistant response, cancellation marker, or
    /// error marker) to the session.
    ///
    /// Tool call records are always returned regardless of success or failure,
    /// so the gateway can persist partial execution history for debugging.
    async fn finish_run(
        &self,
        session_manager: &SessionManager,
        session_id: alms_core::SessionId,
        context_id: &str,
        history: AlmsResult<Vec<LlmMessage>>,
    ) -> AlmsResult<RunOutput> {
        let is_dm = context_id.starts_with("dm:");
        let include_user = Self::is_user_facing_context(context_id);
        let dm_peer = if is_dm {
            self.dm_peer_name(context_id)
        } else {
            None
        };

        let (tool_calls, result) = match history {
            Ok(h) => {
                // Emit context debug snapshot when debug_mode is enabled.
                // This happens after build_context() and before agent_loop()
                // so the UI sees exactly what the LLM will receive on the
                // first iteration.
                if self.config.debug_mode {
                    self.emit_context_debug(&h);
                }

                self.agent_loop(
                    session_manager,
                    session_id,
                    h,
                    is_dm,
                    include_user,
                    dm_peer.as_deref(),
                )
                .await
            }
            Err(e) => (Vec::new(), Err(e)),
        };

        match result {
            Ok(loop_output) => {
                let crate::agent::loop_impl::AgentLoopOutput {
                    response,
                    usage,
                    reasoning,
                } = loop_output;

                // Skip persisting an assistant message when the response is
                // empty. This happens when the agent used `ignore_message` to
                // decline responding — there is nothing to record.
                //
                // For DM sessions: persist the final text response as
                // reasoning (Role::User with message_type="reasoning") so
                // the UI can display it in a collapsible reasoning block
                // after page reload.
                //
                // However, when the agent loop executed tool calls, the
                // thinking text was already persisted by
                // `persist_assistant_tool_calls` as part of the tool call
                // batch.  Persisting it again here would produce duplicate
                // reasoning text entries that `groupDmReasoningBlocks()`
                // concatenates, resulting in doubled text on page reload.
                // Skip persistence when tool calls were present.  (Fixes #687)
                let dm_text_already_persisted = is_dm && !tool_calls.is_empty();
                if !response.is_empty() && !dm_text_already_persisted {
                    let base_meta = if is_dm {
                        self.dm_reasoning_metadata(is_dm)
                    } else {
                        None
                    };
                    let role = if is_dm && base_meta.is_some() {
                        SessionRole::User
                    } else {
                        SessionRole::Assistant
                    };
                    // Attach the extended-thinking trace (if any) as
                    // `reasoning_blocks` metadata on the final assistant
                    // message, merged with any existing DM metadata.
                    let metadata = crate::agent::loop_impl::merge_reasoning_blocks(
                        base_meta,
                        reasoning.as_deref(),
                    );
                    let reply_msg = SessionMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        role,
                        content: SessionContent::Text(response.clone()),
                        timestamp: alms_core::Timestamp::now(),
                        metadata,
                    };
                    session_manager.append_message(session_id, reply_msg)?;
                }

                info!(
                    "Agent {} completed for context {} (prompt={} completion={} tokens)",
                    self.agent_id.0, context_id, usage.prompt_tokens, usage.completion_tokens
                );

                Ok(RunOutput {
                    response,
                    usage,
                    tool_calls,
                })
            }
            Err(AlmsError::Cancelled) => {
                // In DM sessions, attach from_agent metadata and use Role::User
                // so read_messages perspective mapping works consistently — all
                // messages in a shared DM session must be Role::User.
                let role = if is_dm && self.agent_name.is_some() {
                    SessionRole::User
                } else {
                    SessionRole::Assistant
                };
                let cancel_msg = SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::Text("[Run cancelled by user]".to_string()),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: self.dm_marker_metadata(is_dm),
                };
                if let Err(append_err) = session_manager.append_message(session_id, cancel_msg) {
                    warn!(
                        "Failed to persist cancellation marker to session: {}",
                        append_err
                    );
                }
                Err(AlmsError::CancelledWithToolCalls { tool_calls })
            }
            Err(e) => {
                // Write a sanitized error marker so the session reflects the failed attempt
                // without leaking sensitive details (API keys, URLs) into LLM context.
                // In DM sessions, use Role::User with from_agent metadata so
                // perspective mapping works — all messages in shared DM sessions
                // must be Role::User.
                let safe_reason = sanitize_error_for_session(&e);
                let role = if is_dm && self.agent_name.is_some() {
                    SessionRole::User
                } else {
                    SessionRole::Assistant
                };
                let error_msg = SessionMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: SessionContent::Text(format!("[Run failed: {}]", safe_reason)),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: self.dm_marker_metadata(is_dm),
                };
                if let Err(append_err) = session_manager.append_message(session_id, error_msg) {
                    warn!("Failed to persist error marker to session: {}", append_err);
                }

                Err(AlmsError::FailedWithToolCalls {
                    source: Box::new(e),
                    tool_calls,
                })
            }
        }
    }

    /// Get tool registry reference
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}
