//! Unified configuration for ALMS.
//!
//! Loads config with layered precedence:
//! 1. Compiled defaults
//! 2. Config file (alms.toml)
//! 3. Environment variables (ALMS_* prefix)
//!
//! Secrets (API keys) are ONLY loaded from env vars, never from config files.

use crate::{AlmsError, AlmsResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Top-level ALMS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlmsConfig {
    pub server: ServerConfig,
    pub llm: LlmConfig,
    pub session: SessionConfig,
    pub context: ContextConfig,
    pub tools: ToolsConfig,
    pub channels: ChannelsConfig,
}

#[allow(clippy::derivable_impls)]
impl Default for AlmsConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            llm: LlmConfig::default(),
            session: SessionConfig::default(),
            context: ContextConfig::default(),
            tools: ToolsConfig::default(),
            channels: ChannelsConfig::default(),
        }
    }
}

impl AlmsConfig {
    /// Load config with full layered precedence:
    /// defaults → config file → env vars
    pub fn load() -> AlmsResult<Self> {
        let mut config = Self::default();

        // Try to load config file
        let config_path = Self::find_config_file();
        if let Some(path) = &config_path {
            let file_config = Self::from_file(path)?;
            config = file_config;
            info!("Loaded config from {}", path.display());
        }

        // Apply env var overrides
        config.apply_env_overrides();

        // Validate
        config.validate()?;

        Ok(config)
    }

    /// Load from a specific file path
    pub fn from_file(path: &Path) -> AlmsResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            AlmsError::InvalidConfig(format!("Cannot read config file {}: {}", path.display(), e))
        })?;

        let config: Self = toml::from_str(&content).map_err(|e| {
            AlmsError::InvalidConfig(format!("Invalid config file {}: {}", path.display(), e))
        })?;

        Ok(config)
    }

    /// Find config file in standard locations
    fn find_config_file() -> Option<PathBuf> {
        // 1. Current directory
        let local = PathBuf::from("alms.toml");
        if local.exists() {
            return Some(local);
        }

        // 2. Home config directory
        if let Some(home) = dirs_path() {
            let home_config = home.join(".config").join("alms").join("config.toml");
            if home_config.exists() {
                return Some(home_config);
            }
        }

        None
    }

    /// Apply environment variable overrides.
    /// Secrets are ONLY loaded here, never from config files.
    fn apply_env_overrides(&mut self) {
        // LLM settings
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            self.llm.api_key = Some(key);
        }
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            self.llm.api_key = Some(key);
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            self.llm.api_key = Some(key);
        }
        if let Ok(provider) = std::env::var("ALMS_LLM_PROVIDER") {
            self.llm.provider = provider.to_lowercase();
        }
        if let Ok(url) = std::env::var("LLM_BASE_URL") {
            self.llm.base_url = url;
        }
        if let Ok(model) = std::env::var("DEFAULT_MODEL") {
            self.llm.model = model;
        }
        if let Ok(val) = std::env::var("ALMS_LLM_MOCK") {
            self.llm.mock = matches!(val.to_lowercase().as_str(), "1" | "true" | "yes");
        }

        // Server settings
        if let Ok(bind) = std::env::var("ALMS_BIND") {
            self.server.bind = bind;
        }
        if let Ok(token) = std::env::var("ALMS_AUTH_TOKEN") {
            self.server.auth_token = Some(token);
        }

        // Channels
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            self.channels.telegram_token = Some(token);
        }

        // Tools / sandbox settings
        if let Ok(val) = std::env::var("ALMS_SANDBOX_ROOT") {
            self.tools.sandbox_root = val;
        }
        if let Ok(val) = std::env::var("ALMS_SHELL_POLICY") {
            self.tools.shell_policy = val;
        }

        // Context settings
        if let Ok(val) = std::env::var("ALMS_CONTEXT_STRATEGY") {
            self.context.strategy = val;
        }
        if let Ok(val) = std::env::var("ALMS_MAX_INPUT_TOKENS")
            && let Ok(n) = val.parse()
        {
            self.context.max_input_tokens = n;
        }
    }

    /// Validate config. Returns error on invalid values.
    pub fn validate(&self) -> AlmsResult<()> {
        // LLM validation
        if !self.llm.mock && self.llm.api_key.is_none() {
            warn!(
                "No LLM API key configured. Set OPENROUTER_API_KEY, OPENAI_API_KEY, or ANTHROPIC_API_KEY, or enable mock mode with ALMS_LLM_MOCK=1"
            );
        }

        let valid_providers = ["openai", "anthropic"];
        if !valid_providers.contains(&self.llm.provider.as_str()) {
            return Err(AlmsError::InvalidConfig(format!(
                "llm.provider must be one of {:?}, got '{}'",
                valid_providers, self.llm.provider
            )));
        }

        if self.llm.timeout_secs == 0 {
            return Err(AlmsError::InvalidConfig(
                "llm.timeout_secs must be > 0".into(),
            ));
        }

        // Context validation
        let valid_strategies = ["sliding-summary", "full", "truncate"];
        if !valid_strategies.contains(&self.context.strategy.as_str()) {
            return Err(AlmsError::InvalidConfig(format!(
                "context.strategy must be one of {:?}, got '{}'",
                valid_strategies, self.context.strategy
            )));
        }

        if self.context.max_input_tokens == 0 {
            return Err(AlmsError::InvalidConfig(
                "context.max_input_tokens must be > 0".into(),
            ));
        }

        if self.context.recent_window == 0 {
            return Err(AlmsError::InvalidConfig(
                "context.recent_window must be > 0".into(),
            ));
        }

        // Cross-section validation: session storage must hold at least one
        // full context window, otherwise the ContextBuilder could request more
        // tokens than the session retains.
        if self.session.max_context_tokens < self.context.max_input_tokens {
            return Err(AlmsError::InvalidConfig(format!(
                "session.max_context_tokens ({}) must be >= context.max_input_tokens ({}) — \
                 the session storage limit must be at least as large as the LLM context window budget",
                self.session.max_context_tokens, self.context.max_input_tokens
            )));
        }

        // Tools validation
        if self.tools.timeout_secs == 0 {
            return Err(AlmsError::InvalidConfig(
                "tools.timeout_secs must be > 0".into(),
            ));
        }

        let valid_policies = ["sandboxed", "unrestricted"];
        if !valid_policies.contains(&self.tools.shell_policy.as_str()) {
            return Err(AlmsError::InvalidConfig(format!(
                "tools.shell_policy must be one of {:?}, got '{}'",
                valid_policies, self.tools.shell_policy
            )));
        }

        Ok(())
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    /// Bearer token for API authentication — loaded from env only
    #[serde(skip)]
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            auth_token: None,
        }
    }
}

/// LLM provider configuration.
/// Note: api_key is only loaded from env vars, never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Provider: "openai" (default, OpenRouter-compatible) or "anthropic"
    pub provider: String,
    pub base_url: String,
    pub model: String,
    #[serde(skip)]
    pub api_key: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub max_tokens_per_run: u32,
    pub mock: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "moonshotai/kimi-k2.5".into(),
            api_key: None,
            timeout_secs: 120,
            max_retries: 2,
            max_tokens_per_run: 0,
            mock: false,
        }
    }
}

/// Session management configuration.
///
/// Controls how sessions are stored and retained. This is distinct from
/// [`ContextConfig`], which controls how much of a session's history is sent
/// to the LLM in a single request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Idle timeout before archiving (seconds)
    pub idle_timeout_secs: u64,
    pub auto_archive: bool,
    /// Delete archived sessions after this many seconds
    pub archive_ttl_secs: u64,
    pub max_messages: usize,
    /// Maximum total tokens to retain in a session's history.
    ///
    /// This is a **storage** limit — it caps how much conversation history the
    /// session keeps on disk/in the database. It should be >= `context.max_input_tokens`
    /// because the session must store at least as much history as the LLM context
    /// window can consume per request.
    ///
    /// Not to be confused with [`ContextConfig::max_input_tokens`], which controls
    /// how many tokens are sent to the LLM in a single request.
    pub max_context_tokens: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 24 * 60 * 60,
            auto_archive: true,
            archive_ttl_secs: 30 * 24 * 60 * 60,
            max_messages: 10000,
            max_context_tokens: 256_000,
        }
    }
}

/// Context window management configuration.
///
/// Controls how the session's message history is assembled into a prompt for
/// each LLM request. This is distinct from [`SessionConfig`], which controls
/// how much history the session retains in storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Strategy: "sliding-summary", "full", "truncate"
    pub strategy: String,
    /// Maximum tokens to send to the LLM in a single request.
    ///
    /// This is the **per-request** token budget for the context window assembled
    /// by the ContextBuilder. It should match your LLM's context window size.
    ///
    /// Not to be confused with [`SessionConfig::max_context_tokens`], which is
    /// the total token storage limit for the session's history on disk.
    pub max_input_tokens: usize,
    /// Number of recent messages to always keep in full
    pub recent_window: usize,
    /// How often to trigger a new summary (in uncovered messages beyond recent_window)
    pub summary_interval: usize,
    /// Optional separate (cheaper) model for generating summaries.
    /// Falls back to the agent's default model when None.
    pub summary_model: Option<String>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            strategy: "truncate".into(),
            max_input_tokens: 128_000,
            recent_window: 20,
            summary_interval: 30,
            summary_model: None,
        }
    }
}

/// Tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub enabled: Vec<String>,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    /// Filesystem sandbox root. Relative paths are resolved from cwd.
    /// Default: "." (current directory — safe by default).
    /// Set to "" for unrestricted filesystem access.
    pub sandbox_root: String,
    /// Shell execution policy: "sandboxed" (default) or "unrestricted".
    /// In sandboxed mode, shell_exec cwd is restricted to sandbox_root.
    /// Note: sandboxed mode restricts cwd but cannot prevent the executed
    /// command from accessing files outside the sandbox. For true shell
    /// isolation, use a restricted OS user or Landlock (see security-model.md §4.4).
    pub shell_policy: String,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["echo".into(), "math".into(), "http_get".into()],
            timeout_secs: 30,
            max_output_bytes: 65536,
            sandbox_root: ".".into(),
            shell_policy: "sandboxed".into(),
        }
    }
}

/// Channel configuration (Telegram, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    /// Telegram bot token — loaded from env only
    #[serde(skip)]
    pub telegram_token: Option<String>,
    pub telegram_poll_interval_secs: u64,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            telegram_token: None,
            telegram_poll_interval_secs: 5,
        }
    }
}

/// Get home directory path (cross-platform)
fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AlmsConfig::default();
        assert_eq!(config.server.bind, "127.0.0.1:8080");
        assert_eq!(config.context.strategy, "truncate");
        assert_eq!(config.context.recent_window, 20);
        assert!(config.llm.api_key.is_none());
    }

    #[test]
    fn test_config_from_toml() {
        let toml = r#"
[server]
bind = "0.0.0.0:9090"

[llm]
model = "gpt-4"
timeout_secs = 60

[context]
strategy = "sliding-summary"
max_input_tokens = 16000
recent_window = 10
"#;
        let config: AlmsConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.server.bind, "0.0.0.0:9090");
        assert_eq!(config.llm.model, "gpt-4");
        assert_eq!(config.llm.timeout_secs, 60);
        assert_eq!(config.context.strategy, "sliding-summary");
        assert_eq!(config.context.max_input_tokens, 16000);
        // Defaults preserved for unset fields
        assert_eq!(config.tools.timeout_secs, 30);
    }

    #[test]
    fn test_partial_toml() {
        // Only override one section — everything else keeps defaults
        let toml = r#"
[llm]
model = "claude-sonnet"
"#;
        let config: AlmsConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.model, "claude-sonnet");
        assert_eq!(config.server.bind, "127.0.0.1:8080"); // default
        assert_eq!(config.context.recent_window, 20); // default
    }

    #[test]
    fn test_validation_bad_strategy() {
        let mut config = AlmsConfig::default();
        config.context.strategy = "invalid".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_zero_timeout() {
        let mut config = AlmsConfig::default();
        config.llm.timeout_secs = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_bad_shell_policy() {
        let mut config = AlmsConfig::default();
        config.tools.shell_policy = "yolo".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_good() {
        let config = AlmsConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_context_tokens_less_than_input_tokens() {
        let mut config = AlmsConfig::default();
        // Session storage smaller than LLM context window — should fail
        config.session.max_context_tokens = 64_000;
        config.context.max_input_tokens = 128_000;
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_context_tokens"),
            "error should mention max_context_tokens: {msg}"
        );
    }

    #[test]
    fn test_validation_context_tokens_equal_to_input_tokens() {
        let mut config = AlmsConfig::default();
        // Equal is fine — the session stores exactly one context window
        config.session.max_context_tokens = 128_000;
        config.context.max_input_tokens = 128_000;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_default_max_context_tokens_larger_than_max_input_tokens() {
        let config = AlmsConfig::default();
        assert!(
            config.session.max_context_tokens >= config.context.max_input_tokens,
            "default max_context_tokens ({}) should be >= max_input_tokens ({})",
            config.session.max_context_tokens,
            config.context.max_input_tokens,
        );
    }
}
