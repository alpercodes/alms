// SPDX-License-Identifier: Apache-2.0

use crate::Tool;
use crate::error::SandboxResult;
use serde_json::Value;

/// Echo tool - returns the input unchanged
#[derive(Debug, Clone, Default)]
pub struct EchoTool;

impl EchoTool {
    /// Create a new echo tool
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Returns the input value unchanged. Useful for testing and debugging."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo back"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // Extract the 'message' field if present, otherwise return entire params
        if let Some(msg) = params.get("message") {
            Ok(msg.clone())
        } else {
            Ok(params)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool::new();

        // Test with message field
        let result = tool
            .execute(serde_json::json!({"message": "hello"}))
            .await
            .unwrap();
        assert_eq!(result, "hello");

        // Test without message field
        let result = tool
            .execute(serde_json::json!({"key": "value"}))
            .await
            .unwrap();
        assert_eq!(result, serde_json::json!({"key": "value"}));
    }
}
