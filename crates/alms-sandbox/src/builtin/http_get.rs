use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use serde_json::Value;

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
            .user_agent(concat!("ALMS/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("Failed to create HTTP client");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_http_get_invalid_url() {
        let tool = HttpGetTool::new();

        // Missing URL
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
