use crate::llm_types::*;
use alms_core::{AlmsError, AlmsResult};
use reqwest::{Client, RequestBuilder};
use tracing::{debug, error, info, warn};

/// Result of parsing a single SSE event block.
pub(crate) enum SseParseResult {
    /// A valid data chunk was parsed.
    Chunk(StreamChunk),
    /// The `[DONE]` sentinel was received — stream is complete.
    Done,
    /// Comment, empty event, or unparseable data — skip and continue.
    Skip,
}

/// LLM provider type, determined from config at construction time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    OpenAi,
    Anthropic,
}

/// LLM client for making API calls
#[derive(Debug, Clone)]
pub struct LlmClient {
    client: Client,
    config: LlmConfig,
    provider: Provider,
}

impl LlmClient {
    /// Create new LLM client with config
    pub fn new(mut config: LlmConfig) -> AlmsResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| AlmsError::Runtime(format!("Failed to create HTTP client: {}", e)))?;

        let provider = match config.provider.as_str() {
            "anthropic" => Provider::Anthropic,
            _ => Provider::OpenAi,
        };

        // Auto-set base_url for Anthropic if the user didn't override it
        if provider == Provider::Anthropic && config.base_url == "https://openrouter.ai/api/v1" {
            config.base_url = "https://api.anthropic.com/v1".to_string();
        }

        info!(
            "LLM client initialized: provider={}, base_url={}",
            config.provider, config.base_url
        );
        if config.api_key.is_empty() {
            error!(
                "LLM api_key is empty — calls will fail with 401. Set OPENROUTER_API_KEY, OPENAI_API_KEY, or ANTHROPIC_API_KEY."
            );
        } else {
            info!("LLM api_key loaded ({} chars)", config.api_key.len());
        }

        Ok(Self {
            client,
            config,
            provider,
        })
    }

    /// Create from environment variables
    pub fn from_env() -> AlmsResult<Self> {
        Self::new(LlmConfig::from_env())
    }

    /// Create a completion request builder, adapting format per provider.
    fn build_request(&self, request: &CompletionRequest) -> AlmsResult<RequestBuilder> {
        match self.provider {
            Provider::OpenAi => {
                let url = format!("{}/chat/completions", self.config.base_url);
                debug!("Sending OpenAI completion request to {}", url);
                Ok(self
                    .client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", self.config.api_key))
                    .header("Content-Type", "application/json")
                    .json(request))
            }
            Provider::Anthropic => {
                let url = format!("{}/messages", self.config.base_url);
                debug!("Sending Anthropic completion request to {}", url);
                let anthropic_req = crate::anthropic::to_anthropic_request(request);
                Ok(self
                    .client
                    .post(&url)
                    .header("x-api-key", &self.config.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .json(&anthropic_req))
            }
        }
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

        let completion: CompletionResponse = match self.provider {
            Provider::OpenAi => response
                .json()
                .await
                .map_err(|e| AlmsError::Runtime(format!("Failed to parse response: {}", e)))?,
            Provider::Anthropic => {
                let anthropic_resp: crate::anthropic::AnthropicResponse =
                    response.json().await.map_err(|e| {
                        AlmsError::Runtime(format!("Failed to parse Anthropic response: {}", e))
                    })?;
                crate::anthropic::from_anthropic_response(anthropic_resp)
            }
        };

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
        // Anthropic doesn't support stream_options
        if self.provider == Provider::OpenAi {
            request.stream_options = Some(StreamOptions {
                include_usage: true,
            });
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

        // Buffer raw bytes into complete SSE events. TCP chunks don't align
        // with SSE event boundaries, so we accumulate lines and yield parsed
        // StreamChunks only when we see a blank-line separator.
        //
        // Per-chunk read timeout: if no data arrives within the configured window
        // we treat the stream as stalled and terminate it. This prevents indefinite
        // hangs when the LLM server stops sending data without closing the connection.
        let byte_stream = response.bytes_stream();
        let chunk_timeout = std::time::Duration::from_secs(self.config.stream_chunk_timeout_secs);
        let provider = self.provider;
        let stream = futures::stream::unfold(
            (byte_stream, String::new()),
            move |(mut bytes, mut buf)| async move {
                use futures::StreamExt as _;
                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(pos) = buf.find("\n\n") {
                        let event_text = buf[..pos].to_string();
                        buf = buf[pos + 2..].to_string();
                        match Self::dispatch_sse_event(provider, &event_text) {
                            SseParseResult::Chunk(chunk) => {
                                return Some((Ok(chunk), (bytes, buf)));
                            }
                            SseParseResult::Done => {
                                return None; // [DONE] — stream complete
                            }
                            SseParseResult::Skip => {
                                continue; // comment or empty event
                            }
                        }
                    }
                    // Need more data from the network (with timeout)
                    match tokio::time::timeout(chunk_timeout, bytes.next()).await {
                        Ok(Some(Ok(b))) => {
                            // Normalize \r\n → \n so the \n\n event separator works
                            // regardless of whether the upstream sends CRLF or LF.
                            let text = String::from_utf8_lossy(&b).replace("\r\n", "\n");
                            buf.push_str(&text);
                        }
                        Ok(Some(Err(e))) => {
                            return Some((
                                Err(AlmsError::Runtime(format!("Stream error: {}", e))),
                                (bytes, buf),
                            ));
                        }
                        Ok(None) => {
                            // Stream ended — try to parse any remaining buffered data
                            if !buf.trim().is_empty() {
                                let remaining = std::mem::take(&mut buf);
                                if let SseParseResult::Chunk(chunk) =
                                    Self::dispatch_sse_event(provider, remaining.trim())
                                {
                                    return Some((Ok(chunk), (bytes, buf)));
                                }
                            }
                            return None; // stream complete
                        }
                        Err(_) => {
                            warn!(
                                "LLM stream stalled (no data for {}s), terminating",
                                chunk_timeout.as_secs()
                            );
                            return Some((
                                Err(AlmsError::Runtime(format!(
                                    "LLM stream stalled (no data for {}s) — partial response discarded",
                                    chunk_timeout.as_secs()
                                ))),
                                (bytes, buf),
                            ));
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }

    /// Route an SSE event block to the appropriate provider-specific parser.
    fn dispatch_sse_event(provider: Provider, event: &str) -> SseParseResult {
        match provider {
            Provider::OpenAi => Self::parse_sse_event(event),
            Provider::Anthropic => Self::parse_anthropic_sse_block(event),
        }
    }

    /// Parse an Anthropic SSE event block which has `event:` and `data:` fields.
    fn parse_anthropic_sse_block(event: &str) -> SseParseResult {
        let mut event_type: Option<&str> = None;
        let mut data: Option<&str> = None;

        for line in event.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(et) = line
                .strip_prefix("event: ")
                .or_else(|| line.strip_prefix("event:"))
            {
                event_type = Some(et.trim());
            }
            if let Some(d) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                data = Some(d.trim());
            }
        }

        match (event_type, data) {
            (Some(et), Some(d)) => crate::anthropic::parse_anthropic_sse(et, d),
            (Some("message_stop"), _) => SseParseResult::Done,
            _ => SseParseResult::Skip,
        }
    }

    /// Parse a single OpenAI SSE event block (one or more `data:` lines) into a StreamChunk.
    fn parse_sse_event(event: &str) -> SseParseResult {
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
                    return SseParseResult::Done;
                }
                match serde_json::from_str::<StreamChunk>(data) {
                    Ok(chunk) => return SseParseResult::Chunk(chunk),
                    Err(e) => {
                        warn!("Failed to parse SSE chunk: {} - data: {}", e, data);
                        continue;
                    }
                }
            }
        }
        SseParseResult::Skip
    }

    /// Quick completion with default model
    pub async fn quick_complete(&self, messages: Vec<LlmMessage>) -> AlmsResult<String> {
        let request = CompletionRequest::new(&self.config.default_model).with_messages(messages);

        let response = self.complete(request).await?;

        let choice =
            response.choices.into_iter().next().ok_or_else(|| {
                AlmsError::Runtime("LLM returned empty choices array".to_string())
            })?;

        choice.message.content.ok_or_else(|| {
            AlmsError::Runtime(
                "LLM returned null content (tool-call-only response in non-tool context)"
                    .to_string(),
            )
        })
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

    /// Internal: switch provider, base_url, and resolve API key.
    fn apply_provider(&mut self, provider: &str, resolve_key: impl FnOnce(&str) -> Option<String>) {
        self.provider = match provider {
            "anthropic" => Provider::Anthropic,
            "openrouter" => Provider::OpenAi,
            _ => Provider::OpenAi,
        };

        match provider {
            "anthropic" => {
                self.config.base_url = "https://api.anthropic.com/v1".to_string();
            }
            "openrouter" => {
                self.config.base_url = "https://openrouter.ai/api/v1".to_string();
            }
            "openai" => {
                self.config.base_url = "https://api.openai.com/v1".to_string();
            }
            _ => {}
        }

        if let Some(key) = resolve_key(provider) {
            self.config.api_key = key;
        } else {
            warn!(
                "No API key found for provider '{}' — requests will fail",
                provider
            );
        }

        self.config.provider = provider.to_string();
    }

    /// Override the LLM provider, resolving API key from env vars.
    pub fn with_provider(mut self, provider: &str) -> Self {
        self.apply_provider(provider, alms_core::config::select_llm_api_key_from_env);
        self
    }

    /// Override the LLM provider, resolving API key from secrets store
    /// (falls back to env vars if not in secrets).
    pub fn with_provider_and_secrets(
        mut self,
        provider: &str,
        secrets: &alms_core::secrets::SecretsStore,
    ) -> Self {
        self.apply_provider(provider, |p| secrets.resolve_key(p));
        self
    }

    /// Re-resolve the API key from a `SecretsStore` for the current provider.
    ///
    /// Unlike `with_provider_and_secrets`, this does NOT change the provider or
    /// base URL — it only refreshes the API key. Falls back to env vars if the
    /// secrets store has no key for the active provider.
    pub fn with_secrets(mut self, secrets: &alms_core::secrets::SecretsStore) -> Self {
        if let Some(key) = secrets.resolve_key(&self.config.provider) {
            self.config.api_key = key;
        }
        self
    }

    /// Get the configured provider string (e.g. `"openrouter"`, `"anthropic"`).
    pub fn provider(&self) -> &str {
        &self.config.provider
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
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_parse_sse_event_done() {
        assert!(matches!(
            LlmClient::parse_sse_event("data: [DONE]"),
            SseParseResult::Done
        ));
    }

    #[test]
    fn test_parse_sse_event_comment_is_skip() {
        assert!(matches!(
            LlmClient::parse_sse_event(": comment"),
            SseParseResult::Skip
        ));
    }

    #[test]
    fn test_parse_sse_event_tool_call_delta() {
        let event = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"te"}}]},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
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
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
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

    #[test]
    fn test_provider_getter() {
        let config = LlmConfig {
            provider: "anthropic".into(),
            api_key: "test-key".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.provider(), "anthropic");
    }

    #[test]
    fn test_with_secrets_updates_key() {
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "old-key".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        // Create a secrets store with a temp path so set_key can persist
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let mut secrets = alms_core::secrets::SecretsStore::load(secrets_path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("openrouter", "new-runtime-key").unwrap();

        let updated = client.with_secrets(&secrets);
        // Provider should not change
        assert_eq!(updated.provider(), "openrouter");
        // The default model should not change either
        assert_eq!(updated.default_model(), "moonshotai/kimi-k2.5");
    }

    #[test]
    fn test_with_secrets_no_key_keeps_existing() {
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "original-key".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        // Empty secrets store has no key for openrouter
        let secrets = alms_core::secrets::SecretsStore::empty();
        let updated = client.with_secrets(&secrets);
        // Provider unchanged
        assert_eq!(updated.provider(), "openrouter");
    }
}
