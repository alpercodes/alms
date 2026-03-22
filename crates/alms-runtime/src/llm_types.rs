use serde::{Deserialize, Serialize};
use serde_json::Value;

/// LLM message for API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    /// Content can be null when the LLM returns tool calls only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(content.into()),
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
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Get content as string, defaulting to empty string if None
    pub fn content_str(&self) -> &str {
        self.content.as_deref().unwrap_or("")
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

/// Tool call from LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
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

    pub fn with_streaming(mut self) -> Self {
        self.stream = Some(true);
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
            provider: "openai".to_string(),
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
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(provider) = std::env::var("ALMS_LLM_PROVIDER") {
            config.provider = provider.to_lowercase();
        }
        if let Some(api_key) = alms_core::config::select_llm_api_key_from_env(&config.provider) {
            config.api_key = api_key;
        }

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
    fn test_from_env_prefers_openai_key_for_openai_provider() {
        let _guard = EnvGuard::set(&[
            ("ALMS_LLM_PROVIDER", Some("openai")),
            ("OPENROUTER_API_KEY", Some("openrouter-key")),
            ("OPENAI_API_KEY", Some("openai-key")),
            ("ANTHROPIC_API_KEY", Some("anthropic-key")),
        ]);

        let config = LlmConfig::from_env();
        assert_eq!(config.provider, "openai");
        assert_eq!(config.api_key, "openai-key");
    }

    #[test]
    fn test_from_env_prefers_anthropic_key_for_anthropic_provider() {
        let _guard = EnvGuard::set(&[
            ("ALMS_LLM_PROVIDER", Some("anthropic")),
            ("OPENROUTER_API_KEY", Some("openrouter-key")),
            ("OPENAI_API_KEY", Some("openai-key")),
            ("ANTHROPIC_API_KEY", Some("anthropic-key")),
        ]);

        let config = LlmConfig::from_env();
        assert_eq!(config.provider, "anthropic");
        assert_eq!(config.api_key, "anthropic-key");
    }
}
