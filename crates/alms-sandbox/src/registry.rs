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

    /// Create a registry with all built-in tools registered
    pub fn with_builtin_tools() -> Self {
        let registry = Self::new();
        registry.register_builtin_tools();
        registry
    }

    /// Register all built-in tools
    pub fn register_builtin_tools(&self) {
        use crate::builtin::{
            EchoTool, FsListTool, FsReadTool, FsWriteTool, HttpGetTool, MathTool, ShellExecTool,
        };

        // Register echo tool
        if let Err(e) = self.register(Arc::new(EchoTool::new())) {
            error!("Failed to register echo tool: {}", e);
        }

        // Register math tool
        if let Err(e) = self.register(Arc::new(MathTool::new())) {
            error!("Failed to register math tool: {}", e);
        }

        // Register http_get tool
        if let Err(e) = self.register(Arc::new(HttpGetTool::new())) {
            error!("Failed to register http_get tool: {}", e);
        }

        // Register shell_exec tool
        if let Err(e) = self.register(Arc::new(ShellExecTool::new())) {
            error!("Failed to register shell_exec tool: {}", e);
        }

        // Register filesystem tools
        if let Err(e) = self.register(Arc::new(FsReadTool::new())) {
            error!("Failed to register fs_read tool: {}", e);
        }
        if let Err(e) = self.register(Arc::new(FsWriteTool::new())) {
            error!("Failed to register fs_write tool: {}", e);
        }
        if let Err(e) = self.register(Arc::new(FsListTool::new())) {
            error!("Failed to register fs_list tool: {}", e);
        }

        info!("Registered all built-in tools");
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
}
