use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

pub mod builtin;
pub mod error;
pub mod file_state_cache;
pub mod registry;
pub mod shell;

pub use builtin::{
    DatetimeTool, EchoTool, FsEditTool, FsGlobTool, FsGrepTool, FsListTool, FsReadTool,
    FsWriteTool, HttpGetTool, MathTool,
};
pub use error::{SandboxError, SandboxResult};
pub use file_state_cache::FileStateCache;
pub use registry::ToolRegistry;
pub use shell::ShellTool;

/// Type alias for backward compatibility. Use [`ShellTool`] instead.
pub type ShellExecTool = ShellTool;

/// Per-call context threaded into [`Tool::execute_with_context`].
///
/// Carries identifiers the runtime knows about a specific tool invocation
/// that are not part of the LLM-visible `params` payload. Today only
/// `invocation_id` is populated; the struct is non-exhaustive so future
/// per-call fields (run id, agent id, etc.) can be added additively without
/// breaking external implementors of [`Tool`].
///
/// `invocation_id` is the same uuid the runtime emits on the matching
/// `ToolStart` event and uses to thread `ToolEnd`. `InvokeAgentTool`
/// reads it so the coordinator can carry it on the `subagent_started`
/// event back to the parent's stream (#1105) — the frontend resolves
/// SubagentBar entries by this id when the subagent has no name param.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ToolContext {
    /// Unique id of this specific tool invocation. Matches the
    /// `invocation_id` on the runtime's `ToolStart` / `ToolEnd` events
    /// for this call.
    pub invocation_id: Uuid,
}

impl ToolContext {
    /// Construct a new context for the given invocation id.
    pub fn new(invocation_id: Uuid) -> Self {
        Self { invocation_id }
    }
}

/// Tool trait — implemented by native tools registered with the runtime.
#[async_trait::async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    /// Get the tool name
    fn name(&self) -> &str;

    /// Get the tool description
    fn description(&self) -> &str;

    /// Get the JSON Schema for this tool's parameters.
    /// LLMs use this to know what arguments the tool accepts.
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Execute the tool with JSON parameters
    async fn execute(&self, params: Value) -> SandboxResult<Value>;

    /// Execute the tool with JSON parameters and per-call context.
    ///
    /// Default implementation discards `ctx` and calls [`Tool::execute`]
    /// so existing tools work unchanged. Tools that need per-call
    /// metadata (e.g. `InvokeAgentTool` needs the parent's
    /// `invocation_id` to thread onto the `subagent_started` SSE event
    /// at #1105) override this method.
    async fn execute_with_context(&self, params: Value, _ctx: ToolContext) -> SandboxResult<Value> {
        self.execute(params).await
    }

    /// Check if this is a built-in tool
    fn is_builtin(&self) -> bool {
        false
    }

    /// Whether this tool bypasses the approval workflow in guarded posture.
    ///
    /// Tools that are inherently safe and read-only (e.g. `datetime`, `echo`,
    /// `list_agents`) return `true` here so that operators are not prompted
    /// for approval on zero-risk operations.
    ///
    /// Defaults to `false` — most tools require approval in guarded mode.
    fn is_auto_approved(&self) -> bool {
        false
    }
}

/// A tool that wraps a native function
pub type NativeToolFn = Arc<dyn Fn(Value) -> SandboxResult<Value> + Send + Sync>;

/// Native tool implementation
#[derive(Clone)]
pub struct NativeTool {
    name: String,
    description: String,
    handler: NativeToolFn,
}

impl std::fmt::Debug for NativeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .finish()
    }
}

impl NativeTool {
    /// Create a new native tool
    pub fn new<F>(name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(Value) -> SandboxResult<Value> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            description: String::new(),
            handler: Arc::new(handler),
        }
    }

    /// Set the description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }
}

#[async_trait::async_trait]
impl Tool for NativeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn is_builtin(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        (self.handler)(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_native_tool() {
        let tool = NativeTool::new("test", |params| {
            let value = params.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(Value::from(value * 2))
        });

        let result = tool
            .execute(serde_json::json!({"value": 21}))
            .await
            .unwrap();
        assert_eq!(result, 42);
    }
}
