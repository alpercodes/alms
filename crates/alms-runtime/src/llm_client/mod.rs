mod cache_retry;
mod request;
mod sse_parsers;
mod streaming;
#[cfg(test)]
mod test_responder;

use crate::gemini_cache::{CacheLookup, GeminiCacheStore};
use crate::llm_types::*;
use alms_core::config::{AuthScheme, ProviderKind};
use alms_core::{AlmsError, AlmsResult};
use cache_retry::{CacheRetryDecision, decide_cache_retry};
use request::{apply_auth, apply_quirks, is_openai_reasoning_model};
use reqwest::{Client, RequestBuilder};
use streaming::stream_response;
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
pub(crate) enum Provider {
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
    /// Gemini context-cache store (#769), shared across clones so the
    /// coordinator / subagent clones inherit the same cache entries.
    /// A no-op when the effective provider is not Gemini.
    gemini_cache: GeminiCacheStore,
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

        let gemini_cache = GeminiCacheStore::new(client.clone());
        Ok(Self {
            client,
            config,
            provider,
            gemini_cache,
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

    /// Resolve a Gemini context cache for this request (#769).
    ///
    /// Called before [`Self::build_request`] on the Gemini path so the
    /// adapter can populate `cachedContent` on the outgoing body.
    /// Returns [`CacheLookup::Miss`] — and dispatches without cache — when:
    ///
    /// - The effective provider is not Gemini.
    /// - `gemini_cache_enabled != Some(true)`.
    /// - `session_id` is absent on the request.
    /// - Cache creation has previously flagged this prefix as below
    ///   Gemini's 32,768-token minimum, or failed with a transient error.
    ///
    /// Entry point for the async-only side of Gemini caching — it stays
    /// out of [`Self::build_request`] so that function can remain sync.
    async fn resolve_gemini_cache(&self, request: &CompletionRequest) -> CacheLookup {
        if self.provider != Provider::Gemini {
            return CacheLookup::Miss;
        }
        if !matches!(request.gemini_cache_enabled, Some(true)) {
            return CacheLookup::Miss;
        }
        let Some(session_id) = request.session_id else {
            // No session key → no cache reuse across turns. Caller is
            // responsible for setting session_id via `with_session_id`;
            // agent loop does this automatically.
            return CacheLookup::Miss;
        };

        // Build a throwaway copy of the request shape we'd send so we
        // can hand its `system_instruction` and `tools` to the cache
        // store. This is the only way to hash the exact prefix bytes
        // the wire would carry without duplicating the conversion
        // logic.
        let gemini_req = crate::gemini::to_gemini_request(request);
        let ttl_secs = request.gemini_cache_ttl_seconds.unwrap_or(300);

        self.gemini_cache
            .ensure_cache(
                session_id,
                &request.model,
                &self.config.base_url,
                &self.config.api_key,
                gemini_req.system_instruction.as_ref(),
                gemini_req.tools.as_deref(),
                ttl_secs,
            )
            .await
    }

    /// Create a completion request builder, adapting format per provider.
    ///
    /// Applies any configured [`alms_core::config::ProviderQuirks`] to the
    /// outgoing request body before serialization, and attaches the API key
    /// according to the configured [`alms_core::config::AuthScheme`].
    ///
    /// `gemini_cache_name`, when provided, is written to the `cachedContent`
    /// field on the Gemini request body so the provider serves the stable
    /// prefix at the discounted cache-read rate. Ignored for non-Gemini
    /// providers.
    fn build_request(
        &self,
        request: &CompletionRequest,
        gemini_cache_name: Option<&str>,
    ) -> AlmsResult<RequestBuilder> {
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
                let mut gemini_req = crate::gemini::to_gemini_request(request);
                // Attach cached-content reference (#769). When the cache
                // store returned a hit, the outgoing body references the
                // cache by name; Gemini serves the covered prefix at the
                // discounted rate and only bills `contents[]` not
                // covered as standard input.
                gemini_req.cached_content = gemini_cache_name.map(|s| s.to_string());
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

        // Gemini-only: resolve the cached-contents reference (if any) for
        // this request's session before building the outgoing body.
        // Returns `CacheLookup::Miss` on every other provider.
        let cache_lookup = self.resolve_gemini_cache(&request).await;
        let cache_name = match &cache_lookup {
            CacheLookup::Hit(n) => Some(n.as_str()),
            CacheLookup::Miss => None,
        };

        let req = self.build_request(&request, cache_name)?;

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
            // #769: if we referenced a Gemini cache that the server no
            // longer recognises (TTL expired, GC'd, or deleted out of
            // band), invalidate the stored handle and retry once without
            // `cachedContent`. Transparent to the agent loop. Decision
            // lives in `decide_cache_retry` — pure function, unit-tested.
            if let CacheRetryDecision::Retry { session_id } =
                decide_cache_retry(cache_name, &error_text, request.session_id)
            {
                warn!(
                    session_id = %session_id.0,
                    "Gemini cache expired/not-found — invalidating and retrying without cache"
                );
                self.gemini_cache.invalidate(session_id);
                let retry = self.build_request(&request, None)?;
                let retry_response = retry
                    .send()
                    .await
                    .map_err(|e| AlmsError::Runtime(format!("HTTP request failed: {}", e)))?;
                // Symmetric with the `complete_stream()` retry branch
                // (#787 re-review nit 1): if the retry itself fails
                // (e.g. the fresh cache handle is also rejected, or 500
                // server error), surface a clean HTTP error instead of
                // handing a non-success body to `parse_completion_response`
                // where it would masquerade as a JSON-parse error.
                let retry_status = retry_response.status();
                if !retry_status.is_success() {
                    let retry_err = retry_response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    error!(
                        "LLM API error on cache-retry: {} - {}",
                        retry_status, retry_err
                    );
                    return Err(AlmsError::Runtime(format!(
                        "LLM API error: {} - {}",
                        retry_status, retry_err
                    )));
                }
                return self.parse_completion_response(retry_response).await;
            }
            error!("LLM API error: {} - {}", status, error_text);
            return Err(AlmsError::Runtime(format!(
                "LLM API error: {} - {}",
                status, error_text
            )));
        }

        self.parse_completion_response(response).await
    }

    /// Parse a successful HTTP response into a `CompletionResponse`.
    ///
    /// Extracted so the cache-expired retry path (#769) can hit the
    /// same body-read + provider-dispatch + warn-on-null-content logic
    /// as the primary success path. Only called when `response.status()`
    /// is already a success — error handling lives on the caller.
    async fn parse_completion_response(
        &self,
        response: reqwest::Response,
    ) -> AlmsResult<CompletionResponse> {
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

        // Gemini: resolve cached-contents reference (#769). Same flow as
        // `complete()`. Miss-on-non-Gemini is a no-op.
        let cache_lookup = self.resolve_gemini_cache(&request).await;
        let cache_name = match &cache_lookup {
            CacheLookup::Hit(n) => Some(n.as_str()),
            CacheLookup::Miss => None,
        };

        let req = self.build_request(&request, cache_name)?;

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
            // Same cache-expired recovery path as the non-stream branch
            // (#769). On cache-not-found, invalidate and retry once with
            // no `cachedContent`. Transparent to the agent loop. The
            // retry gate lives in `decide_cache_retry` — shared with
            // `complete()` so the two branches cannot diverge.
            if let CacheRetryDecision::Retry { session_id } =
                decide_cache_retry(cache_name, &error_text, request.session_id)
            {
                warn!(
                    session_id = %session_id.0,
                    "Gemini cache expired/not-found — invalidating and retrying stream without cache"
                );
                self.gemini_cache.invalidate(session_id);
                let retry = self.build_request(&request, None)?;
                let retry_response = retry
                    .send()
                    .await
                    .map_err(|e| AlmsError::Runtime(format!("HTTP request failed: {}", e)))?;
                let retry_status = retry_response.status();
                if !retry_status.is_success() {
                    let retry_err = retry_response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    error!(
                        "LLM API error on cache-retry: {} - {}",
                        retry_status, retry_err
                    );
                    return Err(AlmsError::Runtime(format!(
                        "LLM API error: {} - {}",
                        retry_status, retry_err
                    )));
                }
                return Ok(stream_response(
                    retry_response,
                    self.provider,
                    self.config.stream_chunk_timeout_secs,
                ));
            }
            error!("LLM API error: {} - {}", status, error_text);
            return Err(AlmsError::Runtime(format!(
                "LLM API error: {} - {}",
                status, error_text
            )));
        }

        Ok(stream_response(
            response,
            self.provider,
            self.config.stream_chunk_timeout_secs,
        ))
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
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
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
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
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

    /// Get the wire-shape kind of the configured provider.
    ///
    /// Returns the [`ProviderKind`] for the active provider — looked up
    /// from `[llm.providers.<name>].kind` when available, otherwise
    /// inferred from the sugar-name fallback used by `apply_provider`
    /// (`anthropic` → `Anthropic`, `gemini` → `Gemini`, anything else →
    /// `OpenAiCompatible`).
    ///
    /// Used by `resolve_agent_config` to detect cross-namespace per-agent
    /// model leaks (#942) — a per-agent model whose prefix doesn't belong
    /// to the new provider's wire shape would otherwise 404 downstream.
    pub fn provider_kind(&self) -> ProviderKind {
        if let Some(entry) = self.config.providers.get(&self.config.provider) {
            return entry.kind;
        }
        match self.config.provider.as_str() {
            "anthropic" => ProviderKind::Anthropic,
            "gemini" => ProviderKind::Gemini,
            _ => ProviderKind::OpenAiCompatible,
        }
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
        // Tests bypass the async cache-resolution step, so no
        // `cachedContent` is ever attached — the Gemini wire-shape
        // assertions in the test suite remain byte-identical to pre-#769.
        let builder = self.build_request(request, None)?;
        builder
            .build()
            .map_err(|e| AlmsError::Runtime(format!("build request: {e}")))
    }

    /// Build a request with an explicit Gemini cache-name attached (test-only).
    ///
    /// Added for #769 tests that need to assert the `cachedContent`
    /// wire field lands on the outgoing body. Never exercises the
    /// async cache store — the caller supplies the name directly.
    #[cfg(test)]
    pub(crate) fn build_request_with_cache_for_test(
        &self,
        request: &CompletionRequest,
        cache_name: &str,
    ) -> AlmsResult<reqwest::Request> {
        let builder = self.build_request(request, Some(cache_name))?;
        builder
            .build()
            .map_err(|e| AlmsError::Runtime(format!("build request: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::sse_parsers::{parse_gemini_sse_block, parse_openai_sse};
    use super::test_responder::spawn_sequential_responder;
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
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_parse_sse_event_done() {
        assert!(matches!(
            parse_openai_sse("data: [DONE]"),
            SseParseResult::Done
        ));
    }

    #[test]
    fn test_parse_sse_event_comment_is_skip() {
        assert!(matches!(
            parse_openai_sse(": comment"),
            SseParseResult::Skip
        ));
    }

    #[test]
    fn test_parse_sse_event_tool_call_delta() {
        let event = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"te"}}]},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
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
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
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
        let result = parse_gemini_sse_block(event);
        match result {
            SseParseResult::Chunk(chunk) => {
                assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
            }
            _ => panic!("expected Chunk"),
        }
    }

    // ------------------------------------------------------------------
    // Gemini context caching (issue #769)
    // ------------------------------------------------------------------

    /// When the LLM client passes a cache name through to `build_request`,
    /// the outgoing Gemini body carries `cachedContent: "cachedContents/<id>"`
    /// at top level. End-to-end wire-shape check for the attach side.
    #[test]
    fn test_gemini_build_request_attaches_cached_content() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "gemini-test-key".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let request =
            CompletionRequest::new("gemini-2.5-pro").with_messages(vec![LlmMessage::user("hi")]);
        let req = client
            .build_request_with_cache_for_test(&request, "cachedContents/abc123")
            .unwrap();

        let body_bytes = req.body().and_then(|b| b.as_bytes()).expect("body present");
        let body: serde_json::Value = serde_json::from_slice(body_bytes).unwrap();
        assert_eq!(body["cachedContent"], "cachedContents/abc123");
    }

    /// When a Gemini cache is attached, the outgoing request body carries
    /// `cachedContent` **alongside** `systemInstruction` and `tools` on the
    /// same wire body. Pinned because Tim's review flagged the possibility
    /// that Gemini historically rejected requests setting all three — the
    /// current Gemini v1beta API contract (verified 2026-04-22) accepts
    /// all three as independent optional fields on `GenerateContentRequest`,
    /// and the Google Gen AI Python SDK + Vercel AI SDK both send all three
    /// together without stripping. When `cachedContent` resolves to an
    /// existing cache, Gemini serves the cached prefix at the discounted
    /// rate; the request-level fields remain present but are effectively
    /// redundant. If Gemini ever tightens this, the retry path in
    /// the LLM client can broaden the matcher to cover the new error
    /// shape and drop the cache on the retry.
    #[test]
    fn test_gemini_cached_content_coexists_with_system_and_tools() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "gemini-test-key".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let tool =
            ToolDefinition::new("echo", "Echo back the input").with_parameters(serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }));
        let request = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![
                LlmMessage::system("be helpful"),
                LlmMessage::user("hi"),
            ])
            .with_tools(vec![tool]);
        let req = client
            .build_request_with_cache_for_test(&request, "cachedContents/abc123")
            .unwrap();

        let body_bytes = req.body().and_then(|b| b.as_bytes()).expect("body present");
        let body: serde_json::Value = serde_json::from_slice(body_bytes).unwrap();
        // All three top-level fields must coexist on the wire body.
        assert_eq!(
            body["cachedContent"], "cachedContents/abc123",
            "cachedContent must be present"
        );
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"], "be helpful",
            "systemInstruction must be present alongside cachedContent"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"], "echo",
            "tools[] must be present alongside cachedContent"
        );
    }

    /// Byte parity: when no cache name is passed, the Gemini body has
    /// no `cachedContent` field — preserves pre-#769 wire shape.
    #[test]
    fn test_gemini_build_request_without_cache_has_no_cached_content() {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "gemini-test-key".into();
        let client = LlmClient::new(runtime_cfg).unwrap();

        let request =
            CompletionRequest::new("gemini-2.5-pro").with_messages(vec![LlmMessage::user("hi")]);
        let req = client.build_request_for_test(&request).unwrap();

        let body_bytes = req.body().and_then(|b| b.as_bytes()).expect("body present");
        let body: serde_json::Value = serde_json::from_slice(body_bytes).unwrap();
        assert!(
            body.get("cachedContent").is_none(),
            "cachedContent must be absent on non-cache requests"
        );
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

    /// When a response carries all three reasoning wire fields
    /// simultaneously (the adversarial case where an OpenAI-compat
    /// provider echoes both DeepSeek-style `reasoning_content` and
    /// OpenAI-style `reasoning_summary` + `reasoning` on the same
    /// message), the custom `Deserialize` impl on `LlmMessage` (see
    /// `llm_types.rs:48-91`) must pick `reasoning_content` — it has
    /// top priority in the canonical > summary > raw ordering. Pins
    /// the priority rule against a future refactor that reorders the
    /// match arms.
    #[test]
    fn test_openai_response_all_three_reasoning_fields_prefers_reasoning_content() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning_content": "canonical",
                    "reasoning_summary": "summary",
                    "reasoning": "raw"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("canonical"),
            "reasoning_content must win over both reasoning_summary and reasoning"
        );
    }

    /// An empty-string `reasoning_content` must fall through to
    /// `reasoning_summary`, not be treated as a present-but-empty
    /// value. The `.filter(|s| !s.is_empty())` call in the custom
    /// `Deserialize` impl on `LlmMessage` (see `llm_types.rs:48-91`)
    /// is the enforcement point; this test pins that behaviour. A
    /// twin assertion covers the `reasoning_summary: ""` -> `reasoning`
    /// fall-through to round out the empty-string ladder.
    #[test]
    fn test_openai_response_empty_reasoning_content_falls_through_to_summary() {
        // `reasoning_content: ""` falls through to non-empty
        // `reasoning_summary`.
        let empty_content = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning_content": "",
                    "reasoning_summary": "summary text"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(empty_content).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("summary text"),
            "empty reasoning_content must fall through to reasoning_summary"
        );

        // Twin: `reasoning_summary: ""` falls through to non-empty
        // `reasoning`, confirming the fall-through works at the
        // second ladder rung too.
        let empty_summary = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning_summary": "",
                    "reasoning": "raw trace"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(empty_summary).unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("raw trace"),
            "empty reasoning_summary must fall through to reasoning"
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
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
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
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
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
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("condensed")
        );
    }

    /// Streaming counterpart to
    /// `test_openai_response_all_three_reasoning_fields_prefers_reasoning_content`:
    /// when a single SSE delta chunk carries all three reasoning
    /// wire fields at once, the custom `Deserialize` impl on `Delta`
    /// (see `llm_types.rs:445-483`) must select `reasoning_content`
    /// per the canonical > summary > raw priority rule.
    #[test]
    fn test_openai_delta_all_three_reasoning_fields_prefers_reasoning_content() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning_content":"canonical","reasoning_summary":"summary","reasoning":"raw"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("canonical"),
            "reasoning_content must win in the delta over both reasoning_summary and reasoning"
        );
    }

    /// Streaming counterpart to
    /// `test_openai_response_empty_reasoning_content_falls_through_to_summary`:
    /// an empty-string `reasoning_content` in a single SSE delta
    /// must fall through to a non-empty `reasoning_summary`, with a
    /// twin assertion that `reasoning_summary: ""` falls through to
    /// `reasoning`. Pins the `.filter(|s| !s.is_empty())` behaviour
    /// in the `Delta` custom `Deserialize` (see `llm_types.rs:445-483`).
    #[test]
    fn test_openai_delta_empty_reasoning_content_falls_through_to_summary() {
        // Empty `reasoning_content` falls through to `reasoning_summary`.
        let empty_content = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning_content":"","reasoning_summary":"summary text"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(empty_content) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("summary text"),
            "empty reasoning_content must fall through to reasoning_summary in delta"
        );

        // Twin: empty `reasoning_summary` falls through to `reasoning`.
        let empty_summary = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning_summary":"","reasoning":"raw trace"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(empty_summary) else {
            panic!("expected Chunk");
        };
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("raw trace"),
            "empty reasoning_summary must fall through to reasoning in delta"
        );
    }

    /// Terminal case on the priority ladder (#782): when all three
    /// reasoning wire fields are present but empty, the custom
    /// `Deserialize` impl on `LlmMessage` (see `llm_types.rs:48-91`)
    /// must resolve `reasoning_content` to `None` — every rung of the
    /// `.filter(|s| !s.is_empty())` chain rejects, and the final
    /// `.or_else` returns `None`. Pins the bottom of the truth table
    /// against a future refactor that conflates "present-but-empty"
    /// with "populated".
    #[test]
    fn test_openai_response_all_empty_reasoning_fields_yields_none() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final",
                    "reasoning_content": "",
                    "reasoning_summary": "",
                    "reasoning": ""
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        assert!(
            resp.choices[0].message.reasoning_content.is_none(),
            "all-empty reasoning fields must yield None"
        );
    }

    /// Twin of `test_openai_response_all_empty_reasoning_fields_yields_none`
    /// for the case where none of the three reasoning fields appear in
    /// the JSON at all. The `#[serde(default)]` on each `Option<String>`
    /// in `Raw` keeps them as `None`, and the priority chain then
    /// resolves to `None` as well. Closes the "all-absent" corner of the
    /// priority-ladder truth table (#782).
    #[test]
    fn test_openai_response_all_absent_reasoning_fields_yields_none() {
        let json = r#"{
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "final"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let resp: CompletionResponse = serde_json::from_str(json).unwrap();
        assert!(
            resp.choices[0].message.reasoning_content.is_none(),
            "all-absent reasoning fields must yield None"
        );
    }

    /// Streaming counterpart to
    /// `test_openai_response_all_empty_reasoning_fields_yields_none`
    /// (#782): when a single SSE delta chunk carries all three
    /// reasoning fields as empty strings, the custom `Deserialize`
    /// impl on `Delta` (see `llm_types.rs:445-483`) must resolve
    /// `reasoning_content` to `None`. Pins the terminal case on the
    /// streaming path.
    #[test]
    fn test_openai_delta_all_empty_reasoning_fields_yields_none() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"o3","choices":[{"index":0,"delta":{"reasoning_content":"","reasoning_summary":"","reasoning":""},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
            panic!("expected Chunk");
        };
        assert!(
            chunk.choices[0].delta.reasoning_content.is_none(),
            "all-empty reasoning fields in delta must yield None"
        );
    }

    /// Twin of `test_openai_delta_all_empty_reasoning_fields_yields_none`
    /// for the case where none of the three reasoning fields appear in
    /// the delta JSON at all — e.g. a plain content-only chunk. The
    /// `Option` + `#[serde(default)]` combination on `Raw` keeps them
    /// `None`, and the priority chain resolves to `None`. Closes the
    /// "all-absent" corner of the streaming priority-ladder truth
    /// table (#782).
    #[test]
    fn test_openai_delta_all_absent_reasoning_fields_yields_none() {
        let event = r#"data: {"id":"x","object":"chat.completion.chunk","created":0,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let SseParseResult::Chunk(chunk) = parse_openai_sse(event) else {
            panic!("expected Chunk");
        };
        assert!(
            chunk.choices[0].delta.reasoning_content.is_none(),
            "all-absent reasoning fields in delta must yield None"
        );
    }

    // ------------------------------------------------------------------
    // #787 re-review nit 1: symmetric retry status check in `complete()`.
    //
    // `complete_stream()` has always gated the cache-expired retry
    // response on `retry_status.is_success()` before handing it to the
    // stream parser. `complete()` previously went straight to
    // `parse_completion_response`, so a 500 on the retry surfaced as a
    // "Failed to parse response" error instead of a clean "LLM API
    // error: 500 - ...". The fix mirrors the stream branch. This test
    // pins the behaviour with a tiny hand-rolled TCP responder so we
    // don't have to take on wiremock as a dev-dep.
    // ------------------------------------------------------------------

    fn gemini_client_with_base_url(base_url: String) -> LlmClient {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "gemini".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "gemini-test-key".into();
        runtime_cfg.base_url = base_url;
        // Short chunk timeout keeps the test snappy if something hangs.
        runtime_cfg.timeout_secs = 10;
        LlmClient::new(runtime_cfg).unwrap()
    }

    /// Nit 1: when the cache-expired retry in `complete()` itself fails
    /// with a non-success status, the client must surface a clean
    /// `"LLM API error: <status> - ..."` string — not a JSON parse
    /// error from feeding the error body to the success-path parser.
    #[tokio::test]
    async fn complete_cache_retry_non_success_yields_typed_http_error() {
        // Two responses, in order:
        //   1. First request: 404 with a Gemini cache-not-found body.
        //      Triggers the retry branch via `decide_cache_retry`.
        //   2. Retry request: 500 with a generic error body.
        //      Must NOT be parsed — must surface as "LLM API error: 500".
        let base_url = spawn_sequential_responder(vec![
            (
                404,
                r#"{"error":{"code":404,"status":"NOT_FOUND","message":"CachedContent cachedContents/abc was not found"}}"#,
            ),
            (500, r#"{"error":{"message":"internal"}}"#),
        ])
        .await;

        let client = gemini_client_with_base_url(base_url);

        // Seed the gemini cache store with an Active handle so the
        // first request carries `cachedContent: "cachedContents/abc"`
        // and is eligible for the retry branch.
        let session = alms_core::SessionId::new();
        client.gemini_cache.install_active_for_test(
            session,
            "cachedContents/abc".into(),
            0xabcd_ef01,
        );

        let request = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_session_id(session)
            .with_gemini_cache_enabled(true);

        let err = client
            .complete(request)
            .await
            .expect_err("retry returning 500 must surface as a typed error");

        let msg = err.to_string();
        assert!(
            msg.contains("LLM API error: 500"),
            "expected clean HTTP error shape, got: {msg}"
        );
        assert!(
            !msg.contains("Failed to parse"),
            "retry non-success must not masquerade as a parse error, got: {msg}"
        );
    }

    /// Nit 1 companion: the stream branch has always gated on
    /// `retry_status.is_success()` — pin the behaviour so a future
    /// refactor can't silently delete the check and let a 500 response
    /// body flow into `stream_response` as if it were an SSE stream.
    #[tokio::test]
    async fn complete_stream_cache_retry_non_success_yields_typed_http_error() {
        let base_url = spawn_sequential_responder(vec![
            (
                404,
                r#"{"error":{"code":404,"status":"NOT_FOUND","message":"CachedContent cachedContents/abc was not found"}}"#,
            ),
            (500, r#"{"error":{"message":"internal"}}"#),
        ])
        .await;

        let client = gemini_client_with_base_url(base_url);

        let session = alms_core::SessionId::new();
        client.gemini_cache.install_active_for_test(
            session,
            "cachedContents/abc".into(),
            0xabcd_ef01,
        );

        let request = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_session_id(session)
            .with_gemini_cache_enabled(true);

        // `BoxStream` isn't `Debug`, so hand-match instead of `expect_err`.
        let msg = match client.complete_stream(request).await {
            Ok(_) => panic!("stream retry returning 500 must surface as a typed error"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("LLM API error: 500"),
            "expected clean HTTP error shape, got: {msg}"
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
