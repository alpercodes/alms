use crate::sandbox::SandboxConfig;
use crate::{NativeTool, SandboxError, Tool, ToolDef, WasmTool, error::SandboxResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Tool registry for managing available tools
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    /// Registered tools: name -> tool
    tools: Arc<DashMap<String, Arc<dyn Tool>>>,
    /// Default sandbox config for WASM tools
    default_config: SandboxConfig,
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new() -> Self {
        Self {
            tools: Arc::new(DashMap::new()),
            default_config: SandboxConfig::default(),
        }
    }

    /// Create a new registry with custom default config
    pub fn with_config(config: SandboxConfig) -> Self {
        Self {
            tools: Arc::new(DashMap::new()),
            default_config: config,
        }
    }

    /// Register a tool
    pub fn register(&self, tool: Arc<dyn Tool>) -> SandboxResult<()> {
        let name = tool.name().to_string();

        if self.tools.contains_key(&name) {
            warn!("Tool '{}' already registered, replacing", name);
        }

        debug!("Registering tool: {}", name);
        self.tools.insert(name.clone(), tool);
        info!("Successfully registered tool: {}", name);

        Ok(())
    }

    /// Register a native tool
    pub fn register_native<F>(&self, name: impl Into<String>, handler: F) -> SandboxResult<()>
    where
        F: Fn(Value) -> SandboxResult<Value> + Send + Sync + 'static,
    {
        let tool = NativeTool::new(name, handler);
        self.register(Arc::new(tool))
    }

    /// Register a WASM tool from definition
    pub fn register_wasm(&self, def: ToolDef) -> SandboxResult<()> {
        let tool = WasmTool::new(def, self.default_config.clone())?;
        self.register(Arc::new(tool))
    }

    /// Register a WASM tool with custom config
    pub fn register_wasm_with_config(
        &self,
        def: ToolDef,
        config: SandboxConfig,
    ) -> SandboxResult<()> {
        let tool = WasmTool::new(def, config)?;
        self.register(Arc::new(tool))
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

    /// Unregister a tool
    pub fn unregister(&self, name: &str) -> SandboxResult<()> {
        if self.tools.remove(name).is_some() {
            info!("Unregistered tool: {}", name);
            Ok(())
        } else {
            Err(SandboxError::ToolNotFound(name.to_string()))
        }
    }

    /// List all registered tool names
    pub fn list(&self) -> Vec<String> {
        self.tools.iter().map(|e| e.key().clone()).collect()
    }

    /// List built-in tools
    pub fn list_builtin(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|e| e.value().is_builtin())
            .map(|e| e.key().clone())
            .collect()
    }

    /// List WASM tools
    pub fn list_wasm(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|e| e.value().is_wasm())
            .map(|e| e.key().clone())
            .collect()
    }

    /// Get the number of registered tools
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Clear all tools
    pub fn clear(&self) {
        self.tools.clear();
        info!("Cleared all tools from registry");
    }

    /// Get the default sandbox config
    pub fn default_config(&self) -> &SandboxConfig {
        &self.default_config
    }

    /// Set the default sandbox config
    pub fn set_default_config(&mut self, config: SandboxConfig) {
        self.default_config = config;
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
        let registry = Self::new();
        registry.register_builtin_tools_sandboxed(sandbox_root, shell_unrestricted, enabled);
        registry
    }

    /// Register built-in tools with optional sandbox configuration.
    ///
    /// When `enabled` is non-empty, only tools whose name appears in the list
    /// are registered. When empty, all builtins are registered.
    pub fn register_builtin_tools_sandboxed(
        &self,
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
    ) {
        use crate::builtin::{
            EchoTool, FsListTool, FsReadTool, FsWriteTool, HttpGetTool, MathTool, ShellExecTool,
        };

        // Build all tools, then filter by the enabled list.
        let shell_tool = ShellExecTool::with_policy(sandbox_root.clone(), shell_unrestricted);
        let (fs_read, fs_write, fs_list) = match sandbox_root {
            Some(ref root) => (
                FsReadTool::sandboxed(root.clone()),
                FsWriteTool::sandboxed(root.clone()),
                FsListTool::sandboxed(root.clone()),
            ),
            None => (FsReadTool::new(), FsWriteTool::new(), FsListTool::new()),
        };

        let all_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(EchoTool::new()),
            Arc::new(MathTool::new()),
            Arc::new(HttpGetTool::new()),
            Arc::new(shell_tool),
            Arc::new(fs_read),
            Arc::new(fs_write),
            Arc::new(fs_list),
        ];

        for tool in all_tools {
            if enabled.is_empty() || enabled.iter().any(|e| e == tool.name()) {
                if let Err(e) = self.register(tool) {
                    error!("Failed to register {} tool: {}", "builtin", e);
                }
            } else {
                debug!("Skipping disabled builtin tool: {}", tool.name());
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
        assert!(!registry.contains("shell_exec"));
        assert!(!registry.contains("fs_read"));
        assert!(!registry.contains("fs_write"));
        assert!(!registry.contains("fs_list"));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_empty_enabled_registers_all_builtins() {
        let registry = ToolRegistry::with_builtin_tools_sandboxed(None, false, &[]);
        assert!(registry.contains("echo"));
        assert!(registry.contains("math"));
        assert!(registry.contains("http_get"));
        assert!(registry.contains("shell_exec"));
        assert!(registry.contains("fs_read"));
        assert!(registry.contains("fs_write"));
        assert!(registry.contains("fs_list"));
        assert_eq!(registry.len(), 7);
    }
}
