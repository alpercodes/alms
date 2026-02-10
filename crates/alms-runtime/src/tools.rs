use alms_core::{AlmsError, AlmsResult};
use alms_sandbox::{SandboxError, ToolRegistry as SandboxRegistry};
use serde_json::Value;
use tracing::{debug, warn};

/// Runtime tool registry wraps the sandbox registry
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    registry: SandboxRegistry,
}

impl ToolRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            registry: SandboxRegistry::new(),
        }
    }

    /// Create registry with sandbox built-in tools
    pub fn with_builtins() -> Self {
        Self {
            registry: SandboxRegistry::with_builtin_tools(),
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

    /// Get tool definitions for LLM (parameters TBD)
    pub fn to_definitions(&self) -> Vec<crate::llm_types::ToolDefinition> {
        self.registry
            .list()
            .into_iter()
            .filter_map(|name| {
                let tool = self.registry.lookup(&name).ok()?;
                Some(
                    crate::llm_types::ToolDefinition::new(tool.name(), tool.description())
                        .with_parameters(serde_json::json!({
                            "type": "object",
                            "properties": {},
                            "required": []
                        })),
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
