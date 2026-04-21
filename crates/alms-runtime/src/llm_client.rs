use crate::llm_types::*;
use alms_core::config::{AuthScheme, ProviderKind};
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
    Gemini,
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

        let provider = Self::resolve_protocol(&config);

        // Auto-set base_url for native adapters if the user didn't override
        // it (legacy users who only set `provider = "anthropic"` or
        // `"gemini"` in a classic flat config and never touched `base_url`).
        if provider == Provider::Anthropic && config.base_url == "https://openrouter.ai/api/v1" {
            config.base_url = "https://api.anthropic.com/v1".to_string();
        }
        if provider == Provider::Gemini && config.base_url == "https://openrouter.ai/api/v1" {
            config.base_url = "https://generativelanguage.googleapis.com/v1beta".to_string();
        }

        info!(
            "LLM client initialized: provider={}, base_url={}",
            config.provider, config.base_url
        );
        if config.api_key.is_empty() {
            warn!(
                "LLM api_key is empty at construction — will be resolved from secrets later. \
                 If this persists, run `alms auth set <provider> <key>` or enable mock mode."
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

    /// Determine the wire-protocol family for the current provider.
    ///
    /// Checks the `providers` table first (populated from
    /// `[llm.providers.<name>]` + auto-injected sugar entries). Falls
    /// back to the classic hardcoded sugar-name check so that call sites
    /// constructing a bare `LlmConfig` without a providers map (tests,
    /// embedded callers) continue to work.
    fn resolve_protocol(config: &LlmConfig) -> Provider {
        if let Some(entry) = config.providers.get(&config.provider) {
            return match entry.kind {
                ProviderKind::Anthropic => Provider::Anthropic,
                ProviderKind::Gemini => Provider::Gemini,
                ProviderKind::OpenAiCompatible => Provider::OpenAi,
            };
        }
        match config.provider.as_str() {
            "anthropic" => Provider::Anthropic,
            "gemini" => Provider::Gemini,
            _ => Provider::OpenAi,
        }
    }

    /// Create a completion request builder, adapting format per provider.
    ///
    /// Applies any configured [`alms_core::config::ProviderQuirks`] to the
    /// outgoing request body before serialization, and attaches the API key
    /// according to the configured [`alms_core::config::AuthScheme`].
    fn build_request(&self, request: &CompletionRequest) -> AlmsResult<RequestBuilder> {
        match self.provider {
            Provider::OpenAi => {
                let url = format!("{}/chat/completions", self.config.base_url);
                debug!("Sending OpenAI completion request to {}", url);

                // Apply quirks in a provider-agnostic way. Mutating a local
                // copy of the request leaves the caller's input untouched.
                let mut req_body = request.clone();
                apply_quirks(&mut req_body, &self.config.quirks);

                // Gate `reasoning_effort` (#768): the param is only valid
                // for OpenAI-compat reasoning models that actually accept
                // it. Strip the field for:
                //   - DeepSeek R1 endpoints (`deepseek-reasoner` reasons
                //     automatically and rejects the param).
                //   - Non-reasoning OpenAI models (gpt-4o etc. return 400
                //     on unknown params).
                // See `is_openai_reasoning_model` for the exact heuristic
                // (model-name + base-URL based).
                if req_body.reasoning_effort.is_some()
                    && !is_openai_reasoning_model(&req_body.model, &self.config.base_url)
                {
                    req_body.reasoning_effort = None;
                }

                let builder = self
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&req_body);
                Ok(apply_auth(
                    builder,
                    &self.config.auth_scheme,
                    &self.config.api_key,
                ))
            }
            Provider::Anthropic => {
                let url = format!("{}/messages", self.config.base_url);
                debug!("Sending Anthropic completion request to {}", url);
                let anthropic_req = crate::anthropic::to_anthropic_request(request);
                let builder = self
                    .client
                    .post(&url)
                    .header("anthropic-version", "2023-06-01")
                    .header("Content-Type", "application/json")
                    .json(&anthropic_req);
                Ok(apply_auth(
                    builder,
                    &self.config.auth_scheme,
                    &self.config.api_key,
                ))
            }
            Provider::Gemini => {
                // Gemini routes on URL path, not a role-based endpoint:
                //   {base_url}/models/{model}:generateContent
                //   {base_url}/models/{model}:streamGenerateContent?alt=sse
                // `request.stream` is set by `complete_stream` before this
                // is called, so we branch on it here.
                let streaming = request.stream.unwrap_or(false);
                let method = if streaming {
                    "streamGenerateContent?alt=sse"
                } else {
                    "generateContent"
                };
                let url = format!(
                    "{}/models/{}:{}",
                    self.config.base_url, request.model, method
                );
                debug!("Sending Gemini completion request to {}", url);
                let gemini_req = crate::gemini::to_gemini_request(request);
                let builder = self
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .json(&gemini_req);
                Ok(apply_auth(
                    builder,
                    &self.config.auth_scheme,
                    &self.config.api_key,
                ))
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

        // Read the raw body first so we can log it for diagnostics, then
        // parse from the text.  This is essential for debugging models that
        // return content in unexpected fields (e.g. `reasoning_content`).
        let body_text = response
            .text()
            .await
            .map_err(|e| AlmsError::Runtime(format!("Failed to read response body: {}", e)))?;

        debug!(raw_body_len = body_text.len(), "LLM response body received");

        let completion: CompletionResponse = match self.provider {
            Provider::OpenAi => serde_json::from_str(&body_text).map_err(|e| {
                error!(body = body_text.as_str(), "Failed to parse OpenAI response");
                AlmsError::Runtime(format!("Failed to parse response: {}", e))
            })?,
            Provider::Anthropic => {
                let anthropic_resp: crate::anthropic::AnthropicResponse =
                    serde_json::from_str(&body_text).map_err(|e| {
                        error!(
                            body = body_text.as_str(),
                            "Failed to parse Anthropic response"
                        );
                        AlmsError::Runtime(format!("Failed to parse Anthropic response: {}", e))
                    })?;
                crate::anthropic::from_anthropic_response(anthropic_resp)
            }
            Provider::Gemini => {
                let gemini_resp: crate::gemini::GeminiResponse = serde_json::from_str(&body_text)
                    .map_err(|e| {
                    error!(body = body_text.as_str(), "Failed to parse Gemini response");
                    AlmsError::Runtime(format!("Failed to parse Gemini response: {}", e))
                })?;
                crate::gemini::from_gemini_response(gemini_resp)
            }
        };

        if let Some(usage) = &completion.usage {
            debug!(
                "Completion used {} prompt + {} completion = {} total tokens",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
            );
        }

        // Log when content is null but there are completion tokens -- a strong
        // signal that the model returned content in an unexpected field (e.g.
        // `reasoning_content` for reasoning models).
        if let Some(choice) = completion.choices.first()
            && choice.message.content.is_none()
        {
            let has_reasoning = choice.message.reasoning_content.is_some();
            let has_tool_calls = choice.message.tool_calls.is_some();
            let comp_tokens = completion
                .usage
                .as_ref()
                .map(|u| u.completion_tokens)
                .unwrap_or(0);
            if comp_tokens > 0 {
                warn!(
                    comp_tokens,
                    has_reasoning,
                    has_tool_calls,
                    "LLM returned null content with non-zero completion tokens"
                );
            }
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
            Provider::Gemini => Self::parse_gemini_sse_block(event),
        }
    }

    /// Parse a single Gemini SSE event block. Gemini's
    /// `streamGenerateContent?alt=sse` uses OpenAI-style `data: {json}`
    /// events (no typed `event:` header), so we walk the block for the
    /// `data:` line and hand it to the provider parser.
    fn parse_gemini_sse_block(event: &str) -> SseParseResult {
        for line in event.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                return crate::gemini::parse_gemini_sse(data.trim());
            }
        }
        SseParseResult::Skip
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
                reasoning_tokens: None,
                completion_tokens_details: None,
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
                        reasoning_content: None,
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
                        reasoning_tokens: None,
                        completion_tokens_details: None,
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
    ///
    /// When switching to a new provider and no key is found, the existing
    /// (wrong-provider) key is cleared to prevent silent 401 errors from
    /// sending one provider's key to another.
    ///
    /// The provider's `base_url` / `auth_scheme` / `quirks` are sourced
    /// from the `providers` map if an entry is present (the normal path
    /// for configs loaded via `AlmsConfig::load`), falling back to the
    /// hardcoded sugar-name defaults otherwise (used by standalone tests
    /// that construct a bare `LlmConfig`).
    fn apply_provider(&mut self, provider: &str, resolve_key: impl FnOnce(&str) -> Option<String>) {
        let old_provider = self.config.provider.clone();

        if let Some(entry) = self.config.providers.get(provider).cloned() {
            self.provider = match entry.kind {
                ProviderKind::Anthropic => Provider::Anthropic,
                ProviderKind::Gemini => Provider::Gemini,
                ProviderKind::OpenAiCompatible => Provider::OpenAi,
            };
            self.config.base_url = entry.base_url;
            self.config.auth_scheme = entry.auth_scheme;
            self.config.quirks = entry.quirks;
            if let Some(model) = entry.model {
                self.config.default_model = model;
            }
        } else {
            // Fall back to sugar-name mapping for tests / bare configs.
            self.provider = match provider {
                "anthropic" => Provider::Anthropic,
                "gemini" => Provider::Gemini,
                _ => Provider::OpenAi,
            };
            match provider {
                "anthropic" => {
                    self.config.base_url = "https://api.anthropic.com/v1".to_string();
                    self.config.auth_scheme = AuthScheme::Header {
                        name: "x-api-key".into(),
                    };
                }
                "gemini" => {
                    self.config.base_url =
                        "https://generativelanguage.googleapis.com/v1beta".to_string();
                    self.config.auth_scheme = AuthScheme::Header {
                        name: "x-goog-api-key".into(),
                    };
                }
                "openrouter" => {
                    self.config.base_url = "https://openrouter.ai/api/v1".to_string();
                    self.config.auth_scheme = AuthScheme::Bearer;
                }
                "openai" => {
                    self.config.base_url = "https://api.openai.com/v1".to_string();
                    self.config.auth_scheme = AuthScheme::Bearer;
                }
                _ => {}
            }
            self.config.quirks = alms_core::config::ProviderQuirks::default();
        }

        if let Some(key) = resolve_key(provider) {
            self.config.api_key = key;
        } else if old_provider != provider && !self.config.api_key.is_empty() {
            // Switching providers but no key found for the new one.
            // Clear the old key to avoid sending the wrong provider's
            // credentials (which would cause confusing 401 errors).
            warn!(
                old_provider = %old_provider,
                new_provider = %provider,
                "Provider changed but no API key found — clearing stale key"
            );
            self.config.api_key.clear();
        } else if self.config.api_key.is_empty() {
            warn!(
                "No API key found for provider '{}' — requests will fail",
                provider
            );
        }

        self.config.provider = provider.to_string();
    }

    /// Override the LLM provider without resolving a new API key.
    ///
    /// The existing API key is preserved. Callers should prefer
    /// `with_provider_and_secrets` when a secrets store is available.
    pub fn with_provider(mut self, provider: &str) -> Self {
        // No key resolver — keeps the existing key. Callers should prefer
        // `with_provider_and_secrets` when a secrets store is available.
        self.apply_provider(provider, |_| None);
        self
    }

    /// Override the LLM provider, resolving API key from secrets store.
    ///
    /// Key resolution consults the secrets store first, then the
    /// `[llm.providers.<provider>]` entry's `api_key_env` / `api_key`
    /// fields as a fallback. This lets configs declare an env-var-backed
    /// key inline without also running `alms auth set`.
    pub fn with_provider_and_secrets(
        mut self,
        provider: &str,
        secrets: &alms_core::secrets::SecretsStore,
    ) -> Self {
        let entry_key = self
            .config
            .providers
            .get(provider)
            .and_then(|e| e.resolve_api_key());
        self.apply_provider(provider, |p| secrets.resolve_key(p).or(entry_key));
        self
    }

    /// Re-resolve the API key from a `SecretsStore` for the current provider.
    ///
    /// Unlike `with_provider_and_secrets`, this does NOT change the provider or
    /// base URL — it only refreshes the API key from the secrets store.
    pub fn with_secrets(mut self, secrets: &alms_core::secrets::SecretsStore) -> Self {
        if let Some(key) = secrets.resolve_key(&self.config.provider) {
            debug!(
                provider = %self.config.provider,
                key_len = key.len(),
                "API key resolved from secrets store"
            );
            self.config.api_key = key;
        } else {
            debug!(
                provider = %self.config.provider,
                existing_key_empty = self.config.api_key.is_empty(),
                "No key found in secrets store for provider, keeping existing"
            );
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

    /// Get the current API key (test-only).
    #[cfg(test)]
    pub fn api_key(&self) -> &str {
        &self.config.api_key
    }

    /// Get the current base URL (test-only).
    #[cfg(test)]
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// Build a request and return the finalized `reqwest::Request` (test-only).
    ///
    /// Exposed so tests can assert the end-to-end effect of the configured
    /// `auth_scheme` and `quirks` on the outgoing HTTP request — headers,
    /// URL, and serialized body — without dispatching it to a live upstream.
    #[cfg(test)]
    pub(crate) fn build_request_for_test(
        &self,
        request: &CompletionRequest,
    ) -> AlmsResult<reqwest::Request> {
        let builder = self.build_request(request)?;
        builder
            .build()
            .map_err(|e| AlmsError::Runtime(format!("build request: {e}")))
    }
}

/// Attach an API key to a request according to the configured auth scheme.
///
/// Returns the builder unmodified when the key is empty — callers will see
/// a 401 from the upstream, which is the right signal. Silently adding an
/// empty header would make missing-key errors harder to diagnose.
pub(crate) fn apply_auth(
    builder: RequestBuilder,
    scheme: &AuthScheme,
    api_key: &str,
) -> RequestBuilder {
    if api_key.is_empty() {
        return builder;
    }
    match scheme {
        AuthScheme::Bearer => builder.header("Authorization", format!("Bearer {api_key}")),
        AuthScheme::Header { name } => builder.header(name.as_str(), api_key),
    }
}

/// Apply [`alms_core::config::ProviderQuirks`] to an OpenAI-format request.
///
/// See the individual `quirks.*` fields for semantics. Called from
/// `build_request` just before serialization; writes through `&mut` so the
/// caller can compose multiple request transforms without cloning for each.
pub(crate) fn apply_quirks(
    request: &mut CompletionRequest,
    quirks: &alms_core::config::ProviderQuirks,
) {
    // Order matters: `drop_empty_content` runs first so it only sees the
    // user's original history. `tool_gap_fill` then deliberately inserts
    // empty-user separators between consecutive tool results — a shape that
    // `drop_empty_content` would have stripped if it ran afterwards. The
    // two quirks compose cleanly only in this order.
    if quirks.drop_empty_content {
        request.messages.retain(|m| {
            let has_content = m.content.as_deref().is_some_and(|c| !c.is_empty());
            let has_tool_calls = m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());
            let is_tool_result = m.role == "tool";
            has_content || has_tool_calls || is_tool_result
        });
    }

    if quirks.tool_gap_fill && request.messages.len() > 1 {
        // Walk from back to front inserting an empty user turn between two
        // consecutive `tool` messages. Back-to-front avoids re-scanning
        // indexes we've already visited.
        let mut i = request.messages.len() - 1;
        while i > 0 {
            if request.messages[i].role == "tool" && request.messages[i - 1].role == "tool" {
                request.messages.insert(i, LlmMessage::user(""));
            }
            i -= 1;
        }
    }
}

/// Returns `true` if the given model+base_url combination accepts the
/// OpenAI-compat `reasoning_effort` request field (#768).
///
/// Detection is model-name + base-URL based:
/// - DeepSeek endpoints (base URL contains `deepseek`) always return
///   `false` — `deepseek-reasoner` reasons automatically and rejects
///   the param.
/// - OpenAI reasoning models: the `o1`/`o3`/`o4-mini`/`o5` o-series
///   families and the `gpt-5` family all accept the param. Detection
///   checks for model name prefixes like `o1-`, `o3-`, `o4-mini`,
///   `gpt-5`, case-insensitive and with an optional provider prefix
///   (e.g. OpenRouter uses `openai/o3-mini`, `openai/gpt-5`).
/// - xAI Grok reasoning variants: `grok-*-reasoning`, `grok-3-mini`,
///   `grok-4`. Detected by the `grok-` prefix combined with the
///   `-reasoning` suffix or an explicit reasoning tier name.
/// - Everything else (gpt-4o, claude-sonnet via proxy, non-reasoning
///   Grok, etc.) returns `false` so the field gets stripped.
///
/// The heuristic is intentionally conservative: false negatives (param
/// stripped for a model that would accept it) just disable the feature
/// for that model, while false positives (param sent to a model that
/// rejects it) produce a hard 400. If new reasoning models ship, extend
/// this list rather than widening the matcher.
pub(crate) fn is_openai_reasoning_model(model: &str, base_url: &str) -> bool {
    // DeepSeek strips `reasoning_effort` regardless of model name because
    // even if the user points a DeepSeek base URL at a non-reasoner model,
    // the param is still DeepSeek-incompatible.
    let base_lower = base_url.to_ascii_lowercase();
    if base_lower.contains("deepseek") {
        return false;
    }

    // Strip an optional `<provider>/` prefix (OpenRouter-style) before
    // model-name matching. The model name itself is case-insensitive.
    let model_lower = model.to_ascii_lowercase();
    let name = model_lower
        .rsplit_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(&model_lower);

    // OpenAI o-series / GPT-5 reasoning families.
    if name.starts_with("o1")
        || name.starts_with("o3")
        || name.starts_with("o4")
        || name.starts_with("o5")
        || name.starts_with("gpt-5")
    {
        return true;
    }

    // xAI Grok reasoning variants. The SKUs listed here accept the
    // `reasoning_effort` param per xAI's documentation; other Grok
    // models do not.
    if name.starts_with("grok-")
        && (name.contains("-reasoning")
            || name.starts_with("grok-3-mini")
            || name.starts_with("grok-4"))
    {
        return true;
    }

    false
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
        // The key must have been updated to the new value
        assert_eq!(updated.api_key(), "new-runtime-key");
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
        // Key must remain the original value when secrets store has nothing
        assert_eq!(updated.api_key(), "original-key");
    }

    #[test]
    fn test_with_provider_and_secrets_switches_provider() {
        // Start with an OpenAI client
        let config = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.provider(), "openai");
        assert_eq!(client.base_url(), "https://api.openai.com/v1");

        // Create a secrets store with an Anthropic key
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let mut secrets = alms_core::secrets::SecretsStore::load(secrets_path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-test-key").unwrap();

        // Switch to Anthropic via with_provider_and_secrets
        let switched = client.with_provider_and_secrets("anthropic", &secrets);
        assert_eq!(switched.provider(), "anthropic");
        assert_eq!(switched.base_url(), "https://api.anthropic.com/v1");
        assert_eq!(switched.api_key(), "sk-ant-test-key");
    }

    #[test]
    fn test_with_provider_and_secrets_switches_to_openrouter() {
        let config = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let mut secrets = alms_core::secrets::SecretsStore::load(secrets_path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("openrouter", "sk-or-test-key").unwrap();

        let switched = client.with_provider_and_secrets("openrouter", &secrets);
        assert_eq!(switched.provider(), "openrouter");
        assert_eq!(switched.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(switched.api_key(), "sk-or-test-key");
    }

    #[test]
    fn test_provider_switch_clears_stale_key_when_no_new_key() {
        // Start with an OpenRouter client that has a valid key
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "sk-or-valid-key".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.api_key(), "sk-or-valid-key");

        // Switch to Anthropic but secrets store has NO Anthropic key.
        // The old OpenRouter key must be cleared to prevent sending it
        // to the Anthropic API (which would cause a confusing 401).
        let secrets = alms_core::secrets::SecretsStore::empty();
        let switched = client.with_provider_and_secrets("anthropic", &secrets);
        assert_eq!(switched.provider(), "anthropic");
        assert_eq!(switched.base_url(), "https://api.anthropic.com/v1");
        // Key must be empty, not the old OpenRouter key
        assert_eq!(switched.api_key(), "");
    }

    #[test]
    fn test_same_provider_no_key_keeps_existing() {
        // When re-applying the SAME provider and no key is found,
        // the existing key should be preserved (not cleared).
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "sk-or-existing".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        let secrets = alms_core::secrets::SecretsStore::empty();
        let same = client.with_provider_and_secrets("openrouter", &secrets);
        assert_eq!(same.provider(), "openrouter");
        // Same provider, no new key found -- existing key preserved
        assert_eq!(same.api_key(), "sk-or-existing");
    }

    // ------------------------------------------------------------------
    // Generic OpenAI-compatible provider config (issue #765)
    // ------------------------------------------------------------------

    /// Inspect the `Authorization` header a `RequestBuilder` would send
    /// without actually dispatching a request.
    fn header_value(builder: reqwest::RequestBuilder, name: &str) -> Option<String> {
        let req = builder.build().expect("build request");
        req.headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[test]
    fn test_apply_auth_bearer() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let builder = apply_auth(builder, &AuthScheme::Bearer, "secret-key");
        assert_eq!(
            header_value(builder, "authorization"),
            Some("Bearer secret-key".to_string())
        );
    }

    #[test]
    fn test_apply_auth_custom_header() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let scheme = AuthScheme::Header {
            name: "x-api-key".into(),
        };
        let builder = apply_auth(builder, &scheme, "anthropic-key");
        let req = builder.build().unwrap();
        assert_eq!(
            req.headers().get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("anthropic-key")
        );
        // Bearer scheme should NOT have been applied.
        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn test_apply_auth_empty_key_skips_header() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let builder = apply_auth(builder, &AuthScheme::Bearer, "");
        // Empty key -> no header at all (caller will see a 401 upstream).
        assert!(header_value(builder, "authorization").is_none());
    }

    #[test]
    fn test_apply_quirks_drop_empty_content() {
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("hello"),
            LlmMessage {
                role: "assistant".into(),
                content: Some(String::new()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage::assistant("response"),
        ]);
        apply_quirks(&mut req, &quirks);
        // Empty assistant turn is dropped; the two real messages remain.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].content.as_deref(), Some("hello"));
        assert_eq!(req.messages[1].content.as_deref(), Some("response"));
    }

    #[test]
    fn test_apply_quirks_drop_empty_content_keeps_tool_calls() {
        // An assistant message with no content but present tool_calls must
        // NOT be dropped — the tool call IS the payload.
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let tc = ToolCall::new("call_1", "echo", r#"{"text":"hi"}"#);
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("call echo"),
            LlmMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![tc]),
                tool_call_id: None,
            },
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 2);
        assert!(req.messages[1].tool_calls.is_some());
    }

    #[test]
    fn test_apply_quirks_drop_empty_content_keeps_tool_results() {
        // Tool-result messages carry `role == "tool"` and must be kept
        // even though their content might legitimately be empty for
        // no-op tools.
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("go"),
            LlmMessage::tool_result("call_1", ""),
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[1].role, "tool");
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_between_consecutive_tools() {
        // Two back-to-back tool-results get split by an empty user turn.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do both"),
            LlmMessage::tool_result("call_1", "result 1"),
            LlmMessage::tool_result("call_2", "result 2"),
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 4);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[1].role, "tool");
        assert_eq!(req.messages[2].role, "user");
        assert_eq!(req.messages[2].content.as_deref(), Some(""));
        assert_eq!(req.messages[3].role, "tool");
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_noop_on_non_adjacent() {
        // A single tool-result isn't followed by another tool-result,
        // so no gap-fill insertion should happen.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do one"),
            LlmMessage::tool_result("call_1", "result 1"),
            LlmMessage::assistant("ok"),
        ]);
        let before = req.messages.len();
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_three_in_a_row() {
        // Three consecutive tool-results get two separators.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do three"),
            LlmMessage::tool_result("c1", "r1"),
            LlmMessage::tool_result("c2", "r2"),
            LlmMessage::tool_result("c3", "r3"),
        ]);
        apply_quirks(&mut req, &quirks);
        // user, tool, USER, tool, USER, tool  = 6 messages
        assert_eq!(req.messages.len(), 6);
        let roles: Vec<&str> = req.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "tool", "user", "tool", "user", "tool"]);
    }

    #[test]
    fn test_apply_quirks_default_is_noop() {
        let quirks = alms_core::config::ProviderQuirks::default();
        let messages = vec![
            LlmMessage::user("hello"),
            LlmMessage::tool_result("call_1", "ok"),
            LlmMessage::tool_result("call_2", "ok"),
        ];
        let mut req = CompletionRequest::new("test").with_messages(messages.clone());
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), messages.len());
    }

    #[test]
    fn test_generic_provider_entry_sets_base_url_and_auth() {
        // Simulate a config-loaded `AlmsConfig` with a user-declared xAI
        // entry and verify that the `From<alms_core::config::LlmConfig>`
        // impl flattens it into the runtime `LlmConfig` correctly.
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "xai".into();
        core_cfg.providers.insert(
            "xai".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.x.ai/v1".into(),
                api_key_env: None,
                api_key: Some("xai-literal".into()),
                model: Some("grok-4".into()),
                auth_scheme: AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks {
                    drop_empty_content: true,
                    ..Default::default()
                },
            },
        );

        let runtime_cfg: LlmConfig = core_cfg.into();
        assert_eq!(runtime_cfg.base_url, "https://api.x.ai/v1");
        assert_eq!(runtime_cfg.default_model, "grok-4");
        assert!(matches!(runtime_cfg.auth_scheme, AuthScheme::Bearer));
        assert!(runtime_cfg.quirks.drop_empty_content);
    }

    #[test]
    fn test_generic_provider_anthropic_kind_resolves_to_anthropic_protocol() {
        // A provider declared with kind=anthropic should build an Anthropic
        // LlmClient regardless of the provider name.
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "my-anthropic-proxy".into();
        core_cfg.providers.insert(
            "my-anthropic-proxy".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://proxy.example.com/v1".into(),
                api_key_env: None,
                api_key: Some("k".into()),
                model: None,
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );

        let runtime_cfg: LlmConfig = core_cfg.into();
        let client = LlmClient::new(runtime_cfg).unwrap();
        assert_eq!(client.base_url(), "https://proxy.example.com/v1");
        assert_eq!(client.provider(), "my-anthropic-proxy");
    }

    /// End-to-end: a TOML-style `AlmsConfig` with a custom `auth_scheme` and
    /// `quirks` round-trips through `From<core::LlmConfig>` → `LlmClient` →
    /// `build_request` and produces the correct URL, headers, and body. Closes
    /// the gap flagged in PR #770 review where `build_request` was only
    /// indirectly covered.
    #[test]
    fn test_build_request_honours_auth_scheme_and_quirks_end_to_end() {
        // Custom header + drop_empty_content + tool_gap_fill — exercises
        // both transform paths and the non-Bearer auth path simultaneously.
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "customlab".into();
        core_cfg.providers.insert(
            "customlab".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://customlab.example.com/v1".into(),
                api_key_env: None,
                api_key: Some("inline-secret".into()),
                model: Some("lab-7b".into()),
                auth_scheme: AuthScheme::Header {
                    name: "X-Lab-Key".into(),
                },
                quirks: alms_core::config::ProviderQuirks {
                    tool_gap_fill: true,
                    drop_empty_content: true,
                },
            },
        );

        let runtime_cfg: LlmConfig = core_cfg.into();
        // api_key on the flat config is unset; the provider entry's inline
        // key should flow through via `resolve_api_key`, but that's the
        // gateway's job. For this test, populate directly so we can assert
        // the finalized header.
        let mut runtime_cfg = runtime_cfg;
        runtime_cfg.api_key = "inline-secret".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let request = CompletionRequest::new("lab-7b").with_messages(vec![
            LlmMessage::user("hi"),
            // Two back-to-back tool results exercise `tool_gap_fill`.
            LlmMessage::tool_result("t1", "r1"),
            LlmMessage::tool_result("t2", "r2"),
            // Empty assistant turn exercises `drop_empty_content`.
            LlmMessage {
                role: "assistant".into(),
                content: Some(String::new()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ]);
        let req = client.build_request_for_test(&request).unwrap();

        // URL is sourced from the provider entry's base_url.
        assert_eq!(
            req.url().as_str(),
            "https://customlab.example.com/v1/chat/completions"
        );

        // Custom header auth scheme -- no Authorization header, custom header
        // present with the raw key.
        assert!(
            req.headers().get("authorization").is_none(),
            "bearer header must not be set when auth_scheme is Header"
        );
        assert_eq!(
            req.headers().get("X-Lab-Key").and_then(|v| v.to_str().ok()),
            Some("inline-secret"),
            "custom header must carry the resolved api key"
        );

        // Inspect the serialized body to confirm the quirks fired.
        let body_bytes = req
            .body()
            .and_then(|b| b.as_bytes())
            .expect("body bytes present");
        let body: serde_json::Value = serde_json::from_slice(body_bytes).unwrap();
        let messages = body["messages"].as_array().expect("messages array");
        let roles: Vec<&str> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        // Expected after quirks run:
        //   drop_empty_content removes the empty-content assistant turn,
        //   then tool_gap_fill inserts an empty user between the two tool
        //   results. Final shape: user, tool, user, tool.
        assert_eq!(
            roles,
            vec!["user", "tool", "user", "tool"],
            "quirks did not produce the expected message shape: {roles:?}"
        );
    }

    // ------------------------------------------------------------------
    // Gemini native adapter (issue #764)
    // ------------------------------------------------------------------

    /// `ProviderKind::Gemini` resolves to `Provider::Gemini`, and the
    /// sugar entry's `x-goog-api-key` header travels through to the final
    /// request.
    #[test]
    fn test_gemini_provider_kind_resolves_to_gemini_protocol() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();

        let runtime_cfg: LlmConfig = core_cfg.into();
        let client = LlmClient::new(runtime_cfg).unwrap();
        assert_eq!(client.provider(), "gemini");
        assert_eq!(
            client.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    /// End-to-end: a Gemini config builds a request with the correct URL
    /// path (`:generateContent` for non-streaming), `x-goog-api-key`
    /// header, and Gemini-shaped body (`contents[]` with `systemInstruction`).
    #[test]
    fn test_gemini_build_request_non_streaming() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();

        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "gemini-test-key".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let request = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::system("be helpful"),
            LlmMessage::user("hi"),
        ]);
        let req = client.build_request_for_test(&request).unwrap();

        // URL: {base_url}/models/{model}:generateContent — the non-streaming
        // variant (request.stream is None).
        assert_eq!(
            req.url().as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );

        // Auth: `x-goog-api-key` header only, no Bearer.
        assert!(
            req.headers().get("authorization").is_none(),
            "Bearer header must not be set when auth_scheme is Header"
        );
        assert_eq!(
            req.headers()
                .get("x-goog-api-key")
                .and_then(|v| v.to_str().ok()),
            Some("gemini-test-key"),
        );

        // Body: Gemini wire shape — `systemInstruction` pulled out,
        // `contents[]` with role=user.
        let body_bytes = req.body().and_then(|b| b.as_bytes()).unwrap();
        let body: serde_json::Value = serde_json::from_slice(body_bytes).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
        // OpenAI-style `messages` array must not appear.
        assert!(
            body.get("messages").is_none(),
            "Gemini wire format must not emit an OpenAI `messages` array"
        );
    }

    /// Streaming branch builds the correct `:streamGenerateContent?alt=sse`
    /// URL. Verified by forcing `stream = Some(true)` on the request (what
    /// `complete_stream` does internally).
    #[test]
    fn test_gemini_build_request_streaming_url() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();

        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "k".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let mut request =
            CompletionRequest::new("gemini-2.5-flash").with_messages(vec![LlmMessage::user("hi")]);
        request.stream = Some(true);
        let req = client.build_request_for_test(&request).unwrap();

        assert_eq!(
            req.url().as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    /// `dispatch_sse_event` routes Gemini SSE blocks through the Gemini
    /// parser. Verifies the dispatch seam rather than re-covering the
    /// parser, which has direct tests in `gemini.rs`.
    #[test]
    fn test_dispatch_sse_event_routes_gemini() {
        let event = "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]}}]}";
        let result = LlmClient::parse_gemini_sse_block(event);
        match result {
            SseParseResult::Chunk(chunk) => {
                assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
            }
            _ => panic!("expected Chunk"),
        }
    }

    // ------------------------------------------------------------------
    // OpenAI-compat reasoning models (issue #768)
    // ------------------------------------------------------------------

    /// Extract the serialized request body from a built request.
    fn body_json(req: &reqwest::Request) -> serde_json::Value {
        let bytes = req.body().and_then(|b| b.as_bytes()).expect("body present");
        serde_json::from_slice(bytes).expect("valid JSON body")
    }

    /// Helper — build a canonical OpenAI client pointed at the default
    /// `https://api.openai.com/v1` base URL (so the DeepSeek strip branch
    /// stays out of the picture).
    fn openai_client() -> LlmClient {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "openai".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "openai-test-key".into();
        LlmClient::new(runtime_cfg).unwrap()
    }

    /// `reasoning_effort = "high"` produces `"reasoning_effort":"high"` on
    /// the OpenAI wire for a reasoning-capable model.
    #[test]
    fn test_openai_reasoning_effort_serialized_for_o3() {
        let client = openai_client();
        let request = CompletionRequest::new("o3-mini")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_reasoning_effort("high");
        let req = client.build_request_for_test(&request).unwrap();
        let body = body_json(&req);
        assert_eq!(body["reasoning_effort"], "high");
    }

    /// Every supported value (`low` / `medium` / `high` / `minimal`)
    /// survives a round-trip through the OpenAI adapter.
    #[test]
    fn test_openai_reasoning_effort_all_values() {
        let client = openai_client();
        for value in &["low", "medium", "high", "minimal"] {
            let request = CompletionRequest::new("gpt-5-preview")
                .with_messages(vec![LlmMessage::user("hi")])
                .with_reasoning_effort(*value);
            let req = client.build_request_for_test(&request).unwrap();
            let body = body_json(&req);
            assert_eq!(
                body["reasoning_effort"], *value,
                "reasoning_effort={value} did not round-trip"
            );
        }
    }

    /// `reasoning_effort = None` omits the field entirely from the wire
    /// body — preserves existing behaviour for runs that don't opt in.
    #[test]
    fn test_openai_reasoning_effort_none_omits_field() {
        let client = openai_client();
        let request = CompletionRequest::new("gpt-4o").with_messages(vec![LlmMessage::user("hi")]);
        let req = client.build_request_for_test(&request).unwrap();
        let body = body_json(&req);
        assert!(
            body.get("reasoning_effort").is_none(),
            "reasoning_effort must not appear when unset"
        );
    }

    /// `reasoning_effort` sent on a non-reasoning OpenAI model (gpt-4o)
    /// is stripped before hitting the wire — gpt-4o returns 400 on the
    /// unknown param.
    #[test]
    fn test_openai_reasoning_effort_stripped_for_non_reasoning_model() {
        let client = openai_client();
        let request = CompletionRequest::new("gpt-4o")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_reasoning_effort("medium");
        let req = client.build_request_for_test(&request).unwrap();
        let body = body_json(&req);
        assert!(
            body.get("reasoning_effort").is_none(),
            "reasoning_effort must be stripped for non-reasoning models (gpt-4o), got body: {body}"
        );
    }

    /// DeepSeek R1 endpoints reject `reasoning_effort` (reasoning is
    /// implicit for `deepseek-reasoner`). Detected via the base URL.
    #[test]
    fn test_openai_reasoning_effort_stripped_for_deepseek_base_url() {
        // Build a client pointed at a DeepSeek-shaped base URL via a
        // custom provider entry (the generic config path), so we don't
        // need a built-in sugar entry.
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "deepseek".into();
        core_cfg.providers.insert(
            "deepseek".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.deepseek.com/v1".into(),
                api_key_env: None,
                api_key: Some("ds-key".into()),
                model: Some("deepseek-reasoner".into()),
                auth_scheme: AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "ds-key".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let request = CompletionRequest::new("deepseek-reasoner")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_reasoning_effort("high");
        let req = client.build_request_for_test(&request).unwrap();
        let body = body_json(&req);
        assert!(
            body.get("reasoning_effort").is_none(),
            "reasoning_effort must be stripped for DeepSeek endpoints, got body: {body}"
        );
    }

    /// `is_openai_reasoning_model` recognises the families listed in
    /// the issue body and rejects everything else.
    #[test]
    fn test_is_openai_reasoning_model_matches_known_families() {
        // OpenAI o-series / GPT-5 — accept.
        let base = "https://api.openai.com/v1";
        for m in &[
            "o1",
            "o1-mini",
            "o1-preview",
            "o3",
            "o3-mini",
            "o4-mini",
            "o5",
            "gpt-5",
            "gpt-5-preview",
            "openai/o3-mini", // OpenRouter-style prefix
            "openai/gpt-5",
            "O3-Mini", // case-insensitive
        ] {
            assert!(
                is_openai_reasoning_model(m, base),
                "'{m}' should be recognised as an OpenAI reasoning model"
            );
        }
        // Non-reasoning OpenAI models.
        for m in &["gpt-4o", "gpt-4", "gpt-3.5-turbo", "openai/gpt-4o"] {
            assert!(
                !is_openai_reasoning_model(m, base),
                "'{m}' should NOT be recognised as a reasoning model"
            );
        }
        // xAI Grok reasoning variants — accept.
        let xai_base = "https://api.x.ai/v1";
        for m in &["grok-3-mini", "grok-3-reasoning", "grok-4"] {
            assert!(
                is_openai_reasoning_model(m, xai_base),
                "'{m}' should be recognised as a Grok reasoning variant"
            );
        }
        // Non-reasoning Grok variants.
        for m in &["grok-2", "grok-vision"] {
            assert!(
                !is_openai_reasoning_model(m, xai_base),
                "'{m}' should NOT be recognised as a Grok reasoning variant"
            );
        }
        // DeepSeek — always false regardless of model name.
        let ds_base = "https://api.deepseek.com/v1";
        for m in &["deepseek-reasoner", "o3-mini", "gpt-5"] {
            assert!(
                !is_openai_reasoning_model(m, ds_base),
                "'{m}' against DeepSeek base_url must return false"
            );
        }
    }

    /// Non-streaming OpenAI response with `reasoning_content` populates
    /// `LlmMessage::reasoning_content` via serde.
    #[test]
    fn test_openai_response_reasoning_content_parses() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final answer",
                    "reasoning_content": "thinking trace"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("thinking trace")
        );
    }

    /// OpenAI responses that use `reasoning` (raw field) deserialize
    /// into `reasoning_content` via the serde alias.
    #[test]
    fn test_openai_response_reasoning_alias_parses() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o1-preview",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning": "raw chain-of-thought"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("raw chain-of-thought"),
            "`reasoning` alias should populate `reasoning_content`"
        );
    }

    /// OpenAI responses that use `reasoning_summary` deserialize into
    /// `reasoning_content` via the serde alias. When OpenAI emits both
    /// `reasoning` and `reasoning_summary`, the second field listed in
    /// the JSON wins at serde time — which matches OpenAI's preferred
    /// "summary is the user-visible version" semantics since the
    /// summary is sent after the raw trace in the wire ordering.
    #[test]
    fn test_openai_response_reasoning_summary_parses_and_wins() {
        // Only `reasoning_summary` — it populates the field.
        let only_summary = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning_summary": "condensed summary"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(only_summary).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("condensed summary"),
        );

        // Both present: `reasoning_summary` wins because the custom
        // `Deserialize` impl on `LlmMessage` (see `llm_types.rs:48-91`)
        // collects all three fields into the `Raw` struct and applies an
        // explicit priority order — `reasoning_content` -> `reasoning_summary`
        // -> `reasoning`, first non-empty wins. With `reasoning_content` absent
        // here, the summary beats the raw trace. This matches OpenAI's
        // "prefer summary" rule for #768.
        let both = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning": "raw trace",
                    "reasoning_summary": "condensed summary"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(both).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("condensed summary"),
            "when both present, reasoning_summary should win"
        );
    }

    /// OpenAI o-series `usage.completion_tokens_details.reasoning_tokens`
    /// deserializes via the nested struct and is retrievable through
    /// `reasoning_tokens_effective`.
    #[test]
    fn test_usage_reasoning_tokens_openai_nested_shape() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 42,
                "total_tokens": 52,
                "completion_tokens_details": {
                    "reasoning_tokens": 30
                }
            }
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.expect("usage present");
        assert_eq!(usage.reasoning_tokens_effective(), Some(30));
    }

    /// DeepSeek / xAI flat `usage.reasoning_tokens` deserializes into
    /// the flat field and is retrievable through `reasoning_tokens_effective`.
    #[test]
    fn test_usage_reasoning_tokens_flat_shape() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "deepseek-reasoner",
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 100,
                "total_tokens": 110,
                "reasoning_tokens": 70
            }
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.expect("usage present");
        assert_eq!(usage.reasoning_tokens_effective(), Some(70));
    }

    /// Usage without any reasoning-token field leaves
    /// `reasoning_tokens_effective()` as `None` — not zero.
    #[test]
    fn test_usage_reasoning_tokens_absent() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-4o",
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.expect("usage present");
        assert!(usage.reasoning_tokens_effective().is_none());
    }

    /// Streaming delta with `reasoning_content` populates
    /// `Delta::reasoning_content` (DeepSeek / xAI shape).
    #[test]
    fn test_sse_delta_reasoning_content() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"deepseek-reasoner","choices":[{"index":0,"delta":{"reasoning_content":"ponder"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("ponder")
        );
    }

    /// Streaming delta with the `reasoning` alias populates
    /// `Delta::reasoning_content` (OpenAI o-series raw shape).
    #[test]
    fn test_sse_delta_reasoning_alias() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning":"ponder-raw"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("ponder-raw")
        );
    }

    /// Streaming delta with `reasoning_summary` alias populates
    /// `Delta::reasoning_content` (OpenAI summary shape).
    #[test]
    fn test_sse_delta_reasoning_summary_alias() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning_summary":"condensed"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = LlmClient::parse_sse_event(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("condensed")
        );
    }

    /// Wire invariant (#773): an outbound OpenAI request built by the
    /// runtime never carries `reasoning_content` on any message,
    /// because the loop path (see `loop_impl.rs`) always constructs
    /// assistant messages with `reasoning_content: None` regardless of
    /// what the previous turn returned. This test pins that behaviour
    /// by building a full request through the adapter and asserting no
    /// `reasoning_content` appears anywhere in the wire body.
    #[test]
    fn test_openai_request_body_never_contains_reasoning_content() {
        let client = openai_client();
        // Simulate a messages vec shaped the way context builder +
        // loop_impl produce it: system + user + assistant (no reasoning)
        // + tool_result + user. None of the assistant messages carry
        // `reasoning_content` because the runtime strips it before
        // persist and never re-populates it on context assembly.
        let request = CompletionRequest::new("gpt-4o").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("question"),
            LlmMessage::assistant("previous answer"),
            LlmMessage::tool_result("call_1", "tool output"),
            LlmMessage::user("follow-up"),
        ]);
        let req = client.build_request_for_test(&request).unwrap();
        let body = body_json(&req);

        // Walk every message and assert no `reasoning_content` field.
        let messages = body["messages"].as_array().expect("messages array");
        for m in messages {
            assert!(
                m.get("reasoning_content").is_none(),
                "outbound request leaked `reasoning_content` on message: {m}"
            );
        }
    }
}
