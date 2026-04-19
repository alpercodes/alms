#[cfg(test)]
use crate::NativeTool;
use crate::{SandboxError, Tool, error::SandboxResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

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

        if self.tools.contains_key(&name) {
            warn!("Tool '{}' already registered, replacing", name);
        }

        debug!("Registering tool: {}", name);
        self.tools.insert(name.clone(), tool);
        info!("Successfully registered tool: {}", name);

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

        if self.tools.contains_key(alias) {
            warn!("Tool alias '{}' already registered, replacing", alias);
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
        use crate::shell::{SHELL_TOOL_ALIAS, SHELL_TOOL_NAME, ShellTool};

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
        // backward compatibility. The enabled_filter accepts either "shell"
        // or "shell_exec" for the alias registration.
        let shell_alias_allowed = self.enabled_filter.is_empty()
            || self
                .enabled_filter
                .iter()
                .any(|e| e == SHELL_TOOL_NAME || e == SHELL_TOOL_ALIAS);
        if shell_alias_allowed {
            self.tools.insert(SHELL_TOOL_ALIAS.to_string(), shell_tool);
            debug!(
                "Registered shell tool alias '{}' -> '{}'",
                SHELL_TOOL_ALIAS, SHELL_TOOL_NAME
            );
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
}
