mod cache_retry;
mod diagnostic;
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
use diagnostic::{DecodeDiagnostic, flatten_error_chain, format_decode_error};
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
        // The only reqwest-level deadline is the *total* `.timeout()` —
        // "from when the request starts connecting until the response body has
        // finished". It bounds the whole call (connect + headers + full body)
        // at `timeout_secs`, and is the documented outer bound that governs
        // time-to-first-byte for a healthy-but-slow-to-start response.
        //
        // The per-read inactivity guard for a *stalled* body (#1163) is NOT
        // set here as a client-level `.read_timeout()`. reqwest's client-level
        // read timeout also caps the header / time-to-first-byte wait (the
        // sleep is polled while still waiting for connect + TLS + headers and
        // does not reset until the body wrapper takes over), so wiring it to
        // `stream_chunk_timeout_secs` would tighten the time-to-first-byte
        // ceiling from `timeout_secs` to `stream_chunk_timeout_secs` for every
        // call — regressing a healthy non-streaming `application/json` upstream
        // (which buffers the whole completion before sending headers) or a slow
        // reasoning model that legitimately takes longer than that to first
        // byte. Instead the inactivity guard lives *inside the body read* on
        // both paths: `streaming.rs` wraps each SSE chunk poll, and
        // `parse_completion_response` reads the buffered body as a chunk stream
        // under the same per-chunk `tokio::time::timeout`. A mid-body stall
        // therefore faults in `stream_chunk_timeout_secs`, but the header wait
        // stays bounded only by the total `timeout_secs`.
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

    /// Stable, lowercase short name for the wire-protocol family.
    ///
    /// Used to label structured `AlmsError::SubagentLlmError` payloads
    /// (#920) so callers can tell which provider returned the failing
    /// status without re-parsing the body. Distinct from
    /// `LlmConfig::provider`, which carries the user-facing config key
    /// and may be a sugar alias (`openrouter`, `groq`, ...) — we always
    /// return the protocol family the request was actually built against.
    fn provider_name(&self) -> &'static str {
        match self.provider {
            Provider::OpenAi => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Gemini => "gemini",
        }
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
                    // #920: emit the structured variant so the parent
                    // agent's `tool_result` reads as one tractable line
                    // (`Subagent LLM error (gemini 400): ...`) instead of
                    // the legacy 4-prefix wrap. The constructor
                    // normalises newlines in `body` so multi-line
                    // provider responses (e.g. Gemini's pretty JSON)
                    // still render as a single tractable line.
                    return Err(AlmsError::subagent_llm_error(
                        self.provider_name(),
                        retry_status.as_u16(),
                        retry_err,
                    ));
                }
                return self
                    .parse_completion_response(retry_response, &request)
                    .await;
            }
            error!("LLM API error: {} - {}", status, error_text);
            // #920: structured variant (see cache-retry branch above for
            // the full rationale).
            return Err(AlmsError::subagent_llm_error(
                self.provider_name(),
                status.as_u16(),
                error_text,
            ));
        }

        self.parse_completion_response(response, &request).await
    }

    /// Drain a successful response body into a `String` under a per-chunk
    /// inactivity timeout, instead of the unbounded-per-read
    /// `response.text().await` (#1163).
    ///
    /// Each `bytes_stream()` poll is wrapped in
    /// `tokio::time::timeout(stream_chunk_timeout_secs, …)` — the same
    /// body-only idle guard the streaming path applies in `streaming.rs`. A
    /// body that starts arriving and then stalls mid-transfer (the #1163
    /// symptom: a buffered `application/json` completion that never finishes)
    /// therefore faults within `stream_chunk_timeout_secs` rather than hanging
    /// until the total `.timeout()` deadline. Because the guard lives on the
    /// body read and not on the client, the header / time-to-first-byte wait
    /// stays bounded only by the total `timeout_secs` (no healthy-path
    /// regression for a slow-to-start but otherwise healthy response).
    ///
    /// Both failure modes — a mid-body read error (connection reset, malformed
    /// chunked transfer, H2 stream reset, gzip decode failure) and a stall —
    /// surface the enriched #1044 decode diagnostic with provider, model,
    /// status, content-type, and `bytes_read` intact, mirroring the streaming
    /// path's mid-stream error shape.
    async fn read_body_with_idle_timeout(
        &self,
        response: reqwest::Response,
        model: &str,
        status: u16,
        content_type: Option<&str>,
        content_length: Option<u64>,
    ) -> AlmsResult<String> {
        use futures::StreamExt;

        let chunk_timeout = std::time::Duration::from_secs(self.config.stream_chunk_timeout_secs);
        let provider_name = self.config.provider.as_str();

        let mut byte_stream = response.bytes_stream();
        let mut body = Vec::<u8>::new();

        loop {
            match tokio::time::timeout(chunk_timeout, byte_stream.next()).await {
                // A chunk arrived — accumulate and keep reading.
                Ok(Some(Ok(chunk))) => body.extend_from_slice(&chunk),
                // Body finished cleanly.
                Ok(None) => break,
                // Mid-body read error (connection reset, malformed chunked
                // transfer, H2 stream reset, gzip decode failure, etc.).
                // reqwest's bare `Display` collapses this to "error decoding
                // response body" with no context — walk the source chain and
                // bake in the metadata captured before the read so the
                // operator (and the parent agent's tool-call result) can tell
                // the causes apart. See #1044.
                Ok(Some(Err(e))) => {
                    let chain = flatten_error_chain(&e);
                    let diag = DecodeDiagnostic {
                        provider: provider_name,
                        model,
                        status: Some(status),
                        content_type,
                        content_length,
                        bytes_read: Some(body.len()),
                        body_prefix: None,
                    };
                    let msg = format_decode_error("LLM response decode failed", &diag, &chain);
                    error!("{msg}");
                    return Err(AlmsError::Runtime(msg));
                }
                // Per-chunk inactivity timeout: the body started arriving (or
                // not) and then went quiet for `stream_chunk_timeout_secs`.
                // This is the #1163 stall. Surface the same enriched decode
                // diagnostic — reqwest renders a real read/total timeout as
                // "operation timed out", so we use the same wording for the
                // synthetic body-idle timeout to keep the operator-facing
                // shape and the existing tests aligned.
                Err(_) => {
                    let diag = DecodeDiagnostic {
                        provider: provider_name,
                        model,
                        status: Some(status),
                        content_type,
                        content_length,
                        bytes_read: Some(body.len()),
                        body_prefix: None,
                    };
                    let msg = format_decode_error(
                        "LLM response decode failed",
                        &diag,
                        &format!(
                            "response body stalled (no data for {}s) — operation timed out",
                            chunk_timeout.as_secs()
                        ),
                    );
                    error!("{msg}");
                    return Err(AlmsError::Runtime(msg));
                }
            }
        }

        // Decode the assembled bytes. `from_utf8_lossy` matches the streaming
        // path's per-chunk decode; in practice every provider returns UTF-8
        // JSON, and a lossy decode keeps a malformed-byte body parseable as
        // far as possible for the diagnostic body-prefix on a later JSON parse
        // failure rather than erroring opaquely here.
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Parse a successful HTTP response into a `CompletionResponse`.
    ///
    /// Extracted so the cache-expired retry path (#769) can hit the
    /// same body-read + provider-dispatch + warn-on-null-content logic
    /// as the primary success path. Only called when `response.status()`
    /// is already a success — error handling lives on the caller.
    ///
    /// On body-read or parse failure, the bubbled `AlmsError::Runtime`
    /// carries a structured diagnostic — provider, model, HTTP status,
    /// content-type, and (on parse failure) a 512-byte body prefix plus
    /// the full reqwest/serde error chain — produced by [`diagnostic`]
    /// (#1044). The coordinator's subagent path forwards `e.to_string()`
    /// verbatim through the parent's tool-call result, so the enriched
    /// diagnostic surfaces in the daemon log, the parent's tool-call
    /// payload, and the UI's subagent error block from this single site.
    async fn parse_completion_response(
        &self,
        response: reqwest::Response,
        request: &CompletionRequest,
    ) -> AlmsResult<CompletionResponse> {
        // Capture response metadata BEFORE `.text()` consumes the
        // response. Status, headers, and URL all live on `reqwest::Response`
        // and become inaccessible once the body is consumed (#1044).
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_length = response.content_length();
        let provider_name = self.config.provider.as_str();

        // Read the raw body first so we can log it for diagnostics, then
        // parse from the text.  This is essential for debugging models that
        // return content in unexpected fields (e.g. `reasoning_content`).
        //
        // The body is drained as a chunk stream under a per-chunk inactivity
        // timeout (`stream_chunk_timeout_secs`), mirroring the streaming path
        // in `streaming.rs` (#1163). `response.text()` would buffer the whole
        // body under only the total `.timeout()`, so a body that started
        // arriving and then stalled mid-transfer (the #1163 symptom —
        // `minimax/minimax-m3` on openrouter returning a buffered
        // `application/json` body that never finished) would hang for the
        // entire `timeout_secs` window before faulting. Wrapping each chunk
        // poll bounds a mid-body stall to `stream_chunk_timeout_secs` while
        // leaving the header / time-to-first-byte wait governed only by the
        // total `timeout_secs` (the read timeout is deliberately *not* a
        // client-level `.read_timeout()`, which would also cap the header
        // wait — see `LlmClient::new`).
        let body_text = self
            .read_body_with_idle_timeout(
                response,
                &request.model,
                status,
                content_type.as_deref(),
                content_length,
            )
            .await?;

        debug!(raw_body_len = body_text.len(), "LLM response body received");

        // Shared formatter for `serde_json::from_str` failures. Centralises
        // the diagnostic construction so the three provider arms below
        // produce byte-identical error shapes (#1044). The `context` arg
        // keeps the provider name in the human-readable prefix
        // ("...Anthropic..." vs "...Gemini..." etc.) for at-a-glance
        // categorisation in log output.
        let parse_err = |context: &str, e: serde_json::Error| -> AlmsError {
            let diag = DecodeDiagnostic {
                provider: provider_name,
                model: &request.model,
                status: Some(status),
                content_type: content_type.as_deref(),
                content_length,
                bytes_read: None,
                body_prefix: Some(body_text.as_str()),
            };
            let msg = format_decode_error(context, &diag, &e);
            error!("{msg}");
            AlmsError::Runtime(msg)
        };

        let completion: CompletionResponse = match self.provider {
            Provider::OpenAi => serde_json::from_str(&body_text)
                .map_err(|e| parse_err("LLM response parse failed (OpenAI)", e))?,
            Provider::Anthropic => {
                let anthropic_resp: crate::anthropic::AnthropicResponse =
                    serde_json::from_str(&body_text)
                        .map_err(|e| parse_err("LLM response parse failed (Anthropic)", e))?;
                crate::anthropic::from_anthropic_response(anthropic_resp)
            }
            Provider::Gemini => {
                let gemini_resp: crate::gemini::GeminiResponse =
                    serde_json::from_str(&body_text)
                        .map_err(|e| parse_err("LLM response parse failed (Gemini)", e))?;
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
                    // #920: structured variant (mirrors the non-stream
                    // branch — parent's `tool_result` should read as
                    // `Subagent LLM error (gemini 400): ...`). The
                    // constructor normalises newlines so multi-line
                    // provider bodies still render as one line.
                    return Err(AlmsError::subagent_llm_error(
                        self.provider_name(),
                        retry_status.as_u16(),
                        retry_err,
                    ));
                }
                return Ok(stream_response(
                    retry_response,
                    self.provider,
                    self.config.stream_chunk_timeout_secs,
                    self.config.provider.clone(),
                    request.model.clone(),
                ));
            }
            error!("LLM API error: {} - {}", status, error_text);
            // #920: structured variant (see cache-retry branch above for
            // the full rationale).
            return Err(AlmsError::subagent_llm_error(
                self.provider_name(),
                status.as_u16(),
                error_text,
            ));
        }

        Ok(stream_response(
            response,
            self.provider,
            self.config.stream_chunk_timeout_secs,
            self.config.provider.clone(),
            request.model.clone(),
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

    /// Borrow the `[llm.providers]` snapshot the client carries.
    ///
    /// The map is the same one consulted by [`Self::provider_kind`] and
    /// `apply_provider` — exposing it as a borrowed accessor lets gateway
    /// helpers (notably the shared raw-string model resolver used by
    /// `resolve_agent_config` and `validate_patch_budget`'s fleet check)
    /// reach the per-provider entries (`kind`, `model`, etc.) without
    /// duplicating the configuration plumbing.
    pub fn providers_snapshot(
        &self,
    ) -> &std::collections::BTreeMap<String, alms_core::config::ProviderEntry> {
        &self.config.providers
    }

    /// Whether this client is configured for mock mode (no real provider
    /// HTTP calls). Mirrors the `[llm].mock` flag the boot-time
    /// `AlmsConfig::validate` reads — gateway pre-flight paths consult this
    /// to skip provider-cap enforcement that would otherwise reject
    /// otherwise-valid mock test setups (per-run pre-flight #919, P2 #1
    /// follow-up).
    pub fn is_mock(&self) -> bool {
        self.config.mock
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
    use super::test_responder::{
        spawn_sequential_responder, spawn_slow_body_responder, spawn_slow_headers_responder,
        spawn_truncated_body_responder,
    };
    use super::*;
    use std::sync::Mutex;

    /// Module-local env-var serialization mutex. `cargo test` runs unit
    /// tests in this module on a parallel thread-pool by default, and
    /// concurrent `std::env::set_var` calls are unsound in Rust 1.79+
    /// (the writes race against any other thread reading env). Every
    /// test in this mod that touches the process env via `set_var` /
    /// `remove_var` must take this lock first. Mirrors the
    /// `llm_types::tests::ENV_LOCK` sibling pattern.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(updated.default_model(), "moonshotai/kimi-k2.6");
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

    /// Codex follow-up on #1081 (P1 #3): when an `SecretsStore` carries
    /// no key for the new provider but the `[llm.providers.<name>]`
    /// entry exposes one via `api_key` (or `api_key_env`),
    /// `with_provider_and_secrets` must resolve the entry key rather
    /// than clearing.
    ///
    /// This is the contract `AppState::new` now relies on when applying
    /// a persisted server-default provider switch on boot. Pre-fix, the
    /// boot path used `with_provider` (no resolver) and then the
    /// `runs::resolve_agent_config` path only re-checked `SecretsStore`
    /// via `with_secrets` — deployments configuring keys exclusively in
    /// the provider entry would silently boot with an empty key.
    #[test]
    fn test_with_provider_and_secrets_resolves_provider_entry_api_key() {
        // Build a config that mirrors the boot-time shape: starting on
        // OpenRouter with a key, with an `anthropic` provider entry that
        // carries an inline `api_key`. The SecretsStore is empty.
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: Some("sk-ant-from-entry".into()),
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "sk-or-existing".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            providers,
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        let secrets = alms_core::secrets::SecretsStore::empty();
        let switched = client.with_provider_and_secrets("anthropic", &secrets);
        assert_eq!(switched.provider(), "anthropic");
        assert_eq!(switched.base_url(), "https://api.anthropic.com/v1");
        // Key MUST resolve from the provider entry's `api_key`, not be
        // cleared (the pre-fix `with_provider` boot path would have
        // ended up with an empty key here).
        assert_eq!(switched.api_key(), "sk-ant-from-entry");
    }

    /// Variant of the above using `api_key_env` to confirm the env-var
    /// resolver path also flows through `with_provider_and_secrets`.
    #[test]
    fn test_with_provider_and_secrets_resolves_provider_entry_api_key_env() {
        const ENV_VAR: &str = "ALMS_TEST_LARRY_1081_ANTHROPIC_KEY";
        // Serialize against any other env-touching test in this mod —
        // concurrent `set_var` racing with another thread reading any
        // env is UB in Rust 1.79+. The unique key prevents value
        // collisions across tests but does not serialize the writes;
        // the lock does. Held until the matching `remove_var` below.
        let _env_guard = ENV_LOCK.lock().unwrap();
        // SAFETY: the lock above serializes the write against every
        // other test in this mod that touches the process env. Other
        // threads in the process may still observe the env mid-write
        // (this is the genuine UB hole `set_var` carries), but no
        // such reader exists in this test setup.
        unsafe { std::env::set_var(ENV_VAR, "sk-ant-from-env") };

        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: Some(ENV_VAR.into()),
                api_key: None,
                model: Some("claude-haiku-4-5".into()),
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let config = LlmConfig {
            provider: "openrouter".into(),
            api_key: "sk-or-existing".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            providers,
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();

        let secrets = alms_core::secrets::SecretsStore::empty();
        let switched = client.with_provider_and_secrets("anthropic", &secrets);
        unsafe { std::env::remove_var(ENV_VAR) };
        assert_eq!(switched.provider(), "anthropic");
        assert_eq!(switched.api_key(), "sk-ant-from-env");
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
    /// with a non-success status, the client must surface the structured
    /// `AlmsError::SubagentLlmError` variant (#920) carrying provider /
    /// status / body — not a JSON parse error from feeding the error body
    /// to the success-path parser.
    #[tokio::test]
    async fn complete_cache_retry_non_success_yields_typed_http_error() {
        // Two responses, in order:
        //   1. First request: 404 with a Gemini cache-not-found body.
        //      Triggers the retry branch via `decide_cache_retry`.
        //   2. Retry request: 500 with a generic error body.
        //      Must NOT be parsed — must surface as the structured
        //      `AlmsError::SubagentLlmError { provider, status: 500, .. }`
        //      variant (#920) rather than a parse error.
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

        // #920: structured variant. Pin the discriminant + fields so the
        // typed shape sticks, and the Display rendering so callers get a
        // single tractable line.
        match &err {
            AlmsError::SubagentLlmError {
                provider,
                status,
                body,
            } => {
                assert_eq!(provider, "gemini");
                assert_eq!(*status, 500);
                assert!(body.contains("internal"), "body must carry payload: {body}");
            }
            other => panic!("expected SubagentLlmError, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("Subagent LLM error (gemini 500)"),
            "expected structured Display, got: {msg}"
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
        let err = match client.complete_stream(request).await {
            Ok(_) => panic!("stream retry returning 500 must surface as a typed error"),
            Err(e) => e,
        };
        // #920: structured variant on the stream branch too.
        match &err {
            AlmsError::SubagentLlmError {
                provider, status, ..
            } => {
                assert_eq!(provider, "gemini");
                assert_eq!(*status, 500);
            }
            other => panic!("expected SubagentLlmError, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("Subagent LLM error (gemini 500)"),
            "expected structured Display, got: {msg}"
        );
    }

    // ------------------------------------------------------------------
    // #1044 regression: enriched diagnostic on adapter decode/parse
    // failures.
    //
    // Before #1044, every decode/parse failure in the LLM client
    // collapsed into one of two opaque strings — "Failed to read response
    // body: error decoding response body" or "Failed to parse response:
    // <serde>". Neither carried provider, model, HTTP status,
    // content-type, or a body prefix, so operators couldn't tell HTML
    // error pages from schema drift from mid-stream truncation. These
    // tests pin the structured-diagnostic shape so the bubbled
    // `AlmsError::Runtime` always carries the provider+model+status
    // bracket and (when the body was readable) a 512-byte body prefix.
    //
    // The coordinator's subagent path forwards `e.to_string()` verbatim
    // through the parent's tool-call result payload — see
    // `crates/alms-coordinator/src/lib.rs:693`. Pinning the runtime-side
    // shape implicitly pins the subagent/UI surface, since the bubbled
    // error string IS the subagent error block contents.
    // ------------------------------------------------------------------

    fn openai_client_with_base_url(base_url: String) -> LlmClient {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "openai".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "openai-test-key".into();
        runtime_cfg.base_url = base_url;
        runtime_cfg.timeout_secs = 10;
        LlmClient::new(runtime_cfg).unwrap()
    }

    fn anthropic_client_with_base_url(base_url: String) -> LlmClient {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "anthropic".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "anthropic-test-key".into();
        runtime_cfg.base_url = base_url;
        runtime_cfg.timeout_secs = 10;
        LlmClient::new(runtime_cfg).unwrap()
    }

    /// A 200-OK body that's not valid JSON (HTML error page shape — the
    /// canonical "Cloudflare returned an HTML challenge in front of the
    /// API endpoint" failure mode mentioned in #1044). The OpenAI
    /// adapter's `serde_json::from_str::<CompletionResponse>` must reject
    /// this and the bubbled error must carry provider, model, status,
    /// and a body prefix the operator can recognise as HTML.
    #[tokio::test]
    async fn decode_diagnostic_openai_html_body_includes_provider_model_status_and_prefix() {
        let html_body = "<!DOCTYPE html><html><head><title>Cloudflare \
                         challenge</title></head><body><h1>Just a \
                         moment...</h1></body></html>";
        // Tim's review on #1064 (P2): hand the responder an explicit
        // `text/html` content-type so the test actually exercises the
        // "operator can recognise an HTML page from the content_type
        // field" claim in the diagnostic. The original fixture
        // hardcoded `application/json` regardless of body, which made
        // this assertion mildly dishonest about real upstream behaviour.
        let base_url =
            spawn_sequential_responder(vec![(200, "text/html; charset=utf-8", html_body)]).await;
        let client = openai_client_with_base_url(base_url);
        let request =
            CompletionRequest::new("gpt-4o-mini").with_messages(vec![LlmMessage::user("hi")]);

        let err = client
            .complete(request)
            .await
            .expect_err("HTML body must fail to deserialize");
        let msg = err.to_string();

        assert!(
            msg.contains("LLM response parse failed"),
            "diagnostic context missing, got: {msg}"
        );
        assert!(
            msg.contains("provider=openai"),
            "provider field missing, got: {msg}"
        );
        assert!(
            msg.contains("model=gpt-4o-mini"),
            "model field missing, got: {msg}"
        );
        assert!(
            msg.contains("status=200"),
            "status field missing, got: {msg}"
        );
        // The `content_type=text/html` field is the operator's first
        // breadcrumb that an upstream edge proxy returned an HTML page
        // rather than the expected JSON envelope. Pin it so a future
        // fixture regression that drops the content-type doesn't fail
        // silently — the prefix-only check below would still pass.
        assert!(
            msg.contains("content_type=text/html"),
            "content_type field missing or wrong, got: {msg}"
        );
        // The HTML prefix must surface so the operator can recognise the
        // failure mode at a glance (no need to crank tracing to trace
        // level and re-trigger the bug).
        assert!(
            msg.contains("body_prefix="),
            "body_prefix field missing, got: {msg}"
        );
        assert!(
            msg.contains("DOCTYPE html") || msg.contains("Cloudflare"),
            "body prefix doesn't include HTML marker, got: {msg}"
        );
    }

    /// Schema-drift case for the Anthropic adapter: response is valid
    /// JSON but doesn't match `AnthropicResponse`. The bubbled error
    /// must carry the Anthropic-specific context label, status=200, and
    /// the body prefix showing the unexpected shape.
    #[tokio::test]
    async fn decode_diagnostic_anthropic_schema_drift_includes_body_prefix() {
        // Anthropic real response shape has `id`, `type`, `role`,
        // `content`, `model`, etc. This is a parseable JSON object but
        // missing every required field — schema drift / partial
        // response.
        let drift_body = r#"{"unexpected":"shape","no_content":true}"#;
        let base_url = spawn_sequential_responder(vec![(200, drift_body)]).await;
        let client = anthropic_client_with_base_url(base_url);
        let request =
            CompletionRequest::new("claude-sonnet-4-5").with_messages(vec![LlmMessage::user("hi")]);

        let err = client
            .complete(request)
            .await
            .expect_err("schema-drift body must fail to deserialize");
        let msg = err.to_string();

        assert!(
            msg.contains("LLM response parse failed (Anthropic)"),
            "diagnostic context missing, got: {msg}"
        );
        assert!(
            msg.contains("provider=anthropic"),
            "provider field missing, got: {msg}"
        );
        assert!(
            msg.contains("model=claude-sonnet-4-5"),
            "model field missing, got: {msg}"
        );
        assert!(
            msg.contains("status=200"),
            "status field missing, got: {msg}"
        );
        assert!(
            msg.contains("content_type=application/json"),
            "content_type field missing, got: {msg}"
        );
        assert!(
            msg.contains(r#"body_prefix="{\"unexpected\":\"shape\""#),
            "body prefix doesn't include the drift JSON, got: {msg}"
        );
    }

    /// The diagnostic body prefix is bounded — a 10 KB JSON body that
    /// fails to parse must NOT bake the entire payload into the bubbled
    /// error. The bound keeps the tool-call result payload (and any
    /// downstream session-history persistence) bounded regardless of
    /// upstream response size.
    #[tokio::test]
    async fn decode_diagnostic_body_prefix_is_truncated() {
        // 10 KB of malformed JSON. The first character is `{` so the
        // serde parser starts, then fails on the garbage filler.
        let mut huge_body = String::from(r#"{"truncated":"yes","filler":""#);
        huge_body.push_str(&"abc".repeat(4000)); // ~12 KB of filler
        // Don't close the JSON — let serde fail mid-parse on the cap.
        // Static body lifetime for the responder helper:
        let leaked: &'static str = Box::leak(huge_body.into_boxed_str());
        let base_url = spawn_sequential_responder(vec![(200, leaked)]).await;
        let client = openai_client_with_base_url(base_url);
        let request =
            CompletionRequest::new("gpt-4o-mini").with_messages(vec![LlmMessage::user("hi")]);

        let err = client
            .complete(request)
            .await
            .expect_err("malformed huge body must fail to deserialize");
        let msg = err.to_string();

        // The bubbled error must still be a sane size. With a 512-byte
        // body-prefix cap plus a fixed header, the total stays under a
        // few KB regardless of upstream body size — the test asserts
        // a generous ceiling so we catch a regression that drops the cap
        // entirely without being brittle about the exact diagnostic
        // bytes the helper produces today.
        assert!(
            msg.len() < 4096,
            "diagnostic should be bounded; got {} bytes",
            msg.len()
        );
        assert!(
            msg.contains("body_prefix="),
            "body_prefix field missing, got: {msg}"
        );
        // The ellipsis marker confirms the truncator actually fired —
        // pinning this guarantees the body prefix didn't accidentally
        // grow to absorb the full body via a future "let's show more
        // context" refactor.
        assert!(
            msg.contains('…'),
            "truncation ellipsis missing from body prefix, got first 200 chars: {}",
            &msg[..msg.len().min(200)]
        );
    }

    /// #1064 review (Tim, P1): drive `stream_response` through a real
    /// mid-stream `bytes_stream()` error end-to-end. Until this test
    /// landed, the streaming branch of the enriched diagnostic was
    /// pinned only by the formatter unit test
    /// (`formats_streaming_bytes_read`) — the wiring that threads
    /// `provider_name`, `model`, and `total_bytes_read` from
    /// `complete_stream` through `stream_response` into the bubbled
    /// `AlmsError::Runtime` had no end-to-end coverage. Streaming is
    /// the most likely site for the next #1044-class incident
    /// (HTTP/2 RST_STREAM, mid-body connection reset, malformed chunked
    /// transfer), so plugging this gap is load-bearing.
    ///
    /// Fixture: [`spawn_truncated_body_responder`] writes valid HTTP
    /// headers advertising `Content-Length: N` strictly greater than
    /// the body it actually sends, then drops the socket. reqwest sees
    /// the premature EOF on the body stream and surfaces a
    /// `BodyDecodeError` on the next `bytes_stream()` poll — exactly
    /// what we want to drive through the `Ok(Some(Err(e)))` arm.
    #[tokio::test]
    async fn complete_stream_mid_stream_decode_failure_carries_enriched_diagnostic() {
        use futures::StreamExt;

        // Write some valid-looking SSE-shaped prefix bytes so the
        // `bytes_read` counter advances past zero before reqwest sees
        // the premature EOF. The trailing SSE event terminator (two
        // consecutive newlines) is intentionally missing — the SSE
        // parser won't dispatch a chunk, so the byte pull stays in the
        // `Need more data` branch until the stream errors out.
        let partial_body =
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hel";
        let base_url = spawn_truncated_body_responder(
            200,
            "text/event-stream",
            partial_body,
            // Advertise far more bytes than we send — reqwest will fault
            // on the close because the body stream ends short of the
            // advertised length.
            4096,
        )
        .await;
        let client = openai_client_with_base_url(base_url);
        let request =
            CompletionRequest::new("gpt-4o-mini").with_messages(vec![LlmMessage::user("hi")]);

        // `BoxStream` isn't `Debug`, so hand-match instead of `expect_err`.
        let mut stream = match client.complete_stream(request).await {
            Ok(s) => s,
            Err(e) => panic!(
                "complete_stream itself returned Err — wanted the error to surface inside the stream, got: {e}"
            ),
        };

        // Pump the stream. We expect at most a handful of `Skip` cycles
        // and then a single `Err` carrying the enriched diagnostic.
        // The loop runs with a bounded iteration cap so a future
        // regression that yields a non-erroring infinite stream still
        // fails cleanly instead of hanging.
        let mut err_msg = None;
        for _ in 0..32 {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    err_msg = Some(e.to_string());
                    break;
                }
                None => break,
            }
        }
        let msg = err_msg.expect(
            "stream must yield a mid-stream decode error before completing —              the truncated-body responder closes the socket before the              advertised Content-Length is satisfied",
        );

        assert!(
            msg.contains("LLM stream decode failed"),
            "diagnostic context missing, got: {msg}"
        );
        assert!(
            msg.contains("provider=openai"),
            "provider field missing, got: {msg}"
        );
        assert!(
            msg.contains("model=gpt-4o-mini"),
            "model field missing, got: {msg}"
        );
        // `bytes_read=N` must be present. The exact N depends on how
        // many body bytes reqwest delivered before noticing the early
        // close — pinning the field shape, not the value, keeps the
        // test robust across reqwest/hyper version bumps.
        assert!(
            msg.contains("bytes_read="),
            "bytes_read field missing, got: {msg}"
        );
        // The partial buffer capture from P2 on #1064 (review): if any
        // bytes flowed through before the connection-reset, the
        // `body_prefix=` field should surface the SSE prefix so an
        // operator can see exactly where the upstream tore off.
        // Asserted as a shape check — the exact prefix depends on how
        // many bytes reqwest delivered before noticing the early close,
        // which is timing-sensitive.
        if msg.contains("body_prefix=") {
            assert!(
                msg.contains("data:"),
                "body_prefix present but doesn't include SSE prefix, got: {msg}"
            );
        }
    }

    // ------------------------------------------------------------------
    // #1163: a slow / stalled response *body* must fault *promptly* via the
    // body-read per-chunk inactivity timeout (`stream_chunk_timeout_secs`,
    // applied inside `read_body_with_idle_timeout` for the buffered path and
    // `stream_response` for the streaming path) rather than hanging until the
    // total `.timeout()` deadline. The reported failure was
    // `minimax/minimax-m3` on openrouter returning a buffered
    // `application/json` body that stalled mid-transfer — with no inactivity
    // guard on the buffered path, `complete()` hung the *whole* window before
    // surfacing "LLM response decode failed … operation timed out".
    //
    // The first two tests build a client whose per-chunk window (1s) is much
    // shorter than its total deadline (30s) and point it at a responder that
    // sends headers + a partial body, then stalls. The error must surface in
    // ~1s (the body-read window), proving the per-chunk guard — not the total
    // deadline — is what fired.
    //
    // The third test (`complete_slow_headers_succeeds_*`) is the inverse and
    // the regression guard for Tim's Option-1 rework: the inactivity guard is
    // body-only, so a healthy response that is merely slow to send its first
    // byte (delay > the per-chunk window but < the total deadline) must
    // *succeed*. The earlier client-level `.read_timeout()` mechanism also
    // capped the header wait and would have clipped this call at the per-chunk
    // window — this test fails against that head and passes after the rework.
    // ------------------------------------------------------------------

    /// Client with a short per-chunk body-read window and a comfortably longer
    /// total deadline, so a stalled-body test faults via the body-read idle
    /// guard (fast) and never via the total `.timeout()` (which would make the
    /// test slow and prove the wrong thing). Conversely, a slow-*headers* test
    /// must complete because the idle guard does not touch the header wait.
    fn openai_client_short_read_timeout(base_url: String) -> LlmClient {
        let mut core_cfg = alms_core::config::LlmConfig::default();
        core_cfg.ensure_builtin_providers();
        core_cfg.provider = "openai".into();
        let mut runtime_cfg: LlmConfig = core_cfg.into();
        runtime_cfg.api_key = "openai-test-key".into();
        runtime_cfg.base_url = base_url;
        // Total deadline far exceeds the per-chunk window — the body-read idle
        // guard must be the one that fires on a stalled body, and the total
        // deadline must be what (generously) bounds the header wait.
        runtime_cfg.timeout_secs = 30;
        runtime_cfg.stream_chunk_timeout_secs = 1;
        LlmClient::new(runtime_cfg).unwrap()
    }

    /// Buffered (`complete()`) path: a body that starts arriving (200 +
    /// partial JSON) then stalls must surface the structured decode error
    /// quickly, driven by the body-read per-chunk timeout. This is the exact
    /// #1163 symptom (`status=200`, `content_type=application/json`, body
    /// never finishes) — the assertion that it returns *at all*, well under
    /// the 30s total deadline, is the regression guard. Without the body-read
    /// idle guard this test would hang ~30s before the total deadline fired.
    #[tokio::test]
    async fn complete_stalled_body_faults_on_read_timeout_not_total_deadline() {
        // Advertise far more bytes than we send, then stall for longer than
        // the per-read window (1s) but well under the total deadline (30s).
        let partial = r#"{"id":"chatcmpl-1","object":"chat.completion","#;
        let base_url = spawn_slow_body_responder(
            200,
            "application/json",
            partial,
            8192, // claimed length the body never reaches
            // Stall longer than the *total* deadline (30s) so the only thing
            // that can fault the read at ~1s is the per-read timeout. Without
            // it, the call would block until the 30s total deadline (caught
            // by the `< 15s` assertion below).
            40,
        )
        .await;
        let client = openai_client_short_read_timeout(base_url);
        let request = CompletionRequest::new("minimax/minimax-m3")
            .with_messages(vec![LlmMessage::user("hi")]);

        let start = std::time::Instant::now();
        let err = client
            .complete(request)
            .await
            .expect_err("a stalled response body must surface an error, not hang then succeed");
        let elapsed = start.elapsed();

        // The defining assertion: we faulted via the ~1s read timeout, not
        // the 30s total deadline. A generous 15s ceiling keeps the test
        // robust on slow CI while still proving the read timeout (1s) — not
        // the total deadline (30s) — is what fired.
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "stalled body should fault on the per-read timeout (~1s), not the \
             total deadline (30s); took {elapsed:?}"
        );

        // The bubbled error is the enriched non-stream decode diagnostic
        // (#1044) — the same shape the operator saw in #1163.
        let msg = err.to_string();
        assert!(
            msg.contains("LLM response decode failed"),
            "expected the non-stream decode diagnostic, got: {msg}"
        );
        assert!(
            msg.contains("provider=openai") && msg.contains("model=minimax/minimax-m3"),
            "diagnostic must carry provider+model, got: {msg}"
        );
        // reqwest renders both the total-deadline and read-timeout faults as
        // "operation timed out" — pin that the timeout cause surfaces.
        assert!(
            msg.contains("operation timed out"),
            "expected a timeout cause in the chain, got: {msg}"
        );
    }

    /// Streaming (`complete_stream()`) path: a stalled SSE body must also
    /// terminate the stream with a timely error. The application-level
    /// per-chunk timeout in `stream_response` is the sole guard here (the
    /// client-level `.read_timeout()` was removed in the #1163 rework so the
    /// header wait isn't capped — see `LlmClient::new`). The stream must yield
    /// an `Err` well under the total deadline rather than hanging.
    #[tokio::test]
    async fn complete_stream_stalled_body_terminates_with_timely_error() {
        use futures::StreamExt;

        // A valid SSE prefix with no event terminator, then a stall: the
        // SSE parser stays in "need more data" and the per-chunk window (1s)
        // fires before the 30s total deadline.
        let partial = "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hel";
        // Stall longer than the total deadline (30s); the application-level
        // per-chunk SSE timeout (1s) in `stream_response` must catch it first.
        let base_url = spawn_slow_body_responder(200, "text/event-stream", partial, 8192, 40).await;
        let client = openai_client_short_read_timeout(base_url);
        let request = CompletionRequest::new("minimax/minimax-m3")
            .with_messages(vec![LlmMessage::user("hi")]);

        let mut stream = match client.complete_stream(request).await {
            Ok(s) => s,
            Err(e) => panic!(
                "complete_stream itself returned Err — wanted the error to surface inside the stream, got: {e}"
            ),
        };

        let start = std::time::Instant::now();
        let mut err_msg = None;
        // Bounded pump: a future regression yielding a non-erroring infinite
        // stream fails cleanly here instead of hanging the suite.
        for _ in 0..32 {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    err_msg = Some(e.to_string());
                    break;
                }
                None => break,
            }
        }
        let elapsed = start.elapsed();
        let msg = err_msg.expect("a stalled SSE body must yield a stream error, not hang");

        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "stalled stream should terminate on the per-chunk timeout (~1s), \
             not the total deadline (30s); took {elapsed:?}"
        );
        // The application-level per-chunk timeout in `stream_response` is the
        // sole guard now and renders the "LLM stream stalled" diagnostic. The
        // "LLM stream decode failed" arm is retained defensively: a real
        // mid-stream socket error (rather than a clean idle stall) would still
        // surface that shape. Accept either — both are timely, both carry the
        // model.
        assert!(
            msg.contains("LLM stream stalled") || msg.contains("LLM stream decode failed"),
            "expected a stall/decode stream diagnostic, got: {msg}"
        );
        assert!(
            msg.contains("model=minimax/minimax-m3"),
            "stream diagnostic must carry the model, got: {msg}"
        );
    }

    /// Regression guard for Tim's Option-1 rework (#1163): the inactivity
    /// guard is **body-only**, so a healthy upstream that is merely slow to
    /// send its first byte must NOT be clipped at the per-chunk window.
    ///
    /// The responder accepts the connection and sends nothing — no status
    /// line, no headers — for 3s (> the 1s `stream_chunk_timeout_secs`, well
    /// under the 30s `timeout_secs`), then writes one complete, valid OpenAI
    /// completion. With the body-only guard, the 3s delay falls entirely in
    /// the header / time-to-first-byte phase (governed only by the total
    /// deadline), so the body read never starts its per-chunk clock until the
    /// whole response is already in flight — the call succeeds.
    ///
    /// This test FAILS against the pre-rework head: the earlier client-level
    /// `.read_timeout(stream_chunk_timeout_secs)` also capped the header wait
    /// (reqwest polls that sleep while waiting for connect + TLS + headers,
    /// resetting only once the body wrapper takes over), so the 3s header
    /// delay would trip the 1s read window and surface "operation timed out"
    /// before any byte arrived. Removing the client-level read timeout and
    /// scoping the idle guard to the body read is exactly what this pins.
    #[tokio::test]
    async fn complete_slow_headers_succeeds_not_clipped_by_body_idle_guard() {
        // A complete, well-formed OpenAI completion — the responder sends it
        // in one write *after* the header delay.
        let body = r#"{"id":"chatcmpl-1","object":"chat.completion","created":1700000000,"model":"minimax/minimax-m3","choices":[{"index":0,"message":{"role":"assistant","content":"hello from a slow-to-start upstream"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":7,"total_tokens":10}}"#;
        // 3s > stream_chunk_timeout_secs (1s) and << timeout_secs (30s): a
        // healthy-but-slow-to-start response. The body-only guard must let it
        // through; the pre-rework client `.read_timeout()` would have clipped
        // it at ~1s.
        let base_url = spawn_slow_headers_responder(200, "application/json", body, 3).await;
        let client = openai_client_short_read_timeout(base_url);
        let request = CompletionRequest::new("minimax/minimax-m3")
            .with_messages(vec![LlmMessage::user("hi")]);

        let start = std::time::Instant::now();
        let resp = client.complete(request).await.expect(
            "a healthy response that is slow to send its first byte (under the total \
             deadline) must succeed — the inactivity guard is body-only and must not \
             cap the header wait",
        );
        let elapsed = start.elapsed();

        // It really did wait for the slow headers (so we exercised the
        // header-wait path, not some fast-path shortcut) but finished well
        // under the total deadline.
        assert!(
            elapsed >= std::time::Duration::from_secs(3),
            "expected the call to wait out the ~3s header delay, took only {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "expected completion well under the 30s total deadline, took {elapsed:?}"
        );

        // The body parsed into the real completion — proves we got past
        // headers AND read the body to completion under the per-chunk guard.
        let text = resp
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or_default();
        assert_eq!(text, "hello from a slow-to-start upstream");
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
