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
    AnthropicConfig, AuthScheme, ChannelsConfig, ContextConfig, FsEditConfig, GeminiConfig,
    LlmConfig, LoggingConfig, OpenAiConfig, ProviderEntry, ProviderKind, ProviderQuirks,
    ReasoningEffort, RunSummaryMode, SecurityConfig, ServerConfig, SessionConfig,
    ShellClassificationMode, ShellPermissions, ShellSpillConfig, ToolOutputTruncateConfig,
    ToolsConfig,
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
    /// Security knobs the operator sets in `alms.toml` and that are NOT
    /// mutable via `PATCH /settings` (#947). See [`SecurityConfig`] for
    /// the threat model.
    pub security: SecurityConfig,
}

impl Default for AlmsConfig {
    fn default() -> Self {
        let mut llm = LlmConfig::default();
        // Ensure the built-in provider sugar entries are present so that
        // `default() → validate()` (the common short-circuit in tests and
        // CLI fallbacks) passes without requiring the full `load()` path.
        llm.ensure_builtin_providers();
        Self {
            server: ServerConfig::default(),
            llm,
            session: SessionConfig::default(),
            context: ContextConfig::default(),
            tools: ToolsConfig::default(),
            channels: ChannelsConfig::default(),
            logging: LoggingConfig::default(),
            security: SecurityConfig::default(),
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

        // Populate built-in provider entries (openai / openrouter / anthropic)
        // so `llm.providers` always contains them, regardless of whether the
        // user wrote the classic flat form or the new generic form.
        //
        // Order matters: we ensure sugar entries exist BEFORE env overrides so
        // that `apply_env_overrides` can propagate `ALMS_LLM_BASE_URL` /
        // `ALMS_LLM_MODEL` into the resolved provider entry. Without this,
        // env overrides to the sugar providers would land only on the flat
        // `llm.base_url` / `llm.model` fields and be silently discarded by the
        // runtime `From<LlmConfig>` impl, which prefers the entry values.
        config.llm.ensure_builtin_providers();

        // Apply env var overrides
        config.apply_env_overrides();

        // Resolve data_dir to an absolute path so that downstream consumers
        // (db_path(), agents_dir(), shell_exec env) never interpret it
        // relative to a changed cwd. This is the canonical fix for issue #300
        // (stray data/alms.db inside agent workspace directories).
        config.server.data_dir = crate::resolve_to_absolute(Path::new(&config.server.data_dir));

        // Resolve project_root (#945) only when explicitly configured. An
        // empty value defers resolution to runtime so a fresh `Default` does
        // not silently pin a stale cwd into the config.
        if !config.server.project_root.is_empty() {
            config.server.project_root =
                crate::resolve_to_absolute(Path::new(&config.server.project_root));
        }

        // Check for legacy ./data directory and warn about migration.
        config.warn_legacy_data_dir();

        // Normalize episodic memory settings (soft corrections with warnings)
        config.context.normalize_episodic();

        // Validate
        config.validate()?;

        Ok(config)
    }

    /// Load config with fail-fast semantics on validation failure (#924).
    ///
    /// This is the path the gateway entrypoint (`alms gateway`) uses. It
    /// distinguishes two error classes:
    ///
    /// - **File-load failures** (file unreadable due to permissions, TOML
    ///   parse error): warn to stderr and fall back to compiled defaults
    ///   so first-run / fresh-install / corrupted-file scenarios still
    ///   produce a working daemon.
    /// - **Validation failures** (a hand-edited `alms.toml` that loaded
    ///   cleanly but violates a `validate()` invariant — e.g. asymmetric
    ///   `[context].summary_provider` / `summary_model` after #877's
    ///   pair-only rule): **fatal**, the process exits non-zero with a
    ///   clear stderr message pointing at the bad field.
    ///
    /// Falling back to defaults on a validation failure would silently
    /// discard the operator's intent (Tim's review framing on PR #923,
    /// surfaced as #924). The recovery is on the operator (fix the
    /// config) not the daemon (drop the config and pretend nothing
    /// happened). Missing-file (`find_config_file() == None`) is the
    /// genuine bootstrapping case and is handled before either branch
    /// runs — it produces a defaults-with-env-overrides config and zero
    /// exit, same as before.
    ///
    /// Unlike `AlmsConfig::load().unwrap_or_default()`, this ensures
    /// `data_dir` is resolved to an absolute path even in the file-load
    /// fallback case. Use this whenever a best-effort config is
    /// acceptable for the file-load axis but the validation invariant
    /// must hold (gateway boot, scheduler, anything that drives runs).
    ///
    /// **Behaviour scope (#924 follow-up).** This entry-point is shared
    /// across the gateway boot path *and* every CLI subcommand that goes
    /// through `helpers::open_db_with_config()` (`alms agent ...`,
    /// `alms session ...`, `alms run ...`, `alms job ...`, plus
    /// `alms auth`). A validation failure therefore aborts the process
    /// for those CLI commands too, not just `alms gateway`. This is the
    /// intended shape — discarding the operator's intent silently would
    /// be just as wrong on a CLI invocation as on daemon boot — but it
    /// is wider than the issue title suggests, so callers that want a
    /// best-effort defaults-on-validation-failure shape should reach for
    /// [`Self::load_or_default_fallible`] (or [`Self::load`]) directly.
    ///
    /// Calls `std::process::exit(1)` on validation failure. The fallible
    /// shape lives in [`Self::load_or_default_fallible`] for unit
    /// testing.
    pub fn load_or_default() -> Self {
        match Self::load_or_default_fallible() {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Error: invalid configuration: {e}");
                eprintln!(
                    "The daemon will not start with an invalid config — fix the offending \
                     field in alms.toml (or in the corresponding ALMS_* environment \
                     variable) and try again. Removing the config file entirely is also \
                     valid; the daemon will then start with compiled defaults."
                );
                std::process::exit(1);
            }
        }
    }

    /// Inner of [`Self::load_or_default`] — same semantics, but returns
    /// the validation error instead of exiting the process. Exposed for
    /// unit testing the fail-fast contract from #924.
    ///
    /// Contract:
    /// - File-load failures (IO, TOML parse): warn-and-default. Returns
    ///   `Ok(default-shaped config)`.
    /// - Validation failures: returns `Err(InvalidConfig(...))`. The
    ///   process-exit wrapper turns this into a non-zero exit with a
    ///   helpful stderr message.
    pub fn load_or_default_fallible() -> AlmsResult<Self> {
        // Step 1: try to read + parse the config file (or fall through
        // when no file is present). File-load failures are
        // recoverable — warn and use defaults. The eventual `validate()`
        // call below still applies; defaults are guaranteed to pass
        // validation, but env-var overrides could still produce an
        // invalid config that fails the fail-fast check.
        let mut config = match Self::find_config_file() {
            Some(path) => match Self::from_file(&path) {
                Ok(c) => {
                    info!("Loaded config from {}", path.display());
                    c
                }
                Err(e) => {
                    eprintln!(
                        "Warning: failed to load config file {}: {}. Using defaults.",
                        path.display(),
                        e,
                    );
                    Self::default()
                }
            },
            None => Self::default(),
        };

        // Step 2: layer env overrides + provider sugar + path resolution +
        // soft normalisations. Mirrors `load()` so the two paths produce
        // structurally identical configs modulo the file-load fallback.
        config.llm.ensure_builtin_providers();
        config.apply_env_overrides();
        config.server.data_dir = crate::resolve_to_absolute(Path::new(&config.server.data_dir));
        // Resolve project_root (#945) only when the field is non-empty —
        // an empty value means "fall back to current_dir at the moment a
        // gateway / runtime asks". Pre-resolving an empty value here would
        // pin a stale cwd onto the config, which is exactly the behaviour
        // we want `resolved_project_root()` to side-step.
        if !config.server.project_root.is_empty() {
            config.server.project_root =
                crate::resolve_to_absolute(Path::new(&config.server.project_root));
        }
        config.warn_legacy_data_dir();
        config.context.normalize_episodic();

        // Step 3: fail-fast on validation errors (#924). A
        // hand-edited `alms.toml` (or an env-var override) that
        // violates an invariant must NOT be silently replaced with
        // defaults — the operator's intent is then discarded and runs
        // proceed against the wrong config. Propagate the error so the
        // wrapper can exit non-zero with a helpful message.
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

    /// Apply environment variable overrides for non-secret settings.
    ///
    /// API keys and tokens are NOT loaded from env vars — they must be
    /// configured via `alms auth set` and stored in `.alms/secrets.json`.
    ///
    /// Callers must invoke [`LlmConfig::ensure_builtin_providers`] before
    /// this method so that sugar provider entries (`openai`, `openrouter`,
    /// `anthropic`) exist in `llm.providers`. When `ALMS_LLM_BASE_URL` or
    /// `ALMS_LLM_MODEL` is set, the override is propagated into the
    /// resolved provider entry as well as the flat `llm.base_url` /
    /// `llm.model` fields; the runtime `From<LlmConfig>` impl prefers the
    /// entry values, so propagation is required for env overrides to win
    /// end-to-end.
    pub fn apply_env_overrides(&mut self) {
        // LLM settings (non-secret only)
        if let Ok(provider) = std::env::var("ALMS_LLM_PROVIDER") {
            self.llm.provider = provider.to_lowercase();
        }
        // NOTE: API key is NOT loaded from env vars. Use `alms auth set`.
        if let Ok(url) =
            std::env::var("ALMS_LLM_BASE_URL").or_else(|_| std::env::var("LLM_BASE_URL"))
        {
            self.llm.base_url = url.clone();
            // Also propagate to the resolved provider entry so the runtime
            // `From` impl (which prefers entry values) honours the override.
            if let Some(entry) = self.llm.providers.get_mut(&self.llm.provider) {
                entry.base_url = url;
            }
        }
        if let Ok(model) =
            std::env::var("ALMS_LLM_MODEL").or_else(|_| std::env::var("DEFAULT_MODEL"))
        {
            self.llm.model = model.clone();
            // Same propagation rationale as `base_url` above.
            if let Some(entry) = self.llm.providers.get_mut(&self.llm.provider) {
                entry.model = Some(model);
            }
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
        // Project root (#945) — the workspace v2 sandbox boundary.
        // Stored as the raw string here; the gateway resolves it to an
        // absolute path via `ServerConfig::resolved_project_root`. CLI
        // `--project <path>` overrides by writing into this same field
        // before `serve_with_config` runs.
        if let Ok(project_root) = std::env::var("ALMS_PROJECT_ROOT") {
            self.server.project_root = project_root;
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
        if let Ok(val) = std::env::var("ALMS_SHELL_CLASSIFICATION_MODE") {
            match val.to_ascii_lowercase().as_str() {
                "off" => self.tools.shell_classification_mode = types::ShellClassificationMode::Off,
                "warn" => {
                    self.tools.shell_classification_mode = types::ShellClassificationMode::Warn
                }
                "block_destructive" => {
                    self.tools.shell_classification_mode =
                        types::ShellClassificationMode::BlockDestructive
                }
                "strict" => {
                    self.tools.shell_classification_mode = types::ShellClassificationMode::Strict
                }
                other => warn!(
                    value = %other,
                    "Ignoring ALMS_SHELL_CLASSIFICATION_MODE: expected off|warn|block_destructive|strict"
                ),
            }
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
        // LLM validation. Suppress the warning when the selected provider
        // entry declares `api_key_env` or an inline `api_key` — those paths
        // resolve the key at request time, so a missing `llm.api_key` isn't
        // the user's problem (see PR #770 review feedback).
        let entry_has_key_source = self
            .llm
            .providers
            .get(&self.llm.provider)
            .is_some_and(|e| e.api_key_env.is_some() || e.api_key.is_some());
        if !self.llm.mock && self.llm.api_key.is_none() && !entry_has_key_source {
            warn!(
                "No LLM API key configured. Run `alms auth set <provider> <key>` to store a key, or enable mock mode with ALMS_LLM_MOCK=1"
            );
        }

        // Provider must either match a built-in sugar name or be declared
        // as an entry in `[llm.providers.<name>]`. `ensure_builtin_providers`
        // guarantees the sugar names are always present in the map, so a
        // single lookup is sufficient.
        if !self.llm.mock && !self.llm.providers.contains_key(&self.llm.provider) {
            let mut known: Vec<&str> = self.llm.providers.keys().map(|s| s.as_str()).collect();
            known.sort();
            return Err(AlmsError::InvalidConfig(format!(
                "llm.provider '{}' is not defined — add a `[llm.providers.{}]` entry \
                 to alms.toml. Known providers: {:?}",
                self.llm.provider, self.llm.provider, known
            )));
        }

        // Provider entries must have a non-empty base_url.
        for (name, entry) in &self.llm.providers {
            if entry.base_url.is_empty() {
                return Err(AlmsError::InvalidConfig(format!(
                    "llm.providers.{name} is missing a base_url"
                )));
            }
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

        // Symmetric pair-only validation for `[context].summary_provider`
        // / `[context].summary_model` (#877). The PATCH layer (and the
        // per-agent CRUD path, see `validate_summary_pair` in
        // `alms-gateway/src/agents.rs`) already reject asymmetric inputs,
        // but a hand-edited `alms.toml` can land the daemon in that
        // shape without going through PATCH. Validate at config-load
        // time so the daemon refuses to start rather than silently
        // shipping a half-configured summary path. Rule: both fields
        // must be `Some` together or both `None` together — exactly
        // one set is the broken case.
        match (
            self.context.summary_provider.as_deref(),
            self.context.summary_model.as_deref(),
        ) {
            (Some(_), None) => {
                return Err(AlmsError::InvalidConfig(
                    "context.summary_provider is set but context.summary_model is empty. \
                     Set both fields together — the summary provider's wire model namespace \
                     is independent of the agent's primary provider, so partial settings \
                     cannot be safely resolved. Either set both, or remove both to \
                     fall through to the agent's primary provider/model."
                        .into(),
                ));
            }
            (None, Some(_)) => {
                return Err(AlmsError::InvalidConfig(
                    "context.summary_model is set but context.summary_provider is empty. \
                     Set both fields together — leaving summary_provider unset would fall \
                     through to the agent's primary provider, which may not match this \
                     model's namespace. Either set both, or remove both."
                        .into(),
                ));
            }
            _ => {}
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

        // Security validation (#947). Empty / whitespace-only entries in
        // `allow_full_os_access` are caught here so a hand-edited TOML
        // can't accidentally widen the blast radius of every unnamed
        // agent (`SecurityConfig::is_full_os_access_agent` already
        // shortcircuits on empty input, but rejecting at load time
        // surfaces the operator error explicitly rather than silently
        // discarding the broken entry).
        for (idx, name) in self.security.allow_full_os_access.iter().enumerate() {
            if name.trim().is_empty() {
                return Err(AlmsError::InvalidConfig(format!(
                    "security.allow_full_os_access[{idx}] is empty — every entry must \
                     name a registered agent. Remove the empty string or replace it \
                     with the agent's name."
                )));
            }
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
