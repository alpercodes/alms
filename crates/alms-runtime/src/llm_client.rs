use crate::llm_types::*;
use alms_core::{AlmsError, AlmsResult};
use reqwest::{Client, RequestBuilder};
use tracing::{debug, error, info, warn};

/// LLM client for making API calls
#[derive(Debug, Clone)]
pub struct LlmClient {
    client: Client,
    config: LlmConfig,
}

impl LlmClient {
    /// Create new LLM client with config
    pub fn new(config: LlmConfig) -> AlmsResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| AlmsError::Runtime(format!("Failed to create HTTP client: {}", e)))?;

        info!("LLM client initialized with base URL: {}", config.base_url);
        if config.api_key.is_empty() {
            error!("LLM api_key is empty — calls will fail with 401. Set OPENROUTER_API_KEY or OPENAI_API_KEY.");
        } else {
            info!("LLM api_key loaded ({} chars)", config.api_key.len());
        }

        Ok(Self { client, config })
    }

    /// Create from environment variables
    pub fn from_env() -> AlmsResult<Self> {
        Self::new(LlmConfig::from_env())
    }

    /// Create a completion request builder
    fn build_request(&self, request: &CompletionRequest) -> AlmsResult<RequestBuilder> {
        let url = format!("{}/chat/completions", self.config.base_url);

        debug!("Sending completion request to {}", url);

        Ok(self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(request))
    }

    /// Send a non-streaming completion request
    pub async fn complete(&self, request: CompletionRequest) -> AlmsResult<CompletionResponse> {
        if self.config.mock {
            return Ok(self.mock_response(&request));
        }

        let req = self.build_request(&request)?;

        let response = req
            .send()
            .await
            .map_err(|e| AlmsError::Runtime(format!("HTTP request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("LLM API error: {} - {}", status, error_text);
            return Err(AlmsError::Runtime(format!(
                "LLM API error: {} - {}",
                status, error_text
            )));
        }

        let completion: CompletionResponse = response
            .json()
            .await
            .map_err(|e| AlmsError::Runtime(format!("Failed to parse response: {}", e)))?;

        if let Some(usage) = &completion.usage {
            debug!(
                "Completion used {} prompt + {} completion = {} total tokens",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
            );
        }

        Ok(completion)
    }

    /// Send a streaming completion request
    pub async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<futures::stream::BoxStream<'static, AlmsResult<StreamChunk>>> {
        use futures::{StreamExt, stream};

        if self.config.mock {
            let chunk = self.mock_stream_chunk(&request);
            return Ok(stream::once(async move { Ok(chunk) }).boxed());
        }

        let mut request = request;
        request.stream = Some(true);

        let req = self.build_request(&request)?;

        let response = req
            .send()
            .await
            .map_err(|e| AlmsError::Runtime(format!("HTTP request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("LLM API error: {} - {}", status, error_text);
            return Err(AlmsError::Runtime(format!(
                "LLM API error: {} - {}",
                status, error_text
            )));
        }

        let stream = response.bytes_stream().map(|result| {
            result
                .map_err(|e| AlmsError::Runtime(format!("Stream error: {}", e)))
                .and_then(|bytes| {
                    let text = String::from_utf8_lossy(&bytes);
                    Self::parse_sse_chunk(&text)
                })
        });

        Ok(stream.boxed())
    }

    /// Parse a Server-Sent Events chunk
    fn parse_sse_chunk(chunk: &str) -> AlmsResult<StreamChunk> {
        for line in chunk.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with(":") {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    return Err(AlmsError::Runtime("Stream complete".to_string()));
                }

                match serde_json::from_str::<StreamChunk>(data) {
                    Ok(chunk) => return Ok(chunk),
                    Err(e) => {
                        warn!("Failed to parse SSE chunk: {} - data: {}", e, data);
                        continue;
                    }
                }
            }
        }

        Err(AlmsError::Runtime("No valid SSE data found".to_string()))
    }

    /// Quick completion with default model
    pub async fn quick_complete(&self, messages: Vec<LlmMessage>) -> AlmsResult<String> {
        let request = CompletionRequest::new(&self.config.default_model).with_messages(messages);

        let response = self.complete(request).await?;

        response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| AlmsError::Runtime("No response from LLM".to_string()))
    }

    fn mock_response(&self, request: &CompletionRequest) -> CompletionResponse {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .and_then(|msg| msg.content.clone())
            .unwrap_or_else(|| "(no user input)".to_string());

        CompletionResponse {
            id: "mock-completion".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: LlmMessage::assistant(format!("[mock] {}", content)),
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
        }
    }

    fn mock_stream_chunk(&self, request: &CompletionRequest) -> StreamChunk {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .and_then(|msg| msg.content.clone())
            .unwrap_or_else(|| "(no user input)".to_string());

        StreamChunk {
            id: "mock-stream".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: request.model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: Delta {
                    role: Some("assistant".to_string()),
                    content: Some(format!("[mock] {}", content)),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
        }
    }

    /// Override the default model, returning a new client.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.config.default_model = model.into();
        self
    }

    /// Get default model name
    pub fn default_model(&self) -> &str {
        &self.config.default_model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_config_from_env() {
        // This test just verifies the config loads without panicking
        let config = LlmConfig::from_env();
        assert!(!config.base_url.is_empty());
        assert!(!config.default_model.is_empty());
    }

    #[test]
    fn test_completion_request_builder() {
        let request = CompletionRequest::new("test-model")
            .with_messages(vec![LlmMessage::user("Hello")])
            .with_temperature(0.7)
            .with_max_tokens(100);

        assert_eq!(request.model, "test-model");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.max_tokens, Some(100));
    }

    #[test]
    fn test_tool_definition_builder() {
        let tool = ToolDefinition::new("calculator", "Perform arithmetic operations")
            .with_parameters(serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string" }
                },
                "required": ["expression"]
            }));

        assert_eq!(tool.function.name, "calculator");
        assert_eq!(tool.function.description, "Perform arithmetic operations");
        assert_eq!(tool.tool_type, "function");
    }
}
