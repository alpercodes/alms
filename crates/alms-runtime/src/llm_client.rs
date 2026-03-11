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
            error!(
                "LLM api_key is empty — calls will fail with 401. Set OPENROUTER_API_KEY or OPENAI_API_KEY."
            );
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

    /// Send a streaming completion request.
    ///
    /// Returns a stream of `StreamChunk`s. The stream ends naturally when the
    /// LLM sends `[DONE]`. TCP chunk boundaries are handled by an internal
    /// line buffer so SSE events split across packets are reassembled correctly.
    pub async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> AlmsResult<futures::stream::BoxStream<'static, AlmsResult<StreamChunk>>> {
        use futures::{StreamExt, stream};

        if self.config.mock {
            let chunks = self.mock_stream_chunks(&request);
            return Ok(stream::iter(chunks.into_iter().map(Ok)).boxed());
        }

        let mut request = request;
        request.stream = Some(true);
        request.stream_options = Some(StreamOptions {
            include_usage: true,
        });

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

        // Buffer raw bytes into complete SSE events. TCP chunks don't align
        // with SSE event boundaries, so we accumulate lines and yield parsed
        // StreamChunks only when we see a blank-line separator.
        let byte_stream = response.bytes_stream();
        let stream = futures::stream::unfold(
            (byte_stream, String::new()),
            |(mut bytes, mut buf)| async move {
                use futures::StreamExt as _;
                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(pos) = buf.find("\n\n") {
                        let event_text = buf[..pos].to_string();
                        buf = buf[pos + 2..].to_string();
                        if let Some(chunk) = Self::parse_sse_event(&event_text) {
                            return Some((Ok(chunk), (bytes, buf)));
                        }
                        continue; // skip non-data events (comments, [DONE])
                    }
                    // Need more data from the network
                    match bytes.next().await {
                        Some(Ok(b)) => {
                            // Normalize \r\n → \n so the \n\n event separator works
                            // regardless of whether the upstream sends CRLF or LF.
                            let text = String::from_utf8_lossy(&b).replace("\r\n", "\n");
                            buf.push_str(&text);
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(AlmsError::Runtime(format!("Stream error: {}", e))),
                                (bytes, buf),
                            ));
                        }
                        None => {
                            // Stream ended — try to parse any remaining buffered data
                            if !buf.trim().is_empty() {
                                let remaining = std::mem::take(&mut buf);
                                if let Some(chunk) = Self::parse_sse_event(remaining.trim()) {
                                    return Some((Ok(chunk), (bytes, buf)));
                                }
                            }
                            return None; // stream complete
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }

    /// Parse a single SSE event block (one or more `data:` lines) into a StreamChunk.
    /// Returns `None` for `[DONE]` sentinels and comment-only events.
    fn parse_sse_event(event: &str) -> Option<StreamChunk> {
        for line in event.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                let data = data.trim();
                if data == "[DONE]" {
                    return None;
                }
                match serde_json::from_str::<StreamChunk>(data) {
                    Ok(chunk) => return Some(chunk),
                    Err(e) => {
                        warn!("Failed to parse SSE chunk: {} - data: {}", e, data);
                        continue;
                    }
                }
            }
        }
        None
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

    /// Produce multiple mock stream chunks (word-by-word) to simulate real streaming.
    fn mock_stream_chunks(&self, request: &CompletionRequest) -> Vec<StreamChunk> {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .and_then(|msg| msg.content.clone())
            .unwrap_or_else(|| "(no user input)".to_string());

        let full_text = format!("[mock] {}", content);
        let words: Vec<&str> = full_text.split_inclusive(' ').collect();
        let mut chunks = Vec::new();

        for (i, word) in words.iter().enumerate() {
            let is_last = i == words.len() - 1;
            chunks.push(StreamChunk {
                id: "mock-stream".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 0,
                model: request.model.clone(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: Delta {
                        role: if i == 0 {
                            Some("assistant".to_string())
                        } else {
                            None
                        },
                        content: Some(word.to_string()),
                        tool_calls: None,
                    },
                    finish_reason: if is_last {
                        Some("stop".to_string())
                    } else {
                        None
                    },
                }],
                usage: if is_last {
                    Some(Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    })
                } else {
                    None
                },
            });
        }

        chunks
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

    #[test]
    fn test_parse_sse_event_content() {
        let event = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk = LlmClient::parse_sse_event(event).expect("should parse");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_parse_sse_event_done() {
        assert!(LlmClient::parse_sse_event("data: [DONE]").is_none());
    }

    #[test]
    fn test_parse_sse_event_tool_call_delta() {
        let event = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"te"}}]},"finish_reason":null}]}"#;
        let chunk = LlmClient::parse_sse_event(event).expect("should parse");
        let tc = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tc[0].index, 0);
        assert_eq!(tc[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            tc[0].function.as_ref().unwrap().name.as_deref(),
            Some("echo")
        );
        assert_eq!(
            tc[0].function.as_ref().unwrap().arguments.as_deref(),
            Some("{\"te")
        );
    }

    #[test]
    fn test_parse_sse_event_with_usage() {
        let event = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
        let chunk = LlmClient::parse_sse_event(event).expect("should parse");
        let usage = chunk.usage.expect("should have usage");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
    }

    #[tokio::test]
    async fn test_mock_stream_produces_multiple_chunks() {
        use futures::StreamExt;
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        let request =
            CompletionRequest::new("test").with_messages(vec![LlmMessage::user("hello world")]);
        let mut stream = client.complete_stream(request).await.unwrap();

        let mut chunks = Vec::new();
        while let Some(result) = stream.next().await {
            chunks.push(result.unwrap());
        }

        // "[mock] hello world" split by words = ["[mock] ", "hello ", "world"]
        assert!(chunks.len() >= 2, "mock should produce multiple chunks");

        // Reassemble content
        let full: String = chunks
            .iter()
            .filter_map(|c| c.choices.first()?.delta.content.as_deref())
            .collect();
        assert_eq!(full, "[mock] hello world");

        // Last chunk should have finish_reason and usage
        let last = chunks.last().unwrap();
        assert_eq!(last.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(last.usage.is_some());
    }
}
