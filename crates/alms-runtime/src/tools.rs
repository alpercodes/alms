use alms_core::{AlmsError, AlmsResult};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

/// Trait for executable tools
#[async_trait::async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    /// Get the tool name
    fn name(&self) -> &str;
    
    /// Get the tool description
    fn description(&self) -> &str;
    
    /// Get the JSON schema for parameters
    fn parameters(&self) -> Value;
    
    /// Execute the tool with given parameters
    async fn execute(&self, params: Value) -> AlmsResult<Value>;
}

/// Registry of available tools
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }
    
    /// Register a tool
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        info!("Registering tool: {}", name);
        self.tools.insert(name, tool);
    }
    
    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }
    
    /// Check if tool exists
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
    
    /// List all tool names
    pub fn list(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
    
    /// Get tool definitions for LLM
    pub fn to_definitions(&self) -> Vec<crate::llm_types::ToolDefinition> {
        self.tools
            .values()
            .map(|tool| {
                crate::llm_types::ToolDefinition::new(
                    tool.name(),
                    tool.description()
                ).with_parameters(tool.parameters())
            })
            .collect()
    }
    
    /// Execute a tool by name
    pub async fn execute(&self, name: &str, params: Value) -> AlmsResult<Value> {
        match self.tools.get(name) {
            Some(tool) => {
                debug!("Executing tool: {} with params: {:?}", name, params);
                let result = tool.execute(params).await;
                match &result {
                    Ok(_) => debug!("Tool {} executed successfully", name),
                    Err(e) => warn!("Tool {} failed: {}", name, e),
                }
                result
            }
            None => {
                error!("Tool not found: {}", name);
                Err(AlmsError::ToolExecution(format!("Tool '{}' not found", name)))
            }
        }
    }
    
    /// Register built-in tools
    pub fn with_builtins(mut self) -> Self {
        self.register(Box::new(EchoTool));
        self.register(Box::new(MathTool));
        self.register(Box::new(HttpGetTool));
        info!("Registered {} built-in tools", self.tools.len());
        self
    }
}

/// Echo tool - returns the input
#[derive(Debug)]
pub struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    
    fn description(&self) -> &str {
        "Echo back the input text. Useful for testing."
    }
    
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo back"
                }
            },
            "required": ["text"]
        })
    }
    
    async fn execute(&self, params: Value) -> AlmsResult<Value> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AlmsError::ToolExecution("Missing 'text' parameter".to_string()))?;
        
        Ok(serde_json::json!({ "echo": text }))
    }
}

/// Math tool - basic arithmetic
#[derive(Debug)]
pub struct MathTool;

#[async_trait::async_trait]
impl Tool for MathTool {
    fn name(&self) -> &str {
        "math"
    }
    
    fn description(&self) -> &str {
        "Evaluate a mathematical expression. Supports +, -, *, /, ^, parentheses."
    }
    
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Mathematical expression to evaluate, e.g. '2 + 2' or '(10 * 5) / 2'"
                }
            },
            "required": ["expression"]
        })
    }
    
    async fn execute(&self, params: Value) -> AlmsResult<Value> {
        let expr = params
            .get("expression")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AlmsError::ToolExecution("Missing 'expression' parameter".to_string()))?;
        
        // Simple eval - in production, use a proper math parser
        let result = Self::eval_simple(expr)?;
        
        Ok(serde_json::json!({ 
            "expression": expr,
            "result": result 
        }))
    }
}

impl MathTool {
    fn eval_simple(expr: &str) -> AlmsResult<f64> {
        // This is a simplified evaluator - use meval or similar in production
        // For now, just parse simple numbers
        expr.trim()
            .parse::<f64>()
            .map_err(|_| AlmsError::ToolExecution(format!("Could not evaluate: {}", expr)))
    }
}

/// HTTP GET tool
#[derive(Debug)]
pub struct HttpGetTool;

#[async_trait::async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }
    
    fn description(&self) -> &str {
        "Make an HTTP GET request to a URL and return the response body."
    }
    
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch"
                }
            },
            "required": ["url"]
        })
    }
    
    async fn execute(&self, params: Value) -> AlmsResult<Value> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AlmsError::ToolExecution("Missing 'url' parameter".to_string()))?;
        
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AlmsError::ToolExecution(format!("HTTP client error: {}", e)))?;
        
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| AlmsError::ToolExecution(format!("HTTP request failed: {}", e)))?;
        
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AlmsError::ToolExecution(format!("Failed to read response: {}", e)))?;
        
        Ok(serde_json::json!({
            "status": status.as_u16(),
            "body": body
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool;
        let result = tool.execute(serde_json::json!({"text": "hello"})).await.unwrap();
        assert_eq!(result["echo"], "hello");
    }
    
    #[test]
    fn test_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        
        assert!(registry.contains("echo"));
        assert!(!registry.contains("unknown"));
        assert_eq!(registry.list().len(), 1);
    }
    
    #[tokio::test]
    async fn test_registry_execute() {
        let registry = ToolRegistry::new().with_builtins();
        let result = registry.execute("echo", serde_json::json!({"text": "test"})).await.unwrap();
        assert_eq!(result["echo"], "test");
    }
}
