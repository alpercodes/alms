//! Unified configuration for ALMS.
//!
//! Loads config with layered precedence:
//! 1. Compiled defaults
//! 2. Config file (alms.toml)
//! 3. Environment variables (non-secret `ALMS_*` prefix)
//!
//! Secrets (API keys, tokens) are loaded exclusively from `.alms/secrets.json`
//! via `alms auth set`. Environment variable fallback has been removed for
//! security — agents can read env vars via `shell_exec`.

mod types;

#[cfg(test)]
mod tests;

pub use types::{
    ChannelsConfig, ContextConfig, LlmConfig, LoggingConfig, RunSummaryMode, ServerConfig,
    SessionConfig, ShellPermissions, ToolsConfig,
};

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
    pub logging: LoggingConfig,
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
            logging: LoggingConfig::default(),
        }
    }
}

impl AlmsConfig {
    /// Load config with full layered precedence:
    /// defaults -> config file -> env vars
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

        // Resolve data_dir to an absolute path so that downstream consumers
        // (db_path(), workspace_dir(), shell_exec env) never interpret it
        // relative to a changed cwd. This is the canonical fix for issue #300
        // (stray data/alms.db inside agent workspace directories).
        config.server.data_dir = crate::resolve_to_absolute(Path::new(&config.server.data_dir));

        // Check for legacy ./data directory and warn about migration.
        config.warn_legacy_data_dir();

        // Normalize episodic memory settings (soft corrections with warnings)
        config.context.normalize_episodic();

        // Validate
        config.validate()?;

        Ok(config)
    }

    /// Load config, falling back to defaults on error.
    ///
    /// Unlike `AlmsConfig::load().unwrap_or_default()`, this ensures that
    /// `data_dir` is resolved to an absolute path even in the fallback case.
    /// Use this whenever a best-effort config is acceptable (e.g. CLI
    /// subcommands that just need a database path).
    pub fn load_or_default() -> Self {
        match Self::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Warning: failed to load config: {e}. Using defaults.");
                let mut cfg = Self::default();
                cfg.apply_env_overrides();
                cfg.server.data_dir = crate::resolve_to_absolute(Path::new(&cfg.server.data_dir));
                cfg.warn_legacy_data_dir();
                cfg.context.normalize_episodic();
                cfg
            }
        }
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

    /// Apply environment variable overrides for non-secret settings.
    ///
    /// API keys and tokens are NOT loaded from env vars — they must be
    /// configured via `alms auth set` and stored in `.alms/secrets.json`.
    pub fn apply_env_overrides(&mut self) {
        // LLM settings (non-secret only)
        if let Ok(provider) = std::env::var("ALMS_LLM_PROVIDER") {
            self.llm.provider = provider.to_lowercase();
        }
        // NOTE: API key is NOT loaded from env vars. Use `alms auth set`.
        if let Ok(url) =
            std::env::var("ALMS_LLM_BASE_URL").or_else(|_| std::env::var("LLM_BASE_URL"))
        {
            self.llm.base_url = url;
        }
        if let Ok(model) =
            std::env::var("ALMS_LLM_MODEL").or_else(|_| std::env::var("DEFAULT_MODEL"))
        {
            self.llm.model = model;
        }
        if let Ok(val) = std::env::var("ALMS_LLM_MOCK") {
            self.llm.mock = matches!(val.to_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Ok(val) = std::env::var("ALMS_LLM_STREAM_CHUNK_TIMEOUT")
            && let Ok(n) = val.parse()
        {
            self.llm.stream_chunk_timeout_secs = n;
        }

        // Server settings
        if let Ok(bind) = std::env::var("ALMS_BIND") {
            self.server.bind = bind;
        }
        if let Ok(data_dir) = std::env::var("ALMS_DATA_DIR") {
            self.server.data_dir = data_dir;
        }
        if let Ok(token) = std::env::var("ALMS_AUTH_TOKEN") {
            self.server.auth_token = Some(token);
        }

        // NOTE: Telegram token is NOT loaded from env vars. Use `alms auth set telegram <token>`.

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
        if let Ok(val) = std::env::var("ALMS_RUN_SUMMARY_MODE") {
            // FromStr always succeeds — unrecognized values become Unknown,
            // which normalize_episodic() will convert to Llm with a warning.
            self.context.run_summary_mode = val.parse().unwrap_or_default();
        }
        if let Ok(val) = std::env::var("ALMS_RUN_SUMMARY_BUDGET")
            && let Ok(n) = val.parse()
        {
            self.context.run_summary_budget = n;
        }
        if let Ok(val) = std::env::var("ALMS_SUMMARY_MAX_TOKENS")
            && let Ok(n) = val.parse()
        {
            self.context.summary_max_tokens = n;
        }

        // Logging settings
        if let Ok(val) = std::env::var("ALMS_LOG_FILE_ENABLED") {
            self.logging.file_enabled = matches!(val.to_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Ok(val) = std::env::var("ALMS_LOG_DIR") {
            self.logging.log_dir = Some(val);
        }
        if let Ok(val) = std::env::var("ALMS_LOG_FILE_LEVEL") {
            self.logging.file_level = val;
        }
        if let Ok(val) = std::env::var("ALMS_LOG_ROTATION") {
            self.logging.rotation = val;
        }
    }

    /// Validate config. Returns error on invalid values.
    pub fn validate(&self) -> AlmsResult<()> {
        // LLM validation
        if !self.llm.mock && self.llm.api_key.is_none() {
            warn!(
                "No LLM API key configured. Run `alms auth set <provider> <key>` to store a key, or enable mock mode with ALMS_LLM_MOCK=1"
            );
        }

        let valid_providers = ["openai", "anthropic", "openrouter"];
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

        if self.llm.stream_chunk_timeout_secs == 0 {
            return Err(AlmsError::InvalidConfig(
                "llm.stream_chunk_timeout_secs must be > 0".into(),
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

        // Logging validation
        let valid_rotations = ["daily", "hourly", "never"];
        if !valid_rotations.contains(&self.logging.rotation.as_str()) {
            return Err(AlmsError::InvalidConfig(format!(
                "logging.rotation must be one of {:?}, got '{}'",
                valid_rotations, self.logging.rotation
            )));
        }

        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.logging.file_level.as_str()) {
            return Err(AlmsError::InvalidConfig(format!(
                "logging.file_level must be one of {:?}, got '{}'",
                valid_levels, self.logging.file_level
            )));
        }

        Ok(())
    }

    /// Warn if a legacy `./data` directory exists but the new `.alms/`
    /// directory does not. This helps users who upgrade from older versions
    /// discover that the default data directory has changed.
    ///
    /// Does not auto-migrate — too risky. Just logs a warning.
    fn warn_legacy_data_dir(&self) {
        // Only warn when the default `.alms` directory is in use (not a
        // custom ALMS_DATA_DIR override). We check the *raw* default value
        // before absolute-path resolution.
        let data_path = Path::new(&self.server.data_dir);

        // The data_dir has already been resolved to an absolute path at
        // this point, so check whether it ends with `.alms`.
        let is_default = data_path.file_name().is_some_and(|name| name == ".alms");

        if !is_default {
            return;
        }

        // Check for legacy ./data in the parent directory (the workspace root).
        if let Some(parent) = data_path.parent() {
            let legacy_dir = parent.join("data");
            if legacy_dir.is_dir() && !data_path.is_dir() {
                warn!(
                    legacy = %legacy_dir.display(),
                    current = %data_path.display(),
                    "Found legacy data directory. The default data directory has \
                     changed from ./data to ./.alms. To migrate, run: \
                     mv {} {}",
                    legacy_dir.display(),
                    data_path.display(),
                );
            }
        }
    }

    /// Ensure the data directory exists, creating it if necessary.
    ///
    /// Logs a message when a new `.alms/` directory is created (first run),
    /// similar to how `git init` reports creating `.git/`.
    pub fn ensure_data_dir(&self) {
        let data_path = Path::new(&self.server.data_dir);
        let already_exists = data_path.is_dir();

        if let Err(e) = std::fs::create_dir_all(data_path) {
            warn!(
                path = %data_path.display(),
                error = %e,
                "Could not create data directory"
            );
            return;
        }

        if !already_exists {
            info!(
                path = %data_path.display(),
                "Initialized ALMS data directory"
            );
        }
    }

    /// Check for deprecated API key environment variables and log warnings.
    ///
    /// Should be called at startup (e.g. in `main.rs`). Detects env vars that
    /// were previously used to configure secrets and warns the user to migrate
    /// to `alms auth set`.
    pub fn warn_deprecated_secret_env_vars() {
        let deprecated = crate::secrets::SecretsStore::detect_deprecated_env_keys();
        if deprecated.is_empty() {
            return;
        }
        for (var, provider) in &deprecated {
            warn!(
                env_var = %var,
                provider = %provider,
                "API key env var detected but IGNORED for security. \
                 Migrate to: alms auth set {provider} <key>"
            );
        }
        warn!(
            "API key environment variables are no longer used. \
             Store keys securely with `alms auth set <provider> <key>` \
             (encrypted with ALMS_MASTER_KEY)."
        );
    }
}

/// Get home directory path (cross-platform)
fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}
