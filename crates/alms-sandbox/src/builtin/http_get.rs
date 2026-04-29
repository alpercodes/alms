use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use serde_json::Value;

/// Absolute ceiling on the response body `http_get` will read into memory
/// before bailing out (issue #851).
///
/// This is *not* the agent-visible cap — that comes from the in-loop
/// truncation service in `alms-runtime` which trims to ~32 KB. The 5 MB
/// ceiling is purely a defense against `http_get` pulling a 1 GB download
/// and OOM'ing the daemon before the truncation service has a chance to
/// act. Pulled bytes never leave this function — the truncation service
/// strips them down to a bounded preview before they touch any agent
/// context.
pub const MAX_RESPONSE_BYTES: u64 = 5 * 1024 * 1024;

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

        // Bail early when the server's `Content-Length` already exceeds
        // the pre-fetch ceiling. This avoids streaming gigabytes off the
        // wire just to reject them (issue #851).
        if let Some(len) = response.content_length()
            && len > MAX_RESPONSE_BYTES
        {
            return Err(SandboxError::InvalidParameters(format!(
                "Response body declared {len} bytes, exceeds {MAX_RESPONSE_BYTES} byte cap. \
                 http_get is intended for small responses; use the agent's tools to fetch \
                 large payloads to disk via the shell tool (e.g. `curl -o file.bin <url>`)."
            )));
        }

        // Stream the body chunk-by-chunk so we can stop reading once we
        // pass the pre-fetch ceiling. `response.bytes_stream()` decodes
        // transfer-encoding (e.g. chunked) for us; the cap is checked
        // against accumulated decoded bytes.
        use futures::StreamExt;
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(SandboxError::from)?;
            if buf.len().saturating_add(chunk.len()) as u64 > MAX_RESPONSE_BYTES {
                return Err(SandboxError::InvalidParameters(format!(
                    "Response body exceeded {MAX_RESPONSE_BYTES} byte cap mid-stream. \
                     http_get is intended for small responses; use the shell tool to \
                     fetch large payloads to disk."
                )));
            }
            buf.extend_from_slice(&chunk);
        }

        // Decode as UTF-8 (lossy) so binary-ish bodies don't crash the tool.
        // Truncation in the LLM-visible context is handled by the in-loop
        // truncation service in `alms-runtime`; this tool is responsible
        // only for the *pre-fetch* ceiling.
        let body_text = String::from_utf8_lossy(&buf).into_owned();

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

    #[test]
    fn pre_fetch_ceiling_is_5mb() {
        // Sanity check: this is a load-bearing constant referenced from
        // the #851 design. If a future change shrinks it without updating
        // documentation, this test will catch the drift.
        assert_eq!(MAX_RESPONSE_BYTES, 5 * 1024 * 1024);
    }
}
