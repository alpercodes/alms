use super::{AgentConfig, AgentRuntime};
use crate::events::RuntimeEventSender;
use crate::llm_client::LlmClient;
use crate::tools::ToolRegistry;
use crate::workspace::AgentWorkspace;
use crate::workspace_tool::{WorkspaceReadTool, WorkspaceWriteTool};
use alms_core::{AgentId, AlmsError, AlmsResult};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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
            dm_implicit_reply: false,
        };

        // Apply shell permissions / classification to the initially registered
        // shell tool. The shell tool is re-registered by
        // with_shell_default_env(), with_shell_spill(), with_project_root()
        // and with_unrestricted_filesystem(), but we also need it on the
        // initial tool so agents that call none of those get the policy.
        // (This used to name with_workspace() as one of the re-registering
        // builders. It has not registered a shell tool since #945 — see
        // refresh_fs_tools_for_extras for the same correction.)
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
    /// `with_workspace` sees when it (post-#945) registers
    /// `workspace_write` / `workspace_read` only — `with_workspace` no
    /// longer touches fs_*/shell registration, but the ordering invariant
    /// still holds for forward-compatibility. `with_shell_default_env` is
    /// the exception: it is called BEFORE `with_unrestricted_filesystem`
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
        // it (`with_extra_fs_read_root`, or a `with_shell_spill` /
        // `with_tool_output_truncate` that has not run yet, all of which
        // reach `register_fs_tools` via `refresh_fs_tools_for_extras`) do
        // not re-introduce a sandboxed registration. `with_workspace` is
        // NOT one of them — it has registered no fs_* tool since #945.
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
    /// Registers the `workspace_write` and `workspace_read` tools so the
    /// agent can update and re-read its `personality.md` / `goals.md` /
    /// `memories.md` / `user.md` during runs. Ensures the workspace directory
    /// exists.
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
    /// **Note**: both registrations go through `ToolRegistry::register()`,
    /// which checks the `enabled_filter`. If the operator has set `tools.enabled`
    /// and a tool is not in the list, it will be silently skipped.
    /// This is intentional — the operator's allowlist should be the single
    /// source of truth for which tools are available.
    ///
    /// The two are registered together but filtered separately, so an
    /// allowlist naming `workspace_write` and not `workspace_read` is
    /// possible. `workspace_write`'s refusal message (#1310) is written for
    /// that case: its first recovery is `mode: "append"`, which is the same
    /// tool the agent already has, and `workspace_read` is only needed for a
    /// deliberate whole-file rewrite.
    pub fn with_workspace(mut self, workspace: AgentWorkspace) -> Self {
        // Subject to enabled_filter — see doc comment above.
        self.tools
            .register(std::sync::Arc::new(WorkspaceWriteTool::new(
                workspace.clone(),
            )));
        self.tools
            .register(std::sync::Arc::new(WorkspaceReadTool::new(
                workspace.clone(),
            )));

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
    /// 1. The caller-supplied prefix. Always empty today: its one
    ///    caller was `with_workspace`, passing the workspace parent dir
    ///    for sibling-workspace reads (#242), and #945 removed both that
    ///    shim and `with_workspace`'s fs_* registration — the agent's
    ///    metadata now lives under the project root, so a sibling read
    ///    resolves against the primary root directly. The parameter is
    ///    kept because `register_fs_tools` still threads it.
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

    /// Re-register the read-family fs_* tools against the current
    /// primary sandbox root.
    ///
    /// Used by [`Self::with_shell_spill`],
    /// [`Self::with_tool_output_truncate`] and
    /// [`Self::with_extra_fs_read_root`] to widen the agent's read roots
    /// the moment a new per-run directory becomes relevant, rather than
    /// waiting for a later builder to do it.
    ///
    /// A no-op until a primary root exists. [`Self::with_project_root`]
    /// consults the same accumulator via
    /// [`Self::compose_fs_extra_read_roots`] when it runs, so an extra
    /// pushed before the root is pinned is picked up then — the call
    /// order does not matter either way (#921 review).
    ///
    /// **Corrected in #1260.** This used to say that `with_workspace`
    /// re-registers the fs_* tools again "with the workspace as the
    /// primary root". True pre-#945, and not since: #945 collapsed the
    /// two-root model and `with_workspace` now registers only
    /// `workspace_write` / `workspace_read`, touching no sandbox path
    /// (its own doc comment says so). Worth naming rather than quietly
    /// deleting — the stale claim predicts one extra fs_* pass per run,
    /// and that is exactly the rival model the registration-count
    /// enumeration in #1260 had to rule out.
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

    /// Attach a dedicated LLM client for in-loop compact-strategy summary
    /// generation (#866; strategy renamed from `sliding-summary` in #869).
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

    /// Enable implicit DM reply mode (#1154).
    ///
    /// Set by the gateway for peer-triggered DM runs only. See the field
    /// docs on [`Self::dm_implicit_reply`] for the behavioural contract:
    /// the run's final assistant text is delivered to the DM peer by the
    /// gateway's completion gate, the runtime arms the bounded empty-reply
    /// nudge, and `finish_run` leaves the final-text persistence to
    /// `MessageBus::send`.
    pub fn with_dm_implicit_reply(mut self) -> Self {
        self.dm_implicit_reply = true;
        self
    }

    /// Append an extra read-only root onto the accumulated
    /// [`Self::extra_fs_read_roots`] list (#946 sibling-reads
    /// support).
    ///
    /// Used by the gateway's run-lifecycle wiring when an agent runs
    /// in `WorktreeMode::Git`: the agent's primary sandbox root is
    /// the worktree path (`<project>/.alms/worktrees/<name>/`) but
    /// the agent must still be able to read sibling personality
    /// metadata at `<project>/.alms/agents/<sibling>/personality.md`,
    /// which sits OUTSIDE the worktree.
    ///
    /// Order-independent: this builder pushes onto the
    /// [`Self::extra_fs_read_roots`] accumulator and then calls
    /// [`Self::refresh_fs_tools_for_extras`], so the new extra takes
    /// effect immediately whether or not [`Self::with_project_root`]
    /// has already run. When the primary root is set later, its
    /// re-registration consults the same accumulator via
    /// [`Self::compose_fs_extra_read_roots`] and picks the extra up
    /// then. Mirrors the accumulator pattern used by
    /// [`Self::with_shell_spill`] and [`Self::with_tool_output_truncate`].
    pub fn with_extra_fs_read_root(mut self, root: std::path::PathBuf) -> Self {
        // Skip duplicate-root pushes — the accumulator is consulted
        // by every read-family fs_* re-registration so a duplicate
        // would inflate the list without changing behaviour. The
        // O(n) `contains` check is fine; the list is short by design
        // (a handful of per-run spill dirs + the worktree sibling
        // root).
        if !self.extra_fs_read_roots.iter().any(|p| p == &root) {
            self.extra_fs_read_roots.push(root);
        }
        // If the runtime already has a primary sandbox root pinned,
        // re-register the read-family tools so the new extra takes
        // effect immediately. Without this the tools registered at
        // `AgentRuntime::new` time keep their stale extra-roots list.
        // Same shape as `refresh_fs_tools_for_extras`'s callers.
        self.refresh_fs_tools_for_extras();
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
        // `extra_fs_read_roots` so a later fs_* registration —
        // `with_project_root`, or another builder's
        // `refresh_fs_tools_for_extras` — picks it up, and re-register the
        // read-family fs_* tools now so the root is live even if no such
        // builder follows. (This used to say `with_workspace` performs
        // that later registration; it has not since #945.) See
        // [`Self::extra_fs_read_roots`] for the rationale.
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
        // `extra_fs_read_roots` so a later fs_* registration picks it up,
        // and re-register the read-family fs_* tools now so the root is
        // live even if none follows. (Same correction as `with_shell_spill`
        // above: the later registration is `with_project_root` or another
        // `refresh_fs_tools_for_extras`, never `with_workspace`.) The
        // accumulator is the single source of truth for all per-run spill
        // dirs — see [`Self::extra_fs_read_roots`] (#921 review fix).
        //
        // Note this builder registers no shell tool, unlike
        // `with_shell_spill` above — its only registration effect is the
        // `refresh_fs_tools_for_extras` below.
        if enabled {
            self.extra_fs_read_roots.push(run_dir);
            self.refresh_fs_tools_for_extras();
        }

        self
    }

    /// Re-register the shell tool with the current `shell_permissions`.
    ///
    /// Used at construction time to apply permissions to the initial
    /// shell tool registration — it is the second of the five shell
    /// registrations a normal run performs, and the only one that is not
    /// a `with_*` builder. The others (`with_shell_default_env`,
    /// `with_shell_spill`, and whichever of `with_project_root` /
    /// `with_unrestricted_filesystem` the run takes) apply permissions
    /// explicitly too. `with_workspace` is not among them: it has
    /// registered no shell tool since #945.
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
}
