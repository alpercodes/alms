#[cfg(test)]
use crate::NativeTool;
use crate::{SandboxError, Tool, ToolContext, ToolIdentity, error::SandboxResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// The phrase both name-collision warnings share (#1260).
///
/// Exported so that log-asserting tests in other crates pin the string
/// the registry actually emits instead of a copy of it. The negative
/// assertion ("a normal run logs no re-registration warning") is only
/// worth anything while some positive assertion proves the same string
/// is still live, and both need to be looking for the same text.
pub const TOOL_COLLISION_WARNING: &str = "already registered by a different implementation";

/// Whether `existing` and `incoming` are the same tool implementation.
///
/// This is the discriminator behind [`TOOL_COLLISION_WARNING`] (#1260).
/// Two tools are "the same" when they are the same concrete type **and**
/// report the same canonical [`Tool::name`]:
///
/// * **Same type** is what makes the per-run lifecycle silent. Every
///   re-registration a run performs is the same type rebuilt with
///   different constructor arguments — `FsReadTool::new()` replaced by
///   `FsReadTool::sandboxed(root).with_cache(..)`, `ShellTool` replaced
///   by a `ShellTool` that also knows the run's spill directory. Those
///   are configuration changes, not collisions, and each one used to
///   cost a WARN.
/// * **Same canonical name** only ever differs on the alias path, where
///   the map key is not `tool.name()`. Re-registering `ShellTool` under
///   `"shell_exec"` is benign precisely because the alias still resolves
///   to a tool that calls itself `"shell"`; pointing `"shell_exec"` at
///   something else is not.
///
/// # What this deliberately does not claim
///
/// It is an approximation of "a different tool", not a proof of one, and
/// the approximation is exact in one direction only: a type change is
/// always reported, a same-type change never is. The blind spot is a
/// type that can carry more than one identity, and there are exactly two
/// in the tree. 21 of the 22 non-`cfg(test)` `impl Tool` blocks return a
/// hard-coded `&'static str` from `name()`, so across the shipped set
/// the type → name mapping is a bijection and "same type" and "same
/// tool" coincide. The exceptions are [`crate::NativeTool`], which takes
/// its name as a constructor argument, and `BlockingTestTool` in
/// `alms-runtime`, which is `#[cfg(test)]`.
///
/// **`NativeTool`'s exemption is a call-graph invariant, not a
/// structural one.** Its only caller in this tree is the `#[cfg(test)]`
/// `register_native` helper below — but `NativeTool::new` is `pub` and
/// un-gated, so nothing stops a future in-tree caller, or a downstream
/// crate, from registering two differently-named `NativeTool`s and
/// landing in the blind spot without any compiler complaint. The name
/// comparison is what bounds the damage when that happens: it catches
/// the sub-case where the two disagree on their canonical name, which is
/// the whole of the alias path and part of the primary one.
pub fn same_implementation(existing: &dyn Tool, incoming: &dyn Tool) -> bool {
    existing.impl_type_id() == incoming.impl_type_id() && existing.name() == incoming.name()
}

/// Tool registry for managing available tools
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    /// Registered tools: name -> tool
    tools: Arc<DashMap<String, Arc<dyn Tool>>>,
    /// When non-empty, only tools whose name appears in this list can be
    /// registered. This filter applies to **all** `register()` calls —
    /// both initial builtins and dynamically added tools (invoke_agent,
    /// send_message, workspace_write, etc.). An empty vec means all tools
    /// are allowed.
    enabled_filter: Arc<Vec<String>>,
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new() -> Self {
        Self {
            tools: Arc::new(DashMap::new()),
            enabled_filter: Arc::new(Vec::new()),
        }
    }

    /// Register a tool.
    ///
    /// When `enabled_filter` is non-empty, only tools whose name appears in
    /// the list are accepted. This prevents dynamically registered tools
    /// (invoke_agent, send_message, workspace_write, etc.) from bypassing the
    /// operator's `tools.enabled` configuration.
    pub fn register(&self, tool: Arc<dyn Tool>) -> SandboxResult<()> {
        let name = tool.name().to_string();

        // Enforce the enabled filter for ALL registrations — builtins and
        // dynamically added tools alike.
        if !self.enabled_filter.is_empty() && !self.enabled_filter.iter().any(|e| e == &name) {
            debug!(
                "Skipping registration of tool '{}' — not in enabled_filter",
                name
            );
            return Ok(());
        }

        // Re-registration is the normal per-run lifecycle, not an
        // anomaly: a single run rebuilds the registry once and then
        // replaces `shell` and the `fs_*` family several times over as
        // the runtime's builder chain narrows the sandbox root, wires
        // the file-state cache and adds the run's spill directories.
        // Warning on that fired ~29 times per run and trained everyone
        // to skip WARN entirely (#1260). Only a change of implementation
        // behind an existing name is worth saying anything about.
        //
        // The `Ref` guard `get()` hands back is confined to this block:
        // it read-locks the shard `insert()` below needs to write-lock,
        // so it must be dropped before the insert, not merely before the
        // end of the function.
        let collision = {
            self.tools.get(&name).and_then(|existing| {
                let existing = existing.value();
                (!same_implementation(existing.as_ref(), tool.as_ref()))
                    .then(|| (existing.impl_type_name(), existing.name().to_string()))
            })
        };
        if let Some((previous_impl, previous_name)) = collision {
            warn!(
                tool = %name,
                previous_impl,
                previous_name,
                new_impl = tool.impl_type_name(),
                "Tool '{}' is {} — replacing",
                name,
                TOOL_COLLISION_WARNING,
            );
        }

        debug!("Registering tool: {}", name);
        self.tools.insert(name.clone(), tool);

        Ok(())
    }

    /// Register a tool under a specific name (alias).
    ///
    /// Unlike `register()`, this allows inserting a tool under a different
    /// name than `tool.name()`. Used for backward-compatible aliases
    /// (e.g. registering `ShellTool` under the legacy `"shell_exec"` name).
    ///
    /// The enabled filter checks the provided `alias` name, not `tool.name()`.
    pub fn register_as(&self, alias: &str, tool: Arc<dyn Tool>) -> SandboxResult<()> {
        if !self.enabled_filter.is_empty()
            && !self.enabled_filter.iter().any(|e| e == alias)
            // Also accept the tool's canonical name in the filter
            && !self
                .enabled_filter
                .iter()
                .any(|e| e == tool.name())
        {
            debug!(
                "Skipping alias registration of tool '{}' as '{}' — not in enabled_filter",
                tool.name(),
                alias
            );
            return Ok(());
        }

        // Same rule as `register()` (#1260), and on this path the
        // canonical-name half of `same_implementation` is the half that
        // does the work: re-pointing `"shell_exec"` at another
        // `ShellTool` is the per-run lifecycle, while re-pointing it at
        // a tool that calls itself something else silently changes what
        // an established alias means. See the `collision` block in
        // `register()` for why the `Ref` guard is scoped.
        let collision = {
            self.tools.get(alias).and_then(|existing| {
                let existing = existing.value();
                (!same_implementation(existing.as_ref(), tool.as_ref()))
                    .then(|| (existing.impl_type_name(), existing.name().to_string()))
            })
        };
        if let Some((previous_impl, previous_name)) = collision {
            warn!(
                alias = %alias,
                previous_impl,
                previous_name,
                new_impl = tool.impl_type_name(),
                new_name = tool.name(),
                "Tool alias '{}' is {} — repointing",
                alias,
                TOOL_COLLISION_WARNING,
            );
        }

        debug!("Registering tool alias: '{}' -> '{}'", alias, tool.name());
        self.tools.insert(alias.to_string(), tool);
        Ok(())
    }

    /// Lookup a tool by name
    pub fn lookup(&self, name: &str) -> SandboxResult<Arc<dyn Tool>> {
        self.tools
            .get(name)
            .map(|t| Arc::clone(&*t))
            .ok_or_else(|| SandboxError::ToolNotFound(name.to_string()))
    }

    /// Check if a tool is registered
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// List all registered tool names
    pub fn list(&self) -> Vec<String> {
        self.tools.iter().map(|e| e.key().clone()).collect()
    }

    /// Create a registry with all built-in tools registered (unrestricted).
    pub fn with_builtin_tools() -> Self {
        Self::with_builtin_tools_sandboxed(None, false, &[])
    }

    /// Create a registry with built-in tools registered and optional sandbox.
    ///
    /// `sandbox_root` — when `Some`, fs tools and shell cwd are restricted to
    /// this directory (canonicalized). When `None`, no path restriction.
    /// `shell_unrestricted` — when `true`, shell_exec ignores sandbox_root for cwd.
    /// `enabled` — when non-empty, only builtins whose name appears in the
    /// list are registered. Empty slice = all builtins enabled.
    pub fn with_builtin_tools_sandboxed(
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
    ) -> Self {
        let mut registry = Self::new();
        // Store the enabled filter so that subsequent register() calls
        // (dynamic tools like invoke_agent, send_message, etc.) are also
        // subject to the operator's allowlist.
        registry.enabled_filter = Arc::new(enabled.to_vec());
        registry.register_builtin_tools_sandboxed(sandbox_root, shell_unrestricted, enabled);
        registry
    }

    /// Register built-in tools with optional sandbox configuration.
    ///
    /// When `enabled` is non-empty, only tools whose name appears in the list
    /// are registered. When empty, all builtins are registered.
    ///
    /// The shell tool is registered under `"shell"` (primary) and `"shell_exec"`
    /// (backward-compatible alias). The enabled filter accepts either name.
    pub fn register_builtin_tools_sandboxed(
        &self,
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
    ) {
        use crate::builtin::{
            DatetimeTool, EchoTool, FsEditTool, FsGlobTool, FsGrepTool, FsListTool, FsReadTool,
            FsWriteTool, HttpGetTool, MathTool,
        };
        use crate::shell::{SHELL_TOOL_ALIAS, ShellTool};

        // Build all tools, then filter by the enabled list.
        let shell_tool: Arc<dyn Tool> = Arc::new(ShellTool::with_policy(
            sandbox_root.clone(),
            shell_unrestricted,
        ));
        let (fs_read, fs_write, fs_list, fs_edit, fs_grep, fs_glob) = match sandbox_root {
            Some(ref root) => (
                FsReadTool::sandboxed(root.clone()),
                FsWriteTool::sandboxed(root.clone()),
                FsListTool::sandboxed(root.clone()),
                FsEditTool::sandboxed(root.clone()),
                FsGrepTool::sandboxed(root.clone()),
                FsGlobTool::sandboxed(root.clone()),
            ),
            None => (
                FsReadTool::new(),
                FsWriteTool::new(),
                FsListTool::new(),
                FsEditTool::new(),
                FsGrepTool::new(),
                FsGlobTool::new(),
            ),
        };

        let all_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(EchoTool::new()),
            Arc::new(DatetimeTool::new()),
            Arc::new(MathTool::new()),
            Arc::new(HttpGetTool::new()),
            Arc::clone(&shell_tool),
            Arc::new(fs_read),
            Arc::new(fs_write),
            Arc::new(fs_list),
            Arc::new(fs_edit),
            Arc::new(fs_grep),
            Arc::new(fs_glob),
        ];

        let builtin_names: Vec<String> = all_tools.iter().map(|t| t.name().to_string()).collect();

        // Let register() handle the enabled_filter check uniformly for all
        // tools — builtins and dynamically added tools alike.
        for tool in all_tools {
            if let Err(e) = self.register(tool) {
                error!("Failed to register builtin tool: {}", e);
            }
        }

        // Register the shell tool under the legacy alias "shell_exec" for
        // backward compatibility.
        //
        // This used to `insert()` straight into the map behind an
        // open-coded copy of the filter check. `register_as` applies the
        // identical rule — it accepts the alias when the filter is empty,
        // names the alias, or names `tool.name()`, which here is
        // `SHELL_TOOL_NAME` — so routing through it is behaviour-preserving
        // and closes the one insert path that had no collision check at
        // all (#1260).
        if let Err(e) = self.register_as(SHELL_TOOL_ALIAS, shell_tool) {
            error!("Failed to register shell tool alias: {}", e);
        }

        // Warn about enabled entries that don't match any builtin tool name,
        // but only if they are also not a known dynamic tool name.  Dynamic
        // tools (invoke_agent, send_message, workspace_write, etc.) are
        // registered later and are legitimately controlled by the same
        // enabled_filter.
        const KNOWN_DYNAMIC_TOOLS: &[&str] = &[
            "invoke_agent",
            "read_subagent_session",
            "send_message",
            "workspace_write",
            "workspace_read",
            "list_agents",
            "list_my_sessions",
            "read_messages",
            "read_session",
            "ignore_message",
        ];
        for name in enabled {
            if !builtin_names.iter().any(|b| b == name)
                && !KNOWN_DYNAMIC_TOOLS.iter().any(|d| d == name)
                && name != SHELL_TOOL_ALIAS
            {
                warn!(
                    "tools.enabled contains unknown tool '{}' — not a builtin or known dynamic tool; typo?",
                    name
                );
            }
        }

        info!("Registered built-in tools (filter: {:?})", enabled);
    }

    /// Execute a tool by name
    pub async fn execute(&self, name: &str, params: Value) -> SandboxResult<Value> {
        let tool = self.lookup(name)?;
        tool.execute(params).await
    }

    /// Execute a tool by name with per-call context (#1105).
    ///
    /// Used by the agent runtime to thread the parent's `invocation_id`
    /// into `InvokeAgentTool` so the coordinator can carry it on the
    /// `subagent_started` SSE event back to the parent's stream. All
    /// other tools fall through `Tool::execute_with_context`'s default
    /// impl, which discards `ctx` and calls plain `execute(params)`.
    pub async fn execute_with_context(
        &self,
        name: &str,
        params: Value,
        ctx: ToolContext,
    ) -> SandboxResult<Value> {
        let tool = self.lookup(name)?;
        tool.execute_with_context(params, ctx).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ToolRegistry {
    /// Register a native tool (test-only).
    pub fn register_native<F>(&self, name: impl Into<String>, handler: F) -> SandboxResult<()>
    where
        F: Fn(Value) -> SandboxResult<Value> + Send + Sync + 'static,
    {
        let tool = NativeTool::new(name, handler);
        self.register(Arc::new(tool))
    }

    /// Unregister a tool (test-only).
    pub fn unregister(&self, name: &str) -> SandboxResult<()> {
        if self.tools.remove(name).is_some() {
            info!("Unregistered tool: {}", name);
            Ok(())
        } else {
            Err(SandboxError::ToolNotFound(name.to_string()))
        }
    }

    /// List built-in tools (test-only).
    pub fn list_builtin(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|e| e.value().is_builtin())
            .map(|e| e.key().clone())
            .collect()
    }

    /// Get the number of registered tools (test-only).
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Check if registry is empty (test-only).
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Clear all tools (test-only).
    pub fn clear(&self) {
        self.tools.clear();
        info!("Cleared all tools from registry");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let registry = ToolRegistry::new();

        registry
            .register_native("double", |params| {
                let n = params.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(Value::from(n * 2))
            })
            .unwrap();

        assert!(registry.contains("double"));
        assert!(!registry.contains("triple"));

        let tool = registry.lookup("double").unwrap();
        assert_eq!(tool.name(), "double");
    }

    #[test]
    fn test_tool_not_found() {
        let registry = ToolRegistry::new();

        match registry.lookup("nonexistent") {
            Err(SandboxError::ToolNotFound(name)) => {
                assert_eq!(name, "nonexistent");
            }
            _ => panic!("Expected ToolNotFound error"),
        }
    }

    #[test]
    fn test_list_tools() {
        let registry = ToolRegistry::with_builtin_tools();

        let tools = registry.list();
        assert!(tools.contains(&"echo".to_string()));
        assert!(tools.contains(&"math".to_string()));
        assert!(tools.contains(&"http_get".to_string()));
    }

    #[test]
    fn test_unregister() {
        let registry = ToolRegistry::new();

        registry
            .register_native("test", |_| Ok(Value::Null))
            .unwrap();
        assert!(registry.contains("test"));

        registry.unregister("test").unwrap();
        assert!(!registry.contains("test"));
    }

    #[tokio::test]
    async fn test_execute() {
        let registry = ToolRegistry::new();

        registry
            .register_native("add", |params| {
                let a = params.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                let b = params.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                Ok(Value::from(a + b))
            })
            .unwrap();

        let result = registry
            .execute("add", serde_json::json!({"a": 10, "b": 32}))
            .await
            .unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_enabled_filter_restricts_builtins() {
        let enabled = vec!["echo".to_string(), "math".to_string()];
        let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &enabled);
        assert!(registry.contains("echo"));
        assert!(registry.contains("math"));
        assert!(!registry.contains("http_get"));
        assert!(!registry.contains("shell")); // primary name blocked
        assert!(!registry.contains("shell_exec")); // alias also blocked
        assert!(!registry.contains("fs_read"));
        assert!(!registry.contains("fs_write"));
        assert!(!registry.contains("fs_list"));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_empty_enabled_registers_all_builtins() {
        let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &[]);
        assert!(registry.contains("echo"));
        assert!(registry.contains("datetime"));
        assert!(registry.contains("math"));
        assert!(registry.contains("http_get"));
        assert!(registry.contains("shell")); // primary name
        assert!(registry.contains("shell_exec")); // backward-compat alias
        assert!(registry.contains("fs_read"));
        assert!(registry.contains("fs_write"));
        assert!(registry.contains("fs_list"));
        assert!(registry.contains("fs_edit"));
        assert!(registry.contains("fs_grep"));
        assert!(registry.contains("fs_glob"));
        // 11 builtins + 1 alias (shell_exec -> shell) = 12 entries
        assert_eq!(registry.len(), 12);
    }

    #[test]
    fn test_enabled_filter_blocks_dynamic_tools() {
        // When enabled_filter is set, dynamically registered tools that are
        // not in the list should be silently skipped (issue #287).
        let enabled = vec!["echo".to_string(), "math".to_string()];
        let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &enabled);
        assert_eq!(registry.len(), 2);

        // Attempt to register a dynamic tool not in the allowlist.
        registry
            .register_native("invoke_agent", |_| Ok(Value::Null))
            .unwrap();
        assert!(
            !registry.contains("invoke_agent"),
            "invoke_agent should be blocked by enabled_filter"
        );
        assert_eq!(registry.len(), 2);

        // A tool that IS in the allowlist should be accepted (re-registration).
        registry
            .register_native("echo", |_| Ok(Value::Null))
            .unwrap();
        assert!(registry.contains("echo"));
    }

    #[test]
    fn test_empty_enabled_filter_allows_dynamic_tools() {
        // When enabled_filter is empty, all dynamic tools should be accepted.
        let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &[]);

        registry
            .register_native("invoke_agent", |_| Ok(Value::Null))
            .unwrap();
        assert!(registry.contains("invoke_agent"));
        assert_eq!(registry.len(), 13); // 11 builtins + 1 alias + 1 dynamic
    }

    #[test]
    fn test_auto_approved_tools_via_registry() {
        let registry = ToolRegistry::with_builtin_tools();
        // echo and datetime are auto-approved
        let echo = registry.lookup("echo").unwrap();
        assert!(echo.is_auto_approved(), "echo should be auto-approved");
        let datetime = registry.lookup("datetime").unwrap();
        assert!(
            datetime.is_auto_approved(),
            "datetime should be auto-approved"
        );
        // shell is NOT auto-approved
        let shell = registry.lookup("shell").unwrap();
        assert!(
            !shell.is_auto_approved(),
            "shell should NOT be auto-approved"
        );
    }

    /// #1260 — the discriminator that decides whether a replacement is
    /// worth a WARN.
    ///
    /// These rows pin [`same_implementation`] itself. That the registry
    /// *emits* the warning from the branch this function selects is
    /// pinned end to end, over the real per-run builder chain and with a
    /// `tracing` capture, in `alms-gateway`'s
    /// `runs::lifecycle::tests::tool_registration_logging` — `alms-sandbox`
    /// has no log-capture harness and adding a third copy of the #1221 one
    /// is exactly what #1282 exists to stop.
    mod collision_detection {
        use super::*;
        use crate::{FileStateCache, ShellTool};

        /// The two call shapes `ToolIdentity`'s doc comment warns about,
        /// side by side.
        ///
        /// This row does **not** guard the discriminator, though an
        /// earlier version of it claimed to. Through a `&dyn Tool` the
        /// resolution is forced: `&'a dyn Tool` is not `'static`, so the
        /// blanket impl cannot apply and probing can only match
        /// `Self = dyn Tool` by value. A row phrased over `&dyn Tool`
        /// therefore restates its own signature and cannot fail. What
        /// actually pins the type half of `same_implementation` is the
        /// pair of impostor rows below, whose fake tools share the
        /// incumbent's *name*: collapse the type comparison and both go
        /// green-to-red.
        ///
        /// What is worth pinning here is the trap itself, because it is
        /// silent and the wrong shape compiles. Through the `Arc` the
        /// blanket impl *does* apply — `Arc<dyn Tool>` is sized and
        /// `'static`, autoref matches it at the first probe step, and
        /// every tool in the process reports one shared id. Asserting
        /// that equality is a real claim about method resolution, and it
        /// is the claim that makes the `&dyn Tool` argument types on
        /// `same_implementation` load-bearing rather than stylistic.
        #[test]
        fn impl_type_id_reads_the_pointer_unless_reborrowed_as_dyn_tool() {
            let echo: Arc<dyn Tool> = Arc::new(crate::EchoTool::new());
            let echo_again: Arc<dyn Tool> = Arc::new(crate::EchoTool::new());
            let math: Arc<dyn Tool> = Arc::new(crate::MathTool::new());

            fn id_of(tool: &dyn Tool) -> std::any::TypeId {
                tool.impl_type_id()
            }

            assert_eq!(
                id_of(echo.as_ref()),
                id_of(echo_again.as_ref()),
                "two instances of one tool type must share an impl_type_id"
            );
            assert_ne!(
                id_of(echo.as_ref()),
                id_of(math.as_ref()),
                "through a &dyn Tool the vtable entry is reached and \
                 distinct types report distinct ids"
            );

            // The trap: same expressions, no reborrow. Both answer
            // `TypeId::of::<Arc<dyn Tool>>()`, so a `same_implementation`
            // written over `Arc<dyn Tool>` would call every pair of tools
            // in the process the same implementation and never warn.
            #[allow(clippy::needless_borrow)]
            let via_arc_echo = (&echo).impl_type_id();
            #[allow(clippy::needless_borrow)]
            let via_arc_math = (&math).impl_type_id();
            assert_eq!(
                via_arc_echo, via_arc_math,
                "called on the Arc rather than on a &dyn Tool, \
                 impl_type_id reports the pointer's type — this is why \
                 same_implementation takes &dyn Tool arguments"
            );
            assert_eq!(
                via_arc_echo,
                std::any::TypeId::of::<Arc<dyn Tool>>(),
                "and the shared id is specifically the Arc's"
            );
        }

        /// The exact shape `ToolRegistry::attach_fs_cache_to_registry`
        /// and `AgentRuntime::register_fs_tools` produce: one `FsReadTool`
        /// replaced by another that is sandboxed and cache-aware. Four
        /// times per run, per read-family tool.
        #[test]
        fn a_reconfigured_fs_tool_is_the_same_implementation() {
            let plain: Arc<dyn Tool> = Arc::new(crate::FsReadTool::new());
            let sandboxed: Arc<dyn Tool> = Arc::new(
                crate::FsReadTool::sandboxed(std::path::PathBuf::from("."))
                    .with_cache(Arc::new(FileStateCache::default())),
            );

            assert!(
                same_implementation(plain.as_ref(), sandboxed.as_ref()),
                "re-registering fs_read with a sandbox root and a file-state \
                 cache is a configuration change, not a collision"
            );
        }

        /// The other half of the per-run churn: `with_shell_default_env`,
        /// `with_shell_spill`, `with_tool_output_truncate` and
        /// `with_project_root` each rebuild `ShellTool` with a different
        /// policy and re-register it under both `shell` and `shell_exec`.
        #[test]
        fn a_repolicied_shell_tool_is_the_same_implementation() {
            let unrestricted: Arc<dyn Tool> = Arc::new(ShellTool::with_policy(None, true));
            let sandboxed: Arc<dyn Tool> = Arc::new(ShellTool::with_policy(
                Some(std::path::PathBuf::from(".")),
                false,
            ));

            assert!(same_implementation(
                unrestricted.as_ref(),
                sandboxed.as_ref()
            ));
        }

        /// The complement, and the reason the check is not just "always
        /// silent": a different implementation arriving under an
        /// established name is still a collision.
        #[test]
        fn a_different_implementation_is_not_the_same_implementation() {
            let echo: Arc<dyn Tool> = Arc::new(crate::EchoTool::new());
            let math: Arc<dyn Tool> = Arc::new(crate::MathTool::new());

            assert!(!same_implementation(echo.as_ref(), math.as_ref()));
        }

        /// The type half of the discriminator, isolated. Two tools that
        /// agree on the name are still a collision when the code behind
        /// the name changed — this is the shape a real name clash takes
        /// (`register` keys on `tool.name()`, so a genuine clash always
        /// has matching names and differing types) and the row that
        /// fails if the type comparison is ever dropped.
        #[test]
        fn a_foreign_type_claiming_an_existing_name_is_a_collision() {
            let real: Arc<dyn Tool> = Arc::new(crate::EchoTool::new());
            let impostor: Arc<dyn Tool> = Arc::new(NativeTool::new("echo", |_| Ok(Value::Null)));

            assert_eq!(real.name(), impostor.name());
            assert!(!same_implementation(real.as_ref(), impostor.as_ref()));
        }

        /// The canonical-name half of the discriminator, which is what
        /// does the work on the alias path: re-pointing `shell_exec` at
        /// something that calls itself a different name changes what an
        /// established alias means, even when nothing about the type
        /// changed.
        #[test]
        fn same_type_under_a_different_canonical_name_is_a_collision() {
            let shell: Arc<dyn Tool> = Arc::new(NativeTool::new("shell", |_| Ok(Value::Null)));
            let shell_again: Arc<dyn Tool> =
                Arc::new(NativeTool::new("shell", |_| Ok(Value::Null)));
            let impostor: Arc<dyn Tool> =
                Arc::new(NativeTool::new("not_shell", |_| Ok(Value::Null)));

            assert!(!same_implementation(shell.as_ref(), impostor.as_ref()));
            assert!(
                same_implementation(shell.as_ref(), shell_again.as_ref()),
                "the name check must not make every re-registration a \
                 collision — same type and same canonical name is still \
                 the benign case"
            );
        }

        /// `register_builtin_tools_sandboxed` used to open-code the
        /// alias's `enabled_filter` check next to a raw map `insert`.
        /// #1260 routed it through `register_as` so the alias insert is
        /// collision-checked like every other one; these two rows pin
        /// that the filter semantics came along unchanged — the alias is
        /// accepted when the operator names *either* spelling, and the
        /// primary name is still filtered on its own.
        #[test]
        fn alias_is_registered_when_the_filter_names_only_the_alias() {
            let enabled = vec!["shell_exec".to_string()];
            let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &enabled);

            assert!(registry.contains("shell_exec"));
            assert!(
                !registry.contains("shell"),
                "the primary name is filtered on its own name, which the \
                 operator did not list"
            );
            assert_eq!(registry.len(), 1);
        }

        #[test]
        fn alias_is_registered_when_the_filter_names_only_the_primary() {
            let enabled = vec!["shell".to_string()];
            let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &enabled);

            assert!(registry.contains("shell"));
            assert!(
                registry.contains("shell_exec"),
                "register_as accepts the tool's canonical name in the \
                 filter, which is what the open-coded check did"
            );
            assert_eq!(registry.len(), 2);
        }
    }
}
