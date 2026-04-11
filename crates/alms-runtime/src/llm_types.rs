use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default value for `ToolCall::call_type` — always `"function"`.
fn default_tool_call_type() -> String {
    "function".to_string()
}

/// LLM message for API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    /// Content can be null when the LLM returns tool calls only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Reasoning/thinking content returned by reasoning models (e.g. minimax,
    /// deepseek-r1).  OpenRouter surfaces this as a separate field on the
    /// message object.  We capture it so callers can fall back to it when
    /// `content` is null (common when max_tokens is hit before the model
    /// transitions from reasoning to output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
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

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
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

/// Token usage
#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
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

/// Delta in streaming response
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
    /// Reasoning/thinking content delta from reasoning models.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
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
    pub timeout_secs: u64,
    pub mock: bool,
    /// Per-chunk read timeout for SSE streaming (seconds).
    pub stream_chunk_timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openrouter".to_string(),
            api_key: String::new(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            default_model: "moonshotai/kimi-k2.5".to_string(),
            timeout_secs: 120,
            mock: false,
            stream_chunk_timeout_secs: 60,
        }
    }
}

impl From<alms_core::config::LlmConfig> for LlmConfig {
    fn from(c: alms_core::config::LlmConfig) -> Self {
        Self {
            provider: c.provider,
            api_key: c.api_key.unwrap_or_default(),
            base_url: c.base_url,
            default_model: c.model,
            timeout_secs: c.timeout_secs,
            mock: c.mock,
            stream_chunk_timeout_secs: c.stream_chunk_timeout_secs,
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

        if let Ok(base_url) = std::env::var("LLM_BASE_URL") {
            config.base_url = base_url;
        }

        if let Ok(model) = std::env::var("DEFAULT_MODEL") {
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
        let tc = ToolCall {
            id: "call_1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "echo".to_string(),
                arguments: r#"{"text":"hi"}"#.to_string(),
            },
        };
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
            tool_calls: Some(vec![ToolCall {
                id: "call_x".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "echo".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
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
}
