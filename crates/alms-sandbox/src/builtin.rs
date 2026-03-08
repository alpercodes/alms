use crate::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;

/// Built-in tool trait marker
pub trait BuiltinTool: Tool {}

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

impl BuiltinTool for EchoTool {}

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

impl BuiltinTool for MathTool {}

/// HTTP GET tool - performs HTTP GET requests
#[derive(Debug, Clone)]
pub struct HttpGetTool {
    client: reqwest::Client,
}

impl HttpGetTool {
    /// Create a new HTTP GET tool
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("ALMS/0.1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    /// Create with a custom client
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for HttpGetTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }

    fn description(&self) -> &str {
        "Performs an HTTP GET request to a URL and returns the response body"
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to send a GET request to"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("Missing 'url' field".to_string()))?;

        // Parse headers if provided
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(hdrs) = params.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in hdrs {
                if let Some(val_str) = value.as_str()
                    && let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    && let Ok(header_value) = reqwest::header::HeaderValue::from_str(val_str)
                {
                    headers.insert(header_name, header_value);
                }
            }
        }

        // Build request
        let mut request = self.client.get(url);

        // Add headers
        if !headers.is_empty() {
            request = request.headers(headers);
        }

        // Execute request
        let response = request.send().await.map_err(SandboxError::from)?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body_text = response.text().await.map_err(SandboxError::from)?;

        // Try to parse as JSON, fallback to string
        let body = if let Ok(json) = serde_json::from_str::<Value>(&body_text) {
            json
        } else {
            Value::String(body_text)
        };

        // Build response object
        let result = serde_json::json!({
            "status": status,
            "content_type": content_type,
            "body": body,
        });

        Ok(result)
    }
}

impl BuiltinTool for HttpGetTool {}

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

    #[tokio::test]
    async fn test_http_get_invalid_url() {
        let tool = HttpGetTool::new();

        // Missing URL
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_tool_descriptions() {
        assert!(!EchoTool::new().description().is_empty());
        assert!(!MathTool::new().description().is_empty());
        assert!(!HttpGetTool::new().description().is_empty());
    }
}
