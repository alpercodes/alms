use alms_core::{AlmsError, AlmsResult};
use alms_sandbox::{SandboxError, ToolRegistry as SandboxRegistry};
use serde_json::Value;
use std::collections::HashSet;
use tracing::{debug, warn};

/// Runtime tool registry wraps the sandbox registry
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    registry: SandboxRegistry,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            registry: SandboxRegistry::new(),
        }
    }

    /// Create registry with sandbox built-in tools (unrestricted).
    pub fn with_builtins() -> Self {
        Self {
            registry: SandboxRegistry::with_builtin_tools(),
        }
    }

    /// Create registry with sandbox-aware built-in tools.
    ///
    /// `enabled` — when non-empty, only builtins whose name appears in the
    /// list are registered. Empty slice = all builtins enabled.
    pub fn with_builtins_sandboxed(
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
    ) -> Self {
        Self {
            registry: SandboxRegistry::with_builtin_tools_sandboxed(
                sandbox_root,
                shell_unrestricted,
                enabled,
            ),
        }
    }

    /// Register a custom tool implementation (e.g. WorkspaceWriteTool).
    pub fn register(&self, tool: std::sync::Arc<dyn alms_sandbox::Tool>) {
        if let Err(e) = self.registry.register(tool) {
            warn!("Failed to register tool: {}", e);
        }
    }

    /// Register a tool under a specific alias name (for backward compatibility).
    pub fn register_arc_as(&self, alias: &str, tool: std::sync::Arc<dyn alms_sandbox::Tool>) {
        if let Err(e) = self.registry.register_as(alias, tool) {
            warn!("Failed to register tool alias '{}': {}", alias, e);
        }
    }

    /// List all tool names
    pub fn list(&self) -> Vec<String> {
        self.registry.list()
    }

    /// Check if tool exists
    pub fn contains(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    /// Get tool definitions for LLM with real parameter schemas.
    ///
    /// Deduplicates by `tool.name()` so that aliases (e.g. "shell_exec" ->
    /// "shell") do not produce duplicate function definitions — OpenAI and
    /// other providers reject payloads with duplicate function names.
    pub fn to_definitions(&self) -> Vec<crate::llm_types::ToolDefinition> {
        let mut seen = HashSet::new();
        self.registry
            .list()
            .into_iter()
            .filter_map(|name| {
                let tool = self.registry.lookup(&name).ok()?;
                let canonical_name = tool.name().to_string();
                if !seen.insert(canonical_name) {
                    return None;
                }
                Some(
                    crate::llm_types::ToolDefinition::new(tool.name(), tool.description())
                        .with_parameters(tool.parameters()),
                )
            })
            .collect()
    }

    /// Execute a tool by name via sandbox
    pub async fn execute(&self, name: &str, params: Value) -> AlmsResult<Value> {
        debug!("Executing tool via sandbox: {}", name);
        self.registry
            .execute(name, params)
            .await
            .map_err(|e| match e {
                SandboxError::ToolNotFound(tool) => {
                    AlmsError::ToolExecution(format!("Tool '{}' not found", tool))
                }
                other => {
                    warn!("Tool execution failed: {}", other);
                    AlmsError::ToolExecution(other.to_string())
                }
            })
    }
}
