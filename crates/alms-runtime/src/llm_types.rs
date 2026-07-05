use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default value for `ToolCall::call_type` — always `"function"`.
fn default_tool_call_type() -> String {
    "function".to_string()
}

/// LLM message for API calls.
///
/// Uses a custom `Deserialize` implementation rather than serde's standard
/// `alias` attribute for the reasoning field because OpenAI may emit both
/// `reasoning` (raw chain-of-thought) and `reasoning_summary`
/// (user-visible summary) on the same message; serde's `alias` rejects
/// that as a duplicate-field error. The custom impl accepts all three
/// field names and applies a priority rule: `reasoning_content` (the
/// canonical name used by DeepSeek / xAI / OpenRouter) → `reasoning_summary`
/// (OpenAI's preferred user-visible version) → `reasoning` (OpenAI's raw
/// trace) — in that order. The first non-empty value wins, so the
/// "prefer summary when both present" semantics required by #768 is
/// preserved even across JSON field orderings.
#[derive(Debug, Clone, Serialize)]
pub struct LlmMessage {
    pub role: String,
    /// Content can be null when the LLM returns tool calls only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Reasoning/thinking content returned by reasoning models.
    ///
    /// Populated from whichever wire field the provider uses:
    /// - `reasoning_content` — DeepSeek R1, xAI Grok reasoning variants,
    ///   OpenRouter-normalized responses.
    /// - `reasoning_summary` — OpenAI o-series user-visible summary
    ///   (preferred when sent alongside raw `reasoning`).
    /// - `reasoning` — OpenAI o-series raw chain-of-thought.
    ///
    /// Callers can fall back to this field when `content` is null (common
    /// when `max_tokens` is hit before the model transitions from
    /// reasoning to output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl<'de> Deserialize<'de> for LlmMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Intermediate struct that collects every possible wire field
        // name. Serde silently ignores unknown fields by default (we do
        // NOT set `#[serde(deny_unknown_fields)]` here on purpose —
        // OpenAI / xAI / DeepSeek add new response fields regularly, and
        // breaking on unknowns would be fragile). The three reasoning
        // fields below are enumerated explicitly so the priority logic
        // that follows can pick the winner.
        #[derive(Deserialize)]
        struct Raw {
            role: String,
            #[serde(default)]
            content: Option<String>,
            #[serde(default)]
            reasoning_content: Option<String>,
            #[serde(default)]
            reasoning_summary: Option<String>,
            #[serde(default)]
            reasoning: Option<String>,
            #[serde(default)]
            tool_calls: Option<Vec<ToolCall>>,
            #[serde(default)]
            tool_call_id: Option<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        // Priority: canonical `reasoning_content` first (what DeepSeek /
        // xAI / OpenRouter send), then OpenAI's summary (user-visible),
        // then OpenAI's raw trace. Only non-empty strings win — an empty
        // string on one field still falls through to the next.
        let reasoning_content = raw
            .reasoning_content
            .filter(|s| !s.is_empty())
            .or_else(|| raw.reasoning_summary.filter(|s| !s.is_empty()))
            .or_else(|| raw.reasoning.filter(|s| !s.is_empty()));

        Ok(LlmMessage {
            role: raw.role,
            content: raw.content,
            reasoning_content,
            tool_calls: raw.tool_calls,
            tool_call_id: raw.tool_call_id,
        })
    }
}

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(calls);
        self
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Get content as string, defaulting to empty string if None.
    pub fn content_str(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }

    /// Get effective content -- `content` if present and non-empty,
    /// otherwise `reasoning_content` as a fallback.  Useful for non-streaming
    /// calls where a reasoning model may consume all `max_tokens` on thinking
    /// before producing output content, leaving `content` as `null` (or empty)
    /// while `reasoning_content` holds the model's response.
    pub fn effective_content(&self) -> Option<&str> {
        self.content
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.reasoning_content.as_deref())
    }
}

/// Tool definition for LLM — serializes to OpenAI format:
/// `{"type": "function", "function": {"name", "description", "parameters"}}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

/// The function definition inside a tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        }
    }

    pub fn with_parameters(mut self, params: Value) -> Self {
        self.function.parameters = params;
        self
    }
}

/// Tool call from LLM.
///
/// Serializes to the OpenAI format: `{"id": "...", "type": "function", "function": {...}}`.
/// The `type` field is required by the OpenAI spec and enforced by strict providers
/// (e.g. Z.AI via OpenRouter). It defaults to `"function"` on deserialization so
/// responses that omit it (common with many providers) still parse correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// Always `"function"` — required by the OpenAI tool-call wire format.
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

impl ToolCall {
    /// Create a new tool call with `type` defaulting to `"function"`.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Normalize a serialized tool-arguments string into a JSON object.
///
/// Tool arguments are spec'd as JSON objects on every provider we
/// integrate with (Anthropic, OpenAI-compat, Gemini), so the result is
/// always a [`serde_json::Value::Object`] regardless of what the wire
/// shape was. Three cases:
///
/// 1. **Empty / whitespace-only `args`** → `{}`. Anthropic's streaming
///    protocol simply omits the `input_json_delta` event when the model
///    calls a no-args tool, so the streaming accumulator observes
///    `arguments == ""`. `serde_json::from_str("")` is an EOF error,
///    and the legacy fallback (`Value::String(args)`) poisoned the
///    persisted shape: round-tripping through SQLite re-emitted
///    `tool_use.input: ""` on the wire, which Anthropic rejects with
///    `400 invalid_request_error: "Input should be an object"`.
///
/// 2. **Valid JSON that already parses to an object** → returned verbatim.
///
/// 3. **Anything else** (parse error, scalar, array): wrapped under
///    `{"_raw": "<the original string>"}` with a `tracing::warn!` so
///    the model still sees the raw value but we never violate the
///    object-shape contract on the wire. Mirrors the wrapper Gemini
///    has used since day one.
///
/// Used at every persistence + adapter boundary that owns the
/// `arguments: String` → `params: Value` conversion. See issue #967.
pub fn normalize_tool_args(args: &str) -> serde_json::Value {
    if args.trim().is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str::<serde_json::Value>(args) {
        Ok(v) if v.is_object() => v,
        Ok(other) => {
            tracing::warn!(
                "Tool arguments parsed to non-object value ({}); wrapping under `_raw` for object-shape contract",
                match &other {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "bool",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object", // unreachable, covered above
                }
            );
            serde_json::json!({ "_raw": args })
        }
        Err(e) => {
            tracing::warn!(
                "Malformed tool arguments JSON: {}; wrapping under `_raw`",
                e
            );
            serde_json::json!({ "_raw": args })
        }
    }
}

/// LLM completion request
#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Extended-thinking budget, in tokens, for Anthropic Claude 4.x.
    ///
    /// - `None` or `Some(0)`: extended thinking is disabled for this request.
    /// - `Some(n)` with `n > 0`: the Anthropic adapter injects
    ///   `"thinking": {"type": "enabled", "budget_tokens": n}` into the
    ///   outgoing request body.
    ///
    /// Never serialized on the wire as-is — the Anthropic adapter reads it
    /// and rewrites it into the provider-shaped field; other providers
    /// silently ignore it.
    #[serde(skip)]
    pub thinking_budget_tokens: Option<u32>,
    /// OpenAI-compat reasoning effort — serialized as `reasoning_effort`
    /// on the OpenAI chat-completions wire only when the effective
    /// provider is OpenAI-compatible AND the model supports the field.
    ///
    /// The adapter in [`crate::llm_client`] is responsible for stripping
    /// this value for:
    /// - Non-OpenAI wire protocols (Anthropic, Gemini) — they use their
    ///   own reasoning knobs and would 400 on the unknown field.
    /// - DeepSeek R1 (`deepseek-reasoner`) — reasoning is implicit; the
    ///   endpoint rejects `reasoning_effort`.
    /// - Non-reasoning OpenAI models (gpt-4o, etc.) — reject unknown
    ///   params.
    ///
    /// See `ReasoningEffort` and `[llm.openai].reasoning_effort` in
    /// `alms.toml` for the server-default knob. (#768)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Opt-in to Anthropic prompt caching (#766).
    ///
    /// Consumed by the Anthropic adapter in [`crate::anthropic`] — when
    /// `Some(true)`, the adapter attaches `cache_control` markers to the
    /// last tool definition and the trailing system content block. Other
    /// providers ignore the field.
    ///
    /// Populated by the agent loop from the server's
    /// `[llm.anthropic].prompt_cache_enabled` config value. Never
    /// serialized on the wire as-is — the adapter reads it and rewrites
    /// the outgoing shape.
    #[serde(skip)]
    pub prompt_cache_enabled: Option<bool>,
    /// Gemini thinking budget, in tokens (#769).
    ///
    /// Consumed by the Gemini adapter — when `Some(n)` with `n > 0`, the
    /// adapter injects `generationConfig.thinkingConfig: { thinkingBudget: n,
    /// includeThoughts: true }` into the outgoing request. `None` or
    /// `Some(0)` disables extended thinking. Other providers ignore the
    /// field.
    ///
    /// Populated by the agent loop from the server's
    /// `[llm.gemini].thinking_budget` config value (with the two-layer
    /// precedence chain — per-agent > server default — applied by the
    /// gateway). Never serialized on the wire as-is.
    #[serde(skip)]
    pub gemini_thinking_budget: Option<u32>,
    /// Opt-in to Gemini explicit context caching (#769).
    ///
    /// Consumed by the Gemini adapter — when `Some(true)`, the adapter
    /// looks up or creates a `cachedContents` resource keyed by
    /// `session_id` + prefix hash, and references it on subsequent turns
    /// via `cachedContent: "cachedContents/<id>"`. `None` or `Some(false)`
    /// skips the cache interaction entirely. Other providers ignore the
    /// field.
    ///
    /// Populated by the agent loop from the server's
    /// `[llm.gemini].cache_enabled` config value.
    #[serde(skip)]
    pub gemini_cache_enabled: Option<bool>,
    /// Gemini cache TTL in seconds (#769). Sent as `ttl` when creating a
    /// new `cachedContents` entry. `None` falls back to Gemini's server
    /// default (1 hour). Ignored when `gemini_cache_enabled` is not
    /// `Some(true)` and by non-Gemini providers.
    #[serde(skip)]
    pub gemini_cache_ttl_seconds: Option<u64>,
    /// Session ID for provider-scoped caches (#769).
    ///
    /// Gemini's context-caching adapter uses this as the key in the
    /// in-process cache map so that two concurrent sessions don't share
    /// a cache handle (prefixes differ per agent/session). `None`
    /// disables cache reuse across turns, which falls back to the
    /// no-cache behaviour.
    #[serde(skip)]
    pub session_id: Option<alms_core::SessionId>,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: None,
            stream_options: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            prompt_cache_enabled: None,
            gemini_thinking_budget: None,
            gemini_cache_enabled: None,
            gemini_cache_ttl_seconds: None,
            session_id: None,
        }
    }

    pub fn with_messages(mut self, messages: Vec<LlmMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    pub fn with_max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = Some(tokens);
        self
    }

    /// Set the extended-thinking budget for this request. A value of `0`
    /// disables extended thinking; see [`Self::thinking_budget_tokens`].
    pub fn with_thinking_budget(mut self, budget_tokens: u32) -> Self {
        self.thinking_budget_tokens = Some(budget_tokens);
        self
    }

    /// Set the OpenAI-compat reasoning effort for this request.
    ///
    /// Value must be one of `"low" | "medium" | "high" | "minimal"` —
    /// construct from [`alms_core::config::ReasoningEffort::as_wire_str`]
    /// for compile-time validation. See [`Self::reasoning_effort`] for
    /// when the field is actually emitted on the wire. (#768)
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Enable Anthropic prompt caching for this request (#766).
    ///
    /// The Anthropic adapter reads this flag and attaches `cache_control`
    /// markers to the last tool definition and the trailing system block.
    /// Other provider adapters ignore the field.
    pub fn with_prompt_cache_enabled(mut self, enabled: bool) -> Self {
        self.prompt_cache_enabled = Some(enabled);
        self
    }

    /// Set the Gemini thinking budget (#769). See [`Self::gemini_thinking_budget`].
    pub fn with_gemini_thinking_budget(mut self, budget: u32) -> Self {
        self.gemini_thinking_budget = Some(budget);
        self
    }

    /// Enable Gemini explicit context caching for this request (#769).
    /// See [`Self::gemini_cache_enabled`].
    pub fn with_gemini_cache_enabled(mut self, enabled: bool) -> Self {
        self.gemini_cache_enabled = Some(enabled);
        self
    }

    /// Set the Gemini cache TTL in seconds (#769). See [`Self::gemini_cache_ttl_seconds`].
    pub fn with_gemini_cache_ttl(mut self, secs: u64) -> Self {
        self.gemini_cache_ttl_seconds = Some(secs);
        self
    }

    /// Attach a session ID so provider-scoped caches can key off it (#769).
    /// See [`Self::session_id`].
    pub fn with_session_id(mut self, id: alms_core::SessionId) -> Self {
        self.session_id = Some(id);
        self
    }
}

/// LLM completion response
#[derive(Debug, Clone, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Response choice
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: LlmMessage,
    #[serde(rename = "finish_reason")]
    pub finish_reason: Option<String>,
}

/// Nested OpenAI detail object: `usage.completion_tokens_details.reasoning_tokens`.
///
/// DeepSeek and xAI omit this nesting and may place `reasoning_tokens`
/// directly on `usage`; we handle that shape via a sibling field on
/// [`Usage`]. The first non-`None` of the two paths wins at read time via
/// [`Usage::reasoning_tokens_effective`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
}

/// Token usage
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Flat `reasoning_tokens` field emitted directly on `usage` by
    /// DeepSeek R1 and some xAI responses. OpenAI o-series nests the
    /// count under `completion_tokens_details` instead — see
    /// [`Usage::completion_tokens_details`] below. Callers should
    /// consult [`Usage::reasoning_tokens_effective`] to get the
    /// effective count regardless of wire shape.
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    /// OpenAI-shaped detail object carrying the reasoning-tokens count.
    /// Absent on DeepSeek / xAI payloads; hence `Option`.
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    /// Anthropic prompt caching (#766): tokens *written* to the cache on
    /// this request. Populated only by the Anthropic adapter; other
    /// providers leave it as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Prompt caching: tokens *served from* the cache on this request.
    ///
    /// **Provider-neutral field.** The name is historically Anthropic-coloured
    /// (mirrors Anthropic's wire field) but this slot is shared across
    /// providers that report "tokens served from cache". When an operator
    /// sees this populated in a `run_finished` event, the provider can be
    /// disambiguated via `llm.provider` on the run config or the run's
    /// model name.
    ///
    /// - Anthropic (#766): populated from the response's
    ///   `cache_read_input_tokens` field.
    /// - Gemini (#769): populated from `usageMetadata.cachedContentTokenCount`
    ///   when a `cachedContents` entry was referenced.
    ///
    /// `None` for non-caching providers, or when caching was disabled /
    /// missed on this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

impl Usage {
    /// Return the effective reasoning-token count, preferring OpenAI's
    /// nested shape (`completion_tokens_details.reasoning_tokens`) when
    /// present, falling back to the flat field used by DeepSeek / xAI.
    /// Returns `None` when neither path carries a value.
    pub fn reasoning_tokens_effective(&self) -> Option<u32> {
        self.completion_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .or(self.reasoning_tokens)
    }
}

/// Streaming chunk
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Streaming choice
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(rename = "finish_reason")]
    pub finish_reason: Option<String>,
}

/// Delta in streaming response.
///
/// Uses a custom `Deserialize` for the same reason as [`LlmMessage`] —
/// OpenAI may emit `reasoning` and `reasoning_summary` deltas in
/// separate SSE chunks or within the same delta object; the custom impl
/// coalesces them into `reasoning_content` with priority ordering
/// (canonical > summary > raw).
#[derive(Debug, Clone, Default)]
pub struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
    /// Reasoning/thinking content delta from reasoning models. See
    /// `LlmMessage::reasoning_content` for wire-shape details. (#768)
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

impl<'de> Deserialize<'de> for Delta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            role: Option<String>,
            #[serde(default)]
            content: Option<String>,
            #[serde(default)]
            reasoning_content: Option<String>,
            #[serde(default)]
            reasoning_summary: Option<String>,
            #[serde(default)]
            reasoning: Option<String>,
            #[serde(default)]
            tool_calls: Option<Vec<ToolCallDelta>>,
        }

        let raw = Raw::deserialize(deserializer)?;
        // Priority ordering matches `LlmMessage`: canonical field first,
        // then OpenAI summary, then OpenAI raw. Empty strings fall
        // through; only non-empty values win.
        let reasoning_content = raw
            .reasoning_content
            .filter(|s| !s.is_empty())
            .or_else(|| raw.reasoning_summary.filter(|s| !s.is_empty()))
            .or_else(|| raw.reasoning.filter(|s| !s.is_empty()));

        Ok(Delta {
            role: raw.role,
            content: raw.content,
            reasoning_content,
            tool_calls: raw.tool_calls,
        })
    }
}

/// Incremental tool call in streaming responses.
/// Unlike `ToolCall`, all fields except `index` are optional because they
/// arrive piece-by-piece across multiple chunks.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionCallDelta>,
}

/// Incremental function call data in streaming responses.
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Request `stream_options` to include usage in streaming responses.
#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Configuration for LLM client.
/// Note: prefer alms_core::config::LlmConfig for new code.
/// This type is kept for backward compatibility with existing runtime/gateway code.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub default_model: String,
    /// Total per-request deadline (seconds) — reqwest's `.timeout()`,
    /// covering connect + headers + full response body. The only bound on
    /// the header / time-to-first-byte wait (the body-only idle guard below
    /// never touches it).
    pub timeout_secs: u64,
    pub mock: bool,
    /// Per-chunk body-read inactivity timeout (seconds): applied only to the
    /// response *body* on both paths — the application-level per-chunk SSE
    /// timeout in `stream_response` and the same per-chunk guard wrapping the
    /// buffered body read in `read_body_with_idle_timeout` — so a stalled
    /// body (streaming or buffered) faults within this window rather than
    /// hanging until `timeout_secs`. It is deliberately NOT a client-level
    /// `.read_timeout()`, which would also cap the header wait (#1163).
    pub stream_chunk_timeout_secs: u64,
    /// How the API key is attached to each outgoing request. Defaults to
    /// `Bearer` for OpenAI-compatible providers.
    #[serde(default)]
    pub auth_scheme: alms_core::config::AuthScheme,
    /// Provider-specific behaviour tweaks applied at request-build time.
    #[serde(default)]
    pub quirks: alms_core::config::ProviderQuirks,
    /// Snapshot of `[llm.providers]` from the unified `AlmsConfig`. Used
    /// by `with_provider_and_secrets` so that switching to a different
    /// configured provider picks up its `base_url` / `auth_scheme` / quirks.
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, alms_core::config::ProviderEntry>,
    /// Snapshot of `[llm.anthropic]` from the unified `AlmsConfig`. Consumed
    /// by the Anthropic adapter in `anthropic.rs`; ignored for other providers.
    #[serde(default)]
    pub anthropic: alms_core::config::AnthropicConfig,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".to_string(),
            api_key: String::new(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            default_model: "z-ai/glm-5.2".to_string(),
            // Mirror of `alms_core::config::LlmConfig` defaults — keep in sync.
            // 600s = per-call HTTP deadline (heavy reasoning models); 180s =
            // per-chunk body-silence guard. See that type's rustdoc (#1163).
            timeout_secs: 600,
            mock: false,
            stream_chunk_timeout_secs: 180,
            auth_scheme: alms_core::config::AuthScheme::default(),
            quirks: alms_core::config::ProviderQuirks::default(),
            providers: std::collections::BTreeMap::new(),
            anthropic: alms_core::config::AnthropicConfig::default(),
        }
    }
}

impl From<alms_core::config::LlmConfig> for LlmConfig {
    fn from(c: alms_core::config::LlmConfig) -> Self {
        // Merge the resolved provider entry (if any) into the flat runtime
        // LlmConfig. The provider entry's base_url / auth_scheme / quirks
        // always win over the flat fields when present, because the
        // generic form is strictly more expressive.
        let entry = c.providers.get(&c.provider).cloned();

        let (base_url, auth_scheme, quirks, model) = match entry {
            Some(e) => {
                let model = e.model.clone().unwrap_or_else(|| c.model.clone());
                (e.base_url, e.auth_scheme, e.quirks, model)
            }
            None => (
                c.base_url.clone(),
                alms_core::config::AuthScheme::default(),
                alms_core::config::ProviderQuirks::default(),
                c.model.clone(),
            ),
        };

        Self {
            provider: c.provider,
            api_key: c.api_key.unwrap_or_default(),
            base_url,
            default_model: model,
            timeout_secs: c.timeout_secs,
            mock: c.mock,
            stream_chunk_timeout_secs: c.stream_chunk_timeout_secs,
            auth_scheme,
            quirks,
            providers: c.providers,
            anthropic: c.anthropic,
        }
    }
}

impl LlmConfig {
    // TODO(dead-code): test-only helper (~30 lines) — consider gating behind #[cfg(test)]
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(provider) = std::env::var("ALMS_LLM_PROVIDER") {
            config.provider = provider.to_lowercase();
        }
        // NOTE: API key is NOT loaded from env vars. Use `alms auth set`.

        if let Ok(base_url) =
            std::env::var("ALMS_LLM_BASE_URL").or_else(|_| std::env::var("LLM_BASE_URL"))
        {
            config.base_url = base_url;
        }

        if let Ok(model) =
            std::env::var("ALMS_LLM_MODEL").or_else(|_| std::env::var("DEFAULT_MODEL"))
        {
            config.default_model = model;
        }

        if let Ok(mock) = std::env::var("ALMS_LLM_MOCK") {
            let mock = mock.to_lowercase();
            config.mock = mock == "1" || mock == "true" || mock == "yes";
        }

        if let Ok(val) = std::env::var("ALMS_LLM_STREAM_CHUNK_TIMEOUT")
            && let Ok(n) = val.parse()
        {
            config.stream_chunk_timeout_secs = n;
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const LLM_ENV_VARS: [&str; 4] = [
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "ALMS_LLM_PROVIDER",
    ];

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    fn set_env_var(name: &str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }

    fn remove_env_var(name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
    }

    impl EnvGuard {
        fn set(overrides: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let saved = LLM_ENV_VARS
                .iter()
                .map(|name| (*name, std::env::var(name).ok()))
                .collect::<Vec<_>>();

            for name in LLM_ENV_VARS {
                remove_env_var(name);
            }
            for (name, value) in overrides {
                match value {
                    Some(v) => set_env_var(name, v),
                    None => remove_env_var(name),
                }
            }

            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(v) => set_env_var(name, v),
                    None => remove_env_var(name),
                }
            }
        }
    }

    #[test]
    fn test_from_env_does_not_load_api_keys() {
        let _guard = EnvGuard::set(&[
            ("ALMS_LLM_PROVIDER", Some("openai")),
            ("OPENROUTER_API_KEY", Some("openrouter-key")),
            ("OPENAI_API_KEY", Some("openai-key")),
            ("ANTHROPIC_API_KEY", Some("anthropic-key")),
        ]);

        let config = LlmConfig::from_env();
        assert_eq!(config.provider, "openai");
        // API key must NOT be loaded from env vars (security fix).
        assert_eq!(config.api_key, "");
    }

    #[test]
    fn test_from_env_sets_provider_without_key() {
        let _guard = EnvGuard::set(&[("ALMS_LLM_PROVIDER", Some("anthropic"))]);

        let config = LlmConfig::from_env();
        assert_eq!(config.provider, "anthropic");
        assert_eq!(config.api_key, "");
    }

    // -- effective_content / reasoning_content tests --------------------------

    #[test]
    fn test_effective_content_prefers_content() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: Some("real content".into()),
            reasoning_content: Some("thinking...".into()),
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(msg.effective_content(), Some("real content"));
    }

    #[test]
    fn test_effective_content_falls_back_to_reasoning() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: Some("reasoning text".into()),
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(msg.effective_content(), Some("reasoning text"));
    }

    #[test]
    fn test_effective_content_empty_string_falls_back_to_reasoning() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: Some("".into()),
            reasoning_content: Some("reasoning fallback".into()),
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(msg.effective_content(), Some("reasoning fallback"));
    }

    #[test]
    fn test_effective_content_none_when_both_absent() {
        let msg = LlmMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        assert_eq!(msg.effective_content(), None);
    }

    #[test]
    fn test_reasoning_content_deserialized_from_json() {
        // Simulate the response from a reasoning model via OpenRouter
        let json = r#"{
            "role": "assistant",
            "content": null,
            "reasoning_content": "Let me think about this summary..."
        }"#;
        let msg: LlmMessage = serde_json::from_str(json).unwrap();
        assert!(msg.content.is_none());
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("Let me think about this summary...")
        );
        assert_eq!(
            msg.effective_content(),
            Some("Let me think about this summary...")
        );
    }

    #[test]
    fn test_reasoning_content_not_serialized_when_none() {
        let msg = LlmMessage::assistant("hello");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            !json.contains("reasoning_content"),
            "reasoning_content should be skipped when None: {json}"
        );
    }

    // -- ToolCall type field tests (fixes "Tool type cannot be empty" on strict providers) --

    #[test]
    fn test_tool_call_serializes_type_function() {
        let tc = ToolCall::new("call_1", "echo", r#"{"text":"hi"}"#);
        let json = serde_json::to_string(&tc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(
            parsed.get("type").and_then(|v| v.as_str()),
            Some("function"),
            "ToolCall must serialize with \"type\": \"function\": {json}"
        );
        assert_eq!(parsed.get("id").and_then(|v| v.as_str()), Some("call_1"));
        assert!(parsed.get("function").is_some());
    }

    #[test]
    fn test_tool_call_deserializes_with_type() {
        let json =
            r#"{"id":"call_1","type":"function","function":{"name":"echo","arguments":"{}"}}"#;
        let tc: ToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.call_type, "function");
        assert_eq!(tc.function.name, "echo");
    }

    #[test]
    fn test_tool_call_deserializes_without_type_defaults_to_function() {
        // Many providers omit "type" in tool_calls within responses.
        // Verify that deserialization defaults to "function".
        let json = r#"{"id":"call_2","function":{"name":"shell","arguments":"{}"}}"#;
        let tc: ToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(tc.id, "call_2");
        assert_eq!(
            tc.call_type, "function",
            "Missing type field should default to \"function\""
        );
        assert_eq!(tc.function.name, "shell");
    }

    #[test]
    fn test_tool_call_in_message_serializes_type() {
        // Verify that tool calls embedded in LlmMessage serialize the type field.
        let msg = LlmMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall::new("call_x", "echo", "{}")]),
            tool_call_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let tc = &parsed["tool_calls"][0];
        assert_eq!(
            tc.get("type").and_then(|v| v.as_str()),
            Some("function"),
            "tool_calls in messages must include \"type\": \"function\": {json}"
        );
    }

    // ------------------------------------------------------------------
    // normalize_tool_args (#967)
    // ------------------------------------------------------------------

    #[test]
    fn normalize_tool_args_empty_string_returns_empty_object() {
        // Anthropic's streaming protocol omits `input_json_delta`
        // entirely for no-args tool calls, so the streaming
        // accumulator observes `arguments == ""`. The pre-#967 code
        // path treated this as a parse failure and fell back to
        // `Value::String(args)`, which Anthropic 400s on the wire and
        // poisons the persisted `Content::ToolCall.params` shape.
        let v = normalize_tool_args("");
        assert!(v.is_object(), "empty args must normalize to object: {v:?}");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn normalize_tool_args_whitespace_only_returns_empty_object() {
        let v = normalize_tool_args("   \n\t  ");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn normalize_tool_args_valid_object_passes_through() {
        let v = normalize_tool_args(r#"{"path":"src/lib.rs"}"#);
        assert_eq!(v, serde_json::json!({"path": "src/lib.rs"}));
    }

    #[test]
    fn normalize_tool_args_empty_object_passes_through() {
        let v = normalize_tool_args("{}");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn normalize_tool_args_scalar_wraps_under_raw() {
        // Genuine model error: never poison the wire shape, but keep
        // the raw bytes visible to the model so it can self-correct.
        let v = normalize_tool_args("42");
        assert_eq!(v, serde_json::json!({"_raw": "42"}));
    }

    #[test]
    fn normalize_tool_args_array_wraps_under_raw() {
        let v = normalize_tool_args(r#"[1,2,3]"#);
        assert_eq!(v, serde_json::json!({"_raw": "[1,2,3]"}));
    }

    #[test]
    fn normalize_tool_args_malformed_json_wraps_under_raw() {
        let v = normalize_tool_args(r#"{"unterminated"#);
        assert_eq!(v, serde_json::json!({"_raw": r#"{"unterminated"#}));
    }

    #[test]
    fn normalize_tool_args_quoted_empty_string_wraps_under_raw() {
        // The exact poison shape from #967 round-tripping through the
        // legacy persistence path: `Value::String("")`.to_string() ==
        // "\"\"". When this enters the adapter (e.g. on a healed-but-
        // not-yet-fully-fixed code path) we want it as an object, not
        // a JSON string.
        let v = normalize_tool_args("\"\"");
        assert_eq!(v, serde_json::json!({"_raw": "\"\""}));
    }
}
