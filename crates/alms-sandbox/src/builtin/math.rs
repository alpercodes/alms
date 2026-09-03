// SPDX-License-Identifier: Apache-2.0

use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use serde_json::Value;

/// Math tool - performs mathematical operations
#[derive(Debug, Clone, Default)]
pub struct MathTool;

impl MathTool {
    /// Create a new math tool
    pub fn new() -> Self {
        Self
    }

    /// Perform addition
    fn add(&self, a: f64, b: f64) -> f64 {
        a + b
    }

    /// Perform subtraction
    fn subtract(&self, a: f64, b: f64) -> f64 {
        a - b
    }

    /// Perform multiplication
    fn multiply(&self, a: f64, b: f64) -> f64 {
        a * b
    }

    /// Perform division
    fn divide(&self, a: f64, b: f64) -> SandboxResult<f64> {
        if b == 0.0 {
            Err(SandboxError::InvalidParameters(
                "Division by zero".to_string(),
            ))
        } else {
            Ok(a / b)
        }
    }

    /// Calculate power
    fn power(&self, base: f64, exp: f64) -> f64 {
        base.powf(exp)
    }

    /// Calculate square root
    fn sqrt(&self, n: f64) -> SandboxResult<f64> {
        if n < 0.0 {
            Err(SandboxError::InvalidParameters(
                "Cannot calculate square root of negative number".to_string(),
            ))
        } else {
            Ok(n.sqrt())
        }
    }

    /// Calculate absolute value
    fn abs(&self, n: f64) -> f64 {
        n.abs()
    }

    /// Round to nearest integer
    fn round(&self, n: f64) -> f64 {
        n.round()
    }

    /// Floor
    fn floor(&self, n: f64) -> f64 {
        n.floor()
    }

    /// Ceiling
    fn ceil(&self, n: f64) -> f64 {
        n.ceil()
    }
}

#[async_trait::async_trait]
impl Tool for MathTool {
    fn name(&self) -> &str {
        "math"
    }

    fn description(&self) -> &str {
        "Performs mathematical operations: add, subtract, multiply, divide, power, sqrt, abs, round, floor, ceil"
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "The math operation to perform",
                    "enum": ["add", "subtract", "multiply", "divide", "power", "sqrt", "abs", "round", "floor", "ceil"]
                },
                "a": {
                    "type": "number",
                    "description": "First operand (used by add, subtract, multiply, divide, power)"
                },
                "b": {
                    "type": "number",
                    "description": "Second operand (used by add, subtract, multiply, divide, power)"
                },
                "n": {
                    "type": "number",
                    "description": "Single operand (used by sqrt, abs, round, floor, ceil). Falls back to 'a' if not provided."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let operation = params
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("Missing 'operation' field".to_string())
            })?;

        let a = params.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = params.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let result = match operation {
            "add" => self.add(a, b),
            "subtract" => self.subtract(a, b),
            "multiply" => self.multiply(a, b),
            "divide" => self.divide(a, b)?,
            "power" => self.power(a, b),
            "sqrt" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.sqrt(n)?
            }
            "abs" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.abs(n)
            }
            "round" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.round(n)
            }
            "floor" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.floor(n)
            }
            "ceil" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.ceil(n)
            }
            _ => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown operation: {}",
                    operation
                )));
            }
        };

        // Return as number if it's a whole number, otherwise float
        if result.fract() == 0.0 && result.is_finite() {
            Ok(Value::from(result as i64))
        } else {
            Ok(Value::from(result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_math_tool_add() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "add", "a": 10, "b": 32}))
            .await
            .unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_math_tool_divide() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "divide", "a": 10, "b": 2}))
            .await
            .unwrap();
        assert_eq!(result, 5);

        // Test division by zero
        let result = tool
            .execute(serde_json::json!({"operation": "divide", "a": 10, "b": 0}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_math_tool_sqrt() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "sqrt", "n": 16}))
            .await
            .unwrap();
        assert_eq!(result, 4);

        // Test negative number
        let result = tool
            .execute(serde_json::json!({"operation": "sqrt", "n": -1}))
            .await;
        assert!(result.is_err());
    }
}
