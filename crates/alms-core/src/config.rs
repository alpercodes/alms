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

        // Channels
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            self.channels.telegram_token = Some(token);
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
                "No LLM API key configured. Set OPENROUTER_API_KEY or OPENAI_API_KEY, or enable mock mode with ALMS_LLM_MOCK=1"
            );
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

        // Tools validation
        if self.tools.timeout_secs == 0 {
            return Err(AlmsError::InvalidConfig(
                "tools.timeout_secs must be > 0".into(),
            ));
        }

        Ok(())
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
        }
    }
}

/// LLM provider configuration.
/// Note: api_key is only loaded from env vars, never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
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

/// Session management configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Idle timeout before archiving (seconds)
    pub idle_timeout_secs: u64,
    pub auto_archive: bool,
    /// Delete archived sessions after this many seconds
    pub archive_ttl_secs: u64,
    pub max_messages: usize,
    pub max_context_tokens: usize,
    /// Directory for snapshot persistence
    pub snapshot_dir: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 24 * 60 * 60,
            auto_archive: true,
            archive_ttl_secs: 30 * 24 * 60 * 60,
            max_messages: 10000,
            max_context_tokens: 128000,
            snapshot_dir: "./data/snapshots".into(),
        }
    }
}

/// Context window management configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Strategy: "sliding-summary", "full", "truncate"
    pub strategy: String,
    /// Max tokens to send to the LLM
    pub max_input_tokens: usize,
    /// Number of recent messages to always keep in full
    pub recent_window: usize,
    /// How often to update the rolling summary (in messages)
    pub summary_interval: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            strategy: "truncate".into(), // safe default until sliding-summary is implemented
            max_input_tokens: 32000,
            recent_window: 20,
            summary_interval: 30,
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
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["echo".into(), "math".into(), "http_get".into()],
            timeout_secs: 30,
            max_output_bytes: 65536,
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
    fn test_validation_good() {
        let config = AlmsConfig::default();
        assert!(config.validate().is_ok());
    }
}
