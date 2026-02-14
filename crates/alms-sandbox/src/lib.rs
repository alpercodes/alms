use alms_core::{AlmsError, AlmsResult};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use wasmtime::{Config as WasmConfig, Engine, Instance, Memory, Module, Store, TypedFunc};

pub mod builtin;
pub mod error;
pub mod registry;
pub mod sandbox;

pub use builtin::{BuiltinTool, EchoTool, HttpGetTool, MathTool};
pub use error::{SandboxError, SandboxResult};
pub use registry::ToolRegistry;
pub use sandbox::{Sandbox, SandboxConfig};

/// Tool trait - implemented by both WASM and native tools
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

    /// Check if this is a built-in tool
    fn is_builtin(&self) -> bool {
        false
    }

    /// Check if this is a WASM tool
    fn is_wasm(&self) -> bool {
        false
    }
}

/// Tool definition for registration
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub wasm_bytes: Option<Vec<u8>>,
    pub entry_point: String,
}

impl ToolDef {
    /// Create a new WASM tool definition
    pub fn new_wasm(name: impl Into<String>, wasm_bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            wasm_bytes: Some(wasm_bytes),
            entry_point: "execute".to_string(),
        }
    }

    /// Create a new tool definition (for built-in tools)
    pub fn new_builtin(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            wasm_bytes: None,
            entry_point: "execute".to_string(),
        }
    }

    /// Set the description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Set the entry point function name
    pub fn with_entry_point(mut self, entry: impl Into<String>) -> Self {
        self.entry_point = entry.into();
        self
    }
}

/// WASM Tool implementation
#[derive(Debug)]
pub struct WasmTool {
    name: String,
    description: String,
    wasm_bytes: Vec<u8>,
    entry_point: String,
    config: SandboxConfig,
}

impl WasmTool {
    /// Create a new WASM tool
    pub fn new(def: ToolDef, config: SandboxConfig) -> AlmsResult<Self> {
        let wasm_bytes = def
            .wasm_bytes
            .ok_or_else(|| AlmsError::InvalidConfig("WASM bytes required".to_string()))?;

        Ok(Self {
            name: def.name,
            description: def.description,
            wasm_bytes,
            entry_point: def.entry_point,
            config,
        })
    }

    /// Get the WASM bytes
    pub fn wasm_bytes(&self) -> &[u8] {
        &self.wasm_bytes
    }
}

#[async_trait::async_trait]
impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn is_wasm(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let mut sandbox = Sandbox::new(self.config.clone());
        sandbox
            .execute(&self.wasm_bytes, &self.entry_point, &self.name, params)
            .await
    }
}

/// A tool that wraps a native function
pub type NativeToolFn = Arc<
    dyn Fn(Value) -> SandboxResult<Value> + Send + Sync,
>;

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

        let result = tool.execute(serde_json::json!({"value": 21})).await.unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_tool_def() {
        let def = ToolDef::new_wasm("my_tool", vec![1, 2, 3])
            .with_description("A test tool")
            .with_entry_point("run");

        assert_eq!(def.name, "my_tool");
        assert_eq!(def.description, "A test tool");
        assert_eq!(def.entry_point, "run");
        assert!(def.wasm_bytes.is_some());
    }
}
