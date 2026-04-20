//! Tests for the config module.

use super::*;
use std::sync::{Mutex, MutexGuard};

/// Select an API key for the requested LLM provider.
///
/// Preference order:
/// - `openai`: `OPENAI_API_KEY`, then `OPENROUTER_API_KEY`, then
///   `ANTHROPIC_API_KEY`
/// - `anthropic`: `ANTHROPIC_API_KEY`, then `OPENAI_API_KEY`, then
///   `OPENROUTER_API_KEY`
/// - other providers: `OPENAI_API_KEY`, then `OPENROUTER_API_KEY`, then
///   `ANTHROPIC_API_KEY`
///
/// This preserves single-key setups by falling back to any available key when
/// the preferred provider-specific key is absent.
///
/// Used only in tests — runtime key resolution goes through `SecretsStore`.
pub(crate) fn select_llm_api_key(
    provider: &str,
    openrouter_key: Option<String>,
    openai_key: Option<String>,
    anthropic_key: Option<String>,
) -> Option<String> {
    match provider {
        "anthropic" => anthropic_key.or(openai_key).or(openrouter_key),
        "openai" => openai_key.or(openrouter_key).or(anthropic_key),
        "openrouter" => openrouter_key.or(openai_key).or(anthropic_key),
        _ => openai_key.or(openrouter_key).or(anthropic_key),
    }
}

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

/// RAII guard for a single env var. Restores the original value on drop,
/// making tests panic-safe.
struct SingleEnvGuard {
    key: String,
    original: Option<String>,
}

impl SingleEnvGuard {
    fn set(key: &str, val: &str) -> Self {
        let original = std::env::var(key).ok();
        set_env_var(key, val);
        Self {
            key: key.to_string(),
            original,
        }
    }

    fn remove(key: &str) -> Self {
        let original = std::env::var(key).ok();
        remove_env_var(key);
        Self {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for SingleEnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => set_env_var(&self.key, v),
            None => remove_env_var(&self.key),
        }
    }
}

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
fn test_select_llm_api_key_openai_prefers_openai_then_openrouter() {
    let selected = select_llm_api_key(
        "openai",
        Some("openrouter-key".into()),
        Some("openai-key".into()),
        Some("anthropic-key".into()),
    );
    assert_eq!(selected.as_deref(), Some("openai-key"));

    let fallback = select_llm_api_key(
        "openai",
        Some("openrouter-key".into()),
        None,
        Some("anthropic-key".into()),
    );
    assert_eq!(fallback.as_deref(), Some("openrouter-key"));
}

#[test]
fn test_select_llm_api_key_anthropic_prefers_anthropic() {
    let selected = select_llm_api_key(
        "anthropic",
        Some("openrouter-key".into()),
        Some("openai-key".into()),
        Some("anthropic-key".into()),
    );
    assert_eq!(selected.as_deref(), Some("anthropic-key"));
}

#[test]
fn test_select_llm_api_key_returns_none_when_no_keys_available() {
    let selected = select_llm_api_key("openai", None, None, None);
    assert_eq!(selected, None);
}

#[test]
fn test_select_llm_api_key_unknown_provider_uses_openai_fallback_chain() {
    let selected = select_llm_api_key(
        "custom-provider",
        Some("openrouter-key".into()),
        Some("openai-key".into()),
        Some("anthropic-key".into()),
    );
    assert_eq!(selected.as_deref(), Some("openai-key"));
}

#[test]
fn test_select_llm_api_key_anthropic_falls_back_to_openai_then_openrouter() {
    let fallback_to_openai = select_llm_api_key(
        "anthropic",
        Some("openrouter-key".into()),
        Some("openai-key".into()),
        None,
    );
    assert_eq!(fallback_to_openai.as_deref(), Some("openai-key"));

    let fallback_to_openrouter =
        select_llm_api_key("anthropic", Some("openrouter-key".into()), None, None);
    assert_eq!(fallback_to_openrouter.as_deref(), Some("openrouter-key"));
}

#[test]
fn test_apply_env_overrides_does_not_load_api_keys_from_env() {
    let _guard = EnvGuard::set(&[
        ("ALMS_LLM_PROVIDER", Some("openai")),
        ("OPENROUTER_API_KEY", Some("openrouter-key")),
        ("OPENAI_API_KEY", Some("openai-key")),
        ("ANTHROPIC_API_KEY", Some("anthropic-key")),
    ]);

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();

    assert_eq!(config.llm.provider, "openai");
    // API key must NOT be loaded from env vars (security: agents can read env vars)
    assert_eq!(config.llm.api_key, None);
}

#[test]
fn test_apply_env_overrides_provider_without_key() {
    let _guard = EnvGuard::set(&[("ALMS_LLM_PROVIDER", Some("anthropic"))]);

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();

    assert_eq!(config.llm.provider, "anthropic");
    assert_eq!(config.llm.api_key, None);
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

#[test]
fn test_workspace_dir_default() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove("ALMS_WORKSPACE_DIR");

    let config = ServerConfig::default();
    assert_eq!(config.workspace_dir(), PathBuf::from("./.alms/workspace"));
}

#[test]
fn test_workspace_dir_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_WORKSPACE_DIR", "/custom/workspace");

    let config = ServerConfig::default();
    assert_eq!(config.workspace_dir(), PathBuf::from("/custom/workspace"));
}

#[test]
fn test_workspace_dir_uses_data_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove("ALMS_WORKSPACE_DIR");

    let config = ServerConfig {
        data_dir: "/my/data".into(),
        ..Default::default()
    };
    assert_eq!(config.workspace_dir(), PathBuf::from("/my/data/workspace"));
}

#[test]
fn test_validation_zero_stream_chunk_timeout() {
    let mut config = AlmsConfig::default();
    config.llm.stream_chunk_timeout_secs = 0;
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("stream_chunk_timeout_secs"),
        "error should mention stream_chunk_timeout_secs: {msg}"
    );
}

#[test]
fn test_env_override_stream_chunk_timeout() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_STREAM_CHUNK_TIMEOUT", "90");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.stream_chunk_timeout_secs, 90);
}

#[test]
fn test_env_override_alms_llm_model() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_MODEL", "claude-sonnet-4-6");
    // Ensure legacy var is not set
    let _legacy_guard = SingleEnvGuard::remove("DEFAULT_MODEL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.model, "claude-sonnet-4-6");
}

#[test]
fn test_env_override_legacy_default_model_still_works() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("DEFAULT_MODEL", "gpt-4o");
    // Ensure new var is not set so legacy is used
    let _new_guard = SingleEnvGuard::remove("ALMS_LLM_MODEL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.model, "gpt-4o");
}

#[test]
fn test_env_override_alms_llm_model_takes_precedence_over_legacy() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _new_guard = SingleEnvGuard::set("ALMS_LLM_MODEL", "new-model");
    let _legacy_guard = SingleEnvGuard::set("DEFAULT_MODEL", "legacy-model");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(
        config.llm.model, "new-model",
        "ALMS_LLM_MODEL should take precedence over DEFAULT_MODEL"
    );
}

#[test]
fn test_env_override_alms_llm_base_url() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "https://custom.api.com/v1");
    let _legacy_guard = SingleEnvGuard::remove("LLM_BASE_URL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.base_url, "https://custom.api.com/v1");
}

#[test]
fn test_env_override_legacy_llm_base_url_still_works() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("LLM_BASE_URL", "https://legacy.api.com/v1");
    let _new_guard = SingleEnvGuard::remove("ALMS_LLM_BASE_URL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.base_url, "https://legacy.api.com/v1");
}

#[test]
fn test_toml_stream_chunk_timeout() {
    let toml = r#"
[llm]
stream_chunk_timeout_secs = 120
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.llm.stream_chunk_timeout_secs, 120);
}

// ---- Gemini provider layering tests (issue #764) ----

/// `ensure_builtin_providers` populates a `gemini` sugar entry so that
/// `llm.provider = "gemini"` Just Works without an explicit
/// `[llm.providers.gemini]` block.
#[test]
fn test_gemini_sugar_entry_auto_populated() {
    let config = AlmsConfig::default();
    let entry = config
        .llm
        .providers
        .get("gemini")
        .expect("gemini sugar entry must be auto-populated by ensure_builtin_providers");
    assert_eq!(entry.kind, ProviderKind::Gemini);
    assert_eq!(
        entry.base_url,
        "https://generativelanguage.googleapis.com/v1beta"
    );
    match &entry.auth_scheme {
        AuthScheme::Header { name } => assert_eq!(name, "x-goog-api-key"),
        other => panic!("expected header auth scheme for gemini, got {other:?}"),
    }
}

/// Selecting Gemini via `ALMS_LLM_PROVIDER=gemini` resolves to the sugar
/// entry and passes validation (given a mock/no-key config).
#[test]
fn test_env_override_selects_gemini_provider() {
    let _guard = EnvGuard::set(&[("ALMS_LLM_PROVIDER", Some("gemini"))]);

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.provider, "gemini");

    // Provider must be known to `validate()` so the error path isn't
    // triggered. The sugar entry makes this true even without a TOML file.
    config.llm.mock = true;
    assert!(config.validate().is_ok());
}

/// `ALMS_LLM_MODEL` override propagates into the resolved provider entry,
/// not just the flat `llm.model` field — mirrors the existing test for
/// openrouter/openai/anthropic.
#[test]
fn test_env_override_model_propagates_to_gemini_entry() {
    let _provider_guard = EnvGuard::set(&[("ALMS_LLM_PROVIDER", Some("gemini"))]);
    let _model_guard = SingleEnvGuard::set("ALMS_LLM_MODEL", "gemini-2.5-flash");
    let _legacy_guard = SingleEnvGuard::remove("DEFAULT_MODEL");

    let mut config = AlmsConfig::default();
    config.llm.ensure_builtin_providers();
    config.apply_env_overrides();

    assert_eq!(config.llm.provider, "gemini");
    assert_eq!(config.llm.model, "gemini-2.5-flash");
    let entry = config.llm.providers.get("gemini").unwrap();
    assert_eq!(entry.model.as_deref(), Some("gemini-2.5-flash"));
}

/// A user-declared `[llm.providers.gemini]` block overrides the sugar
/// entry — in particular `api_key_env` and `model` flow through.
#[test]
fn test_toml_gemini_provider_with_api_key_env() {
    let toml = r#"
[llm]
provider = "gemini"
model    = "gemini-2.5-pro"

[llm.providers.gemini]
kind        = "gemini"
base_url    = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
model       = "gemini-2.5-pro"
auth_scheme = { type = "header", name = "x-goog-api-key" }
"#;
    let mut config: AlmsConfig = toml::from_str(toml).unwrap();
    config.llm.ensure_builtin_providers();
    config.llm.mock = true;
    config.validate().unwrap();

    let entry = config.llm.providers.get("gemini").unwrap();
    assert_eq!(entry.kind, ProviderKind::Gemini);
    assert_eq!(entry.api_key_env.as_deref(), Some("GEMINI_API_KEY"));
    assert_eq!(entry.model.as_deref(), Some("gemini-2.5-pro"));
}

/// `ProviderEntry::resolve_api_key()` reads from the declared env var for
/// the Gemini entry.
#[test]
fn test_gemini_resolve_api_key_from_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_TEST_GEMINI_KEY_764", "gemini-test-key");

    let entry = ProviderEntry {
        kind: ProviderKind::Gemini,
        base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
        api_key_env: Some("ALMS_TEST_GEMINI_KEY_764".into()),
        api_key: None,
        model: Some("gemini-2.5-pro".into()),
        auth_scheme: AuthScheme::Header {
            name: "x-goog-api-key".into(),
        },
        quirks: ProviderQuirks::default(),
    };
    assert_eq!(entry.resolve_api_key().as_deref(), Some("gemini-test-key"));
}

// ---- LoggingConfig tests ----

#[test]
fn test_logging_config_defaults() {
    let config = LoggingConfig::default();
    assert!(config.file_enabled);
    assert_eq!(config.file_level, "debug");
    assert_eq!(config.rotation, "daily");
    assert!(config.log_dir.is_none());
}

#[test]
fn test_logging_resolve_log_dir_default() {
    let config = LoggingConfig::default();
    let resolved = config.resolve_log_dir("./.alms");
    assert_eq!(resolved, PathBuf::from("./.alms/logs"));
}

/// Verify that `resolve_log_dir` produces valid paths even when
/// `data_dir` uses Windows backslash separators (no forward-slash mixing).
#[cfg(windows)]
#[test]
fn test_logging_resolve_log_dir_windows_backslash() {
    let config = LoggingConfig::default();
    let resolved = config.resolve_log_dir(r"C:\Users\test\data");
    // Path::join produces backslash-separated path on Windows
    assert_eq!(resolved, PathBuf::from(r"C:\Users\test\data\logs"));
}

#[test]
fn test_logging_resolve_log_dir_override() {
    let config = LoggingConfig {
        log_dir: Some("/custom/logs".into()),
        ..Default::default()
    };
    let resolved = config.resolve_log_dir("./data");
    assert_eq!(resolved, PathBuf::from("/custom/logs"));
}

#[test]
fn test_validation_bad_rotation() {
    let mut config = AlmsConfig::default();
    config.logging.rotation = "weekly".into();
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("logging.rotation"),
        "error should mention logging.rotation: {msg}"
    );
}

#[test]
fn test_validation_bad_file_level() {
    let mut config = AlmsConfig::default();
    config.logging.file_level = "verbose".into();
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("logging.file_level"),
        "error should mention logging.file_level: {msg}"
    );
}

#[test]
fn test_validation_valid_rotations() {
    for rotation in &["daily", "hourly", "never"] {
        let mut config = AlmsConfig::default();
        config.logging.rotation = (*rotation).into();
        assert!(
            config.validate().is_ok(),
            "rotation '{}' should be valid",
            rotation
        );
    }
}

#[test]
fn test_validation_valid_file_levels() {
    for level in &["trace", "debug", "info", "warn", "error"] {
        let mut config = AlmsConfig::default();
        config.logging.file_level = (*level).into();
        assert!(
            config.validate().is_ok(),
            "file_level '{}' should be valid",
            level
        );
    }
}

#[test]
fn test_env_override_log_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LOG_DIR", "/tmp/alms-logs");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.log_dir.as_deref(), Some("/tmp/alms-logs"));
}

#[test]
fn test_env_override_log_file_level() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LOG_FILE_LEVEL", "warn");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.file_level, "warn");
}

#[test]
fn test_env_override_log_rotation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LOG_ROTATION", "hourly");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.rotation, "hourly");
}

#[test]
fn test_env_override_file_enabled_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LOG_FILE_ENABLED", "false");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert!(!config.logging.file_enabled);
}

#[test]
fn test_env_override_file_enabled_true() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LOG_FILE_ENABLED", "true");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert!(config.logging.file_enabled);
}

#[test]
fn test_logging_toml_round_trip() {
    let toml = r#"
[logging]
file_enabled = false
file_level = "info"
rotation = "hourly"
log_dir = "/var/log/alms"
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert!(!config.logging.file_enabled);
    assert_eq!(config.logging.file_level, "info");
    assert_eq!(config.logging.rotation, "hourly");
    assert_eq!(config.logging.log_dir.as_deref(), Some("/var/log/alms"));
}

#[test]
fn test_load_produces_absolute_data_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Use mock LLM to avoid API key validation failure.
    let _mock_guard = SingleEnvGuard::set("ALMS_LLM_MOCK", "1");
    // Clear ALMS_DATA_DIR to ensure the default `./.alms` is used.
    let _data_guard = SingleEnvGuard::remove("ALMS_DATA_DIR");

    let config = AlmsConfig::load().expect("load should succeed with mock LLM");
    let data_path = std::path::Path::new(&config.server.data_dir);
    assert!(
        data_path.is_absolute(),
        "data_dir should be absolute after load(), got: {}",
        config.server.data_dir
    );
}

#[test]
fn test_load_with_env_override_data_dir_is_absolute() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _mock_guard = SingleEnvGuard::set("ALMS_LLM_MOCK", "1");
    // Set a relative ALMS_DATA_DIR — load() should still resolve to absolute.
    let _data_guard = SingleEnvGuard::set("ALMS_DATA_DIR", "relative/data/dir");

    let config = AlmsConfig::load().expect("load should succeed with mock LLM");
    let data_path = std::path::Path::new(&config.server.data_dir);
    assert!(
        data_path.is_absolute(),
        "data_dir should be absolute even with relative env override, got: {}",
        config.server.data_dir
    );
}

#[test]
fn test_db_path_is_absolute_after_load() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _mock_guard = SingleEnvGuard::set("ALMS_LLM_MOCK", "1");
    let _data_guard = SingleEnvGuard::remove("ALMS_DATA_DIR");
    let _db_guard = SingleEnvGuard::remove("ALMS_DB_PATH");

    let config = AlmsConfig::load().expect("load should succeed with mock LLM");
    let db_path = config.server.db_path();
    let db_path_obj = std::path::Path::new(&db_path);
    assert!(
        db_path_obj.is_absolute(),
        "db_path() should be absolute after load(), got: {}",
        db_path
    );
}

#[test]
fn test_load_or_default_resolves_data_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Clear data dir env to use the relative default "./.alms".
    let _data_guard = SingleEnvGuard::remove("ALMS_DATA_DIR");
    // Mock LLM irrelevant here -- load_or_default() swallows errors.
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");

    let config = AlmsConfig::load_or_default();
    let data_path = std::path::Path::new(&config.server.data_dir);
    assert!(
        data_path.is_absolute(),
        "data_dir should be absolute after load_or_default(), got: {}",
        config.server.data_dir
    );
}

// ---- Episodic memory config tests ----

#[test]
fn test_context_config_defaults_episodic() {
    let config = ContextConfig::default();
    assert_eq!(
        config.summary_model,
        Some("minimax/minimax-m2.7".into()),
        "summary_model should default to a cheap non-reasoning model"
    );
    assert_eq!(config.run_summary_mode, RunSummaryMode::Llm);
    assert_eq!(config.run_summary_budget, 2000);
    assert_eq!(config.summary_max_tokens, 1000);
}

#[test]
fn test_normalize_episodic_valid_modes() {
    for mode in &[
        RunSummaryMode::Off,
        RunSummaryMode::Heuristic,
        RunSummaryMode::Llm,
    ] {
        let mut config = ContextConfig {
            run_summary_mode: mode.clone(),
            ..Default::default()
        };
        config.normalize_episodic();
        assert_eq!(
            config.run_summary_mode, *mode,
            "valid mode '{mode}' should be preserved",
        );
    }
}

#[test]
fn test_normalize_episodic_unknown_mode_falls_back_to_llm() {
    let mut config = ContextConfig {
        run_summary_mode: RunSummaryMode::Unknown,
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.run_summary_mode,
        RunSummaryMode::Llm,
        "unknown mode should fall back to Llm"
    );
}

#[test]
fn test_run_summary_mode_from_str_invalid() {
    let mode: RunSummaryMode = "invalid".parse().unwrap();
    assert_eq!(mode, RunSummaryMode::Unknown);
}

#[test]
fn test_run_summary_mode_from_str_empty() {
    let mode: RunSummaryMode = "".parse().unwrap();
    assert_eq!(mode, RunSummaryMode::Unknown);
}

#[test]
fn test_normalize_episodic_budget_within_cap() {
    let mut config = ContextConfig {
        max_input_tokens: 128_000,
        run_summary_budget: 2000,
        ..Default::default()
    };
    config.normalize_episodic();
    // 15% of 128_000 = 19_200, so 2000 is well within the cap
    assert_eq!(config.run_summary_budget, 2000);
}

#[test]
fn test_normalize_episodic_budget_exactly_at_cap() {
    let mut config = ContextConfig {
        max_input_tokens: 20_000,
        run_summary_budget: 3000, // 15% of 20_000 = 3_000
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.run_summary_budget, 3000,
        "budget exactly at 15% cap should be preserved"
    );
}

#[test]
fn test_normalize_episodic_budget_exceeds_cap_is_clamped() {
    let mut config = ContextConfig {
        max_input_tokens: 20_000,
        run_summary_budget: 5000, // 15% of 20_000 = 3_000
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.run_summary_budget, 3000,
        "budget exceeding 15% cap should be clamped to 3000"
    );
}

#[test]
fn test_normalize_episodic_budget_clamp_with_small_context() {
    let mut config = ContextConfig {
        max_input_tokens: 4_000,
        run_summary_budget: 2000, // 15% of 4_000 = 600
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.run_summary_budget, 600,
        "budget should be clamped to 15% of 4_000 = 600"
    );
}

#[test]
fn test_normalize_episodic_zero_summary_max_tokens_reset() {
    let mut config = ContextConfig {
        summary_max_tokens: 0,
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.summary_max_tokens, 1000,
        "zero summary_max_tokens should be reset to default 1000"
    );
}

#[test]
fn test_normalize_episodic_nonzero_summary_max_tokens_preserved() {
    let mut config = ContextConfig {
        summary_max_tokens: 500,
        ..Default::default()
    };
    config.normalize_episodic();
    assert_eq!(
        config.summary_max_tokens, 500,
        "non-zero summary_max_tokens should be preserved"
    );
}

#[test]
fn test_toml_episodic_config() {
    let toml = r#"
[context]
run_summary_mode = "heuristic"
run_summary_budget = 4000
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.context.run_summary_mode, RunSummaryMode::Heuristic);
    assert_eq!(config.context.run_summary_budget, 4000);
}

#[test]
fn test_toml_episodic_config_invalid_mode_deserializes_as_unknown() {
    let toml = r#"
[context]
run_summary_mode = "bogus"
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.context.run_summary_mode, RunSummaryMode::Unknown);
}

#[test]
fn test_env_override_run_summary_mode() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_RUN_SUMMARY_MODE", "llm");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.run_summary_mode, RunSummaryMode::Llm);
}

#[test]
fn test_env_override_run_summary_budget() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_RUN_SUMMARY_BUDGET", "5000");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.run_summary_budget, 5000);
}

#[test]
fn test_env_override_run_summary_budget_invalid_ignored() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_RUN_SUMMARY_BUDGET", "not-a-number");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    // Invalid parse should be silently ignored, keeping the default
    assert_eq!(config.context.run_summary_budget, 2000);
}

#[test]
fn test_toml_summary_max_tokens() {
    let toml = r#"
[context]
summary_max_tokens = 2000
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.context.summary_max_tokens, 2000);
}

#[test]
fn test_env_override_summary_max_tokens() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_SUMMARY_MAX_TOKENS", "1500");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.summary_max_tokens, 1500);
}

#[test]
fn test_env_override_summary_max_tokens_invalid_ignored() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_SUMMARY_MAX_TOKENS", "not-a-number");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    // Invalid parse should be silently ignored, keeping the default
    assert_eq!(config.context.summary_max_tokens, 1000);
}

// ---- warn_legacy_data_dir / ensure_data_dir tests ----

/// Helper: create a temp dir with a unique UUID suffix, returning the
/// root path. The caller is responsible for cleanup via `remove_dir_all`.
fn temp_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("alms-cfg-{label}-{}", uuid::Uuid::new_v4()))
}

/// Fresh install: no `./data/`, no `.alms/` — `ensure_data_dir` creates
/// the `.alms/` directory.
#[test]
fn test_ensure_data_dir_fresh_install() {
    let root = temp_root("fresh");
    std::fs::create_dir_all(&root).unwrap();

    let alms_dir = root.join(".alms");
    assert!(!alms_dir.exists(), "precondition: .alms should not exist");
    assert!(
        !root.join("data").exists(),
        "precondition: data should not exist"
    );

    let mut cfg = AlmsConfig::default();
    cfg.server.data_dir = alms_dir.to_string_lossy().into_owned();

    cfg.ensure_data_dir();

    assert!(
        alms_dir.is_dir(),
        ".alms/ should be created by ensure_data_dir"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Legacy: `./data/` exists but `.alms/` does not — `warn_legacy_data_dir`
/// should fire (no panic, method completes successfully).
#[test]
fn test_warn_legacy_data_dir_legacy_exists() {
    let root = temp_root("legacy");
    std::fs::create_dir_all(root.join("data")).unwrap();

    let alms_dir = root.join(".alms");
    assert!(!alms_dir.exists(), "precondition: .alms should not exist");
    assert!(
        root.join("data").is_dir(),
        "precondition: data should exist"
    );

    let mut cfg = AlmsConfig::default();
    cfg.server.data_dir = alms_dir.to_string_lossy().into_owned();

    // Should not panic — the warning is emitted via tracing.
    cfg.warn_legacy_data_dir();

    let _ = std::fs::remove_dir_all(&root);
}

/// Both `./data/` and `.alms/` exist — no warning (already migrated).
#[test]
fn test_warn_legacy_data_dir_both_exist() {
    let root = temp_root("both");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::create_dir_all(root.join(".alms")).unwrap();

    let alms_dir = root.join(".alms");

    let mut cfg = AlmsConfig::default();
    cfg.server.data_dir = alms_dir.to_string_lossy().into_owned();

    // Should complete without issue (legacy dir present but .alms also exists).
    cfg.warn_legacy_data_dir();

    let _ = std::fs::remove_dir_all(&root);
}

/// Neither `./data/` nor `.alms/` exist — no warning (fresh install).
#[test]
fn test_warn_legacy_data_dir_neither_exists() {
    let root = temp_root("neither");
    std::fs::create_dir_all(&root).unwrap();

    let alms_dir = root.join(".alms");
    assert!(!alms_dir.exists());
    assert!(!root.join("data").exists());

    let mut cfg = AlmsConfig::default();
    cfg.server.data_dir = alms_dir.to_string_lossy().into_owned();

    // No legacy dir, no .alms — should be silent.
    cfg.warn_legacy_data_dir();

    let _ = std::fs::remove_dir_all(&root);
}

/// Custom `data_dir` (not ending in `.alms`) — no warning regardless of
/// whether a `data` directory exists nearby, because the user explicitly
/// chose a non-default path.
#[test]
fn test_warn_legacy_data_dir_custom_data_dir() {
    let root = temp_root("custom");
    std::fs::create_dir_all(root.join("data")).unwrap();

    // Point data_dir to a custom path that does NOT end in `.alms`
    let custom_dir = root.join("my-custom-dir");

    let mut cfg = AlmsConfig::default();
    cfg.server.data_dir = custom_dir.to_string_lossy().into_owned();

    // Even though ./data/ exists, custom data_dir should suppress the warning.
    cfg.warn_legacy_data_dir();

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Generic OpenAI-compatible provider config (issue #765)
// ---------------------------------------------------------------------------

/// The generic `[llm.providers.<name>]` table parses from TOML and threads
/// through to the `providers` map on `LlmConfig`.
#[test]
fn test_llm_providers_generic_form_parses() {
    let toml_str = r#"
        [llm]
        provider = "xai"
        base_url = "https://openrouter.ai/api/v1"
        model = "grok-4"

        [llm.providers.xai]
        kind = "openai_compatible"
        base_url = "https://api.x.ai/v1"
        api_key_env = "XAI_API_KEY"
        model = "grok-4"
        auth_scheme = { type = "bearer" }

        [llm.providers.xai.quirks]
        tool_gap_fill = false
        drop_empty_content = true
    "#;

    let cfg: AlmsConfig = toml::from_str(toml_str).expect("parses");
    let entry = cfg.llm.providers.get("xai").expect("xai entry present");
    assert_eq!(entry.base_url, "https://api.x.ai/v1");
    assert_eq!(entry.api_key_env.as_deref(), Some("XAI_API_KEY"));
    assert_eq!(entry.model.as_deref(), Some("grok-4"));
    assert_eq!(entry.kind, ProviderKind::OpenAiCompatible);
    assert!(matches!(entry.auth_scheme, AuthScheme::Bearer));
    assert!(!entry.quirks.tool_gap_fill);
    assert!(entry.quirks.drop_empty_content);
}

/// Entries default `kind = "openai_compatible"` when the field is omitted,
/// so adding a new provider is as minimal as `base_url` + `api_key_env`.
#[test]
fn test_llm_providers_kind_defaults_to_openai_compatible() {
    let toml_str = r#"
        [llm]
        provider = "ollama"

        [llm.providers.ollama]
        base_url = "http://localhost:11434/v1"
    "#;
    let cfg: AlmsConfig = toml::from_str(toml_str).unwrap();
    let entry = cfg.llm.providers.get("ollama").unwrap();
    assert_eq!(entry.kind, ProviderKind::OpenAiCompatible);
}

/// Anthropic sugar is auto-populated on load with the right kind and
/// auth scheme even when the user writes nothing but `provider = "anthropic"`.
#[test]
fn test_ensure_builtin_providers_anthropic_sugar() {
    let mut cfg = LlmConfig::default();
    cfg.ensure_builtin_providers();
    let anthropic = cfg.providers.get("anthropic").expect("anthropic sugar");
    assert_eq!(anthropic.kind, ProviderKind::Anthropic);
    assert_eq!(anthropic.base_url, "https://api.anthropic.com/v1");
    match &anthropic.auth_scheme {
        AuthScheme::Header { name } => assert_eq!(name, "x-api-key"),
        other => panic!("expected x-api-key header, got {other:?}"),
    }
}

/// User-defined entries for sugar names must not be silently overwritten
/// by `ensure_builtin_providers`.
#[test]
fn test_ensure_builtin_providers_preserves_user_entries() {
    let toml_str = r#"
        [llm]
        provider = "openai"

        [llm.providers.openai]
        base_url = "http://localhost:9999/v1"
    "#;
    let mut cfg: AlmsConfig = toml::from_str(toml_str).unwrap();
    cfg.llm.ensure_builtin_providers();
    assert_eq!(
        cfg.llm.providers.get("openai").unwrap().base_url,
        "http://localhost:9999/v1"
    );
}

/// `validate()` rejects provider selectors that match neither a built-in
/// sugar name nor a user-declared entry.
#[test]
fn test_validation_unknown_provider_rejected() {
    let mut cfg = AlmsConfig::default();
    cfg.llm.provider = "does-not-exist".into();
    let err = cfg.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist"),
        "error should mention bad provider name: {msg}"
    );
    assert!(
        msg.contains("llm.providers"),
        "error should point at the providers table: {msg}"
    );
}

/// `validate()` accepts a user-declared provider entry.
#[test]
fn test_validation_generic_provider_accepted() {
    let mut cfg = AlmsConfig::default();
    cfg.llm.provider = "xai".into();
    cfg.llm.providers.insert(
        "xai".into(),
        ProviderEntry {
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            api_key: None,
            model: Some("grok-4".into()),
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        },
    );
    assert!(cfg.validate().is_ok());
}

/// A provider entry with an empty `base_url` is a config error — it
/// would produce nonsense URLs at request time.
#[test]
fn test_validation_empty_base_url_rejected() {
    let mut cfg = AlmsConfig::default();
    cfg.llm
        .providers
        .get_mut("openai")
        .unwrap()
        .base_url
        .clear();
    let err = cfg.validate().unwrap_err();
    assert!(err.to_string().contains("base_url"), "got: {err}");
}

/// `ProviderEntry::resolve_api_key` reads from `api_key_env` when the
/// named environment variable is set and non-empty.
#[test]
fn test_resolve_api_key_from_env() {
    let _guard = SingleEnvGuard::set("XAI_API_KEY_FOR_TEST", "xai-secret-value");
    let entry = ProviderEntry {
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.x.ai/v1".into(),
        api_key_env: Some("XAI_API_KEY_FOR_TEST".into()),
        api_key: None,
        model: None,
        auth_scheme: AuthScheme::Bearer,
        quirks: ProviderQuirks::default(),
    };
    assert_eq!(entry.resolve_api_key().as_deref(), Some("xai-secret-value"));
}

/// `api_key_env` takes precedence over `api_key` when both are set.
#[test]
fn test_resolve_api_key_env_beats_literal() {
    let _guard = SingleEnvGuard::set("XAI_API_KEY_PRECEDENCE", "from-env");
    let entry = ProviderEntry {
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.x.ai/v1".into(),
        api_key_env: Some("XAI_API_KEY_PRECEDENCE".into()),
        api_key: Some("from-literal".into()),
        model: None,
        auth_scheme: AuthScheme::Bearer,
        quirks: ProviderQuirks::default(),
    };
    assert_eq!(entry.resolve_api_key().as_deref(), Some("from-env"));
}

/// With no env var and no literal, resolution returns None (the gateway
/// then falls back to the secrets store).
#[test]
fn test_resolve_api_key_none_when_nothing_configured() {
    let entry = ProviderEntry {
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.x.ai/v1".into(),
        api_key_env: Some("DEFINITELY_UNSET_ENV_VAR_765".into()),
        api_key: None,
        model: None,
        auth_scheme: AuthScheme::Bearer,
        quirks: ProviderQuirks::default(),
    };
    // Ensure the var really is unset.
    unsafe { std::env::remove_var("DEFINITELY_UNSET_ENV_VAR_765") };
    assert!(entry.resolve_api_key().is_none());
}

/// `ALMS_LLM_PROVIDER` env-var override selects a user-declared generic
/// provider by name (the env var wins over the file's `provider` key).
#[test]
fn test_env_var_selects_generic_provider() {
    let _guard = EnvGuard::set(&[("ALMS_LLM_PROVIDER", Some("groq"))]);
    let mut cfg = AlmsConfig::default();
    cfg.llm.providers.insert(
        "groq".into(),
        ProviderEntry {
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key_env: Some("GROQ_API_KEY".into()),
            api_key: None,
            model: Some("llama-3.3-70b".into()),
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        },
    );
    cfg.apply_env_overrides();
    assert_eq!(cfg.llm.provider, "groq");
    assert!(cfg.validate().is_ok());
}

/// Custom header auth scheme parses from TOML.
#[test]
fn test_auth_scheme_header_parses() {
    let toml_str = r#"
        [llm]
        provider = "custom"

        [llm.providers.custom]
        base_url = "https://example.com/v1"
        auth_scheme = { type = "header", name = "X-Custom-Key" }
    "#;
    let cfg: AlmsConfig = toml::from_str(toml_str).unwrap();
    let entry = cfg.llm.providers.get("custom").unwrap();
    match &entry.auth_scheme {
        AuthScheme::Header { name } => assert_eq!(name, "X-Custom-Key"),
        other => panic!("expected Header, got {other:?}"),
    }
}

// --------------------------------------------------------------------
// Env-var override regression tests (PR #770 review follow-up)
// --------------------------------------------------------------------

/// `ALMS_LLM_BASE_URL` must propagate into the resolved sugar provider
/// entry, not just the flat `llm.base_url`. Regression: previously the
/// runtime `From<LlmConfig>` impl preferred `entry.base_url` which held
/// the hardcoded default, silently discarding the env override.
#[test]
fn test_env_override_base_url_propagates_to_openai_sugar_entry() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "http://local-proxy:9999/v1");
    let _legacy = SingleEnvGuard::remove("LLM_BASE_URL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "openai");

    let mut cfg = AlmsConfig::default();
    // Mirror the `load()` order: ensure sugar entries, then apply env.
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    assert_eq!(cfg.llm.base_url, "http://local-proxy:9999/v1");
    let entry = cfg
        .llm
        .providers
        .get("openai")
        .expect("openai sugar entry present");
    assert_eq!(
        entry.base_url, "http://local-proxy:9999/v1",
        "ALMS_LLM_BASE_URL must propagate into the openai sugar entry"
    );
}

#[test]
fn test_env_override_base_url_propagates_to_openrouter_sugar_entry() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "https://proxy.example.com/v1");
    let _legacy = SingleEnvGuard::remove("LLM_BASE_URL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "openrouter");

    let mut cfg = AlmsConfig::default();
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    let entry = cfg.llm.providers.get("openrouter").unwrap();
    assert_eq!(entry.base_url, "https://proxy.example.com/v1");
}

#[test]
fn test_env_override_base_url_propagates_to_anthropic_sugar_entry() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "https://anthropic-proxy.local/v1");
    let _legacy = SingleEnvGuard::remove("LLM_BASE_URL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "anthropic");

    let mut cfg = AlmsConfig::default();
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    let entry = cfg.llm.providers.get("anthropic").unwrap();
    assert_eq!(entry.base_url, "https://anthropic-proxy.local/v1");
}

/// Same story for `ALMS_LLM_MODEL`: the env override must win over the
/// (currently empty) `entry.model` so the runtime picks it up.
#[test]
fn test_env_override_model_propagates_to_resolved_entry() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_MODEL", "my-env-model");
    let _legacy = SingleEnvGuard::remove("DEFAULT_MODEL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "openai");

    let mut cfg = AlmsConfig::default();
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    assert_eq!(cfg.llm.model, "my-env-model");
    let entry = cfg.llm.providers.get("openai").unwrap();
    assert_eq!(entry.model.as_deref(), Some("my-env-model"));
}

/// Env-override propagation also applies to user-declared generic
/// providers: setting `ALMS_LLM_BASE_URL` after selecting an xai entry
/// should overwrite that entry's base_url.
#[test]
fn test_env_override_base_url_propagates_to_user_declared_provider() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "http://override.local/v1");
    let _legacy = SingleEnvGuard::remove("LLM_BASE_URL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "xai");

    let mut cfg = AlmsConfig::default();
    cfg.llm.providers.insert(
        "xai".into(),
        ProviderEntry {
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            api_key: None,
            model: Some("grok-4".into()),
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        },
    );
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    let entry = cfg.llm.providers.get("xai").unwrap();
    assert_eq!(entry.base_url, "http://override.local/v1");
}

/// Inline `api_key` in TOML should deserialize into the entry, matching
/// the dev-convenience affordance documented on the field. Prior to the
/// PR #770 review follow-up, `#[serde(skip)]` silently dropped the value.
#[test]
fn test_provider_entry_inline_api_key_deserializes_from_toml() {
    let toml_str = r#"
        [llm]
        provider = "customlab"

        [llm.providers.customlab]
        base_url = "https://customlab.example.com/v1"
        api_key  = "sk-inline-dev-key"
    "#;
    let cfg: AlmsConfig = toml::from_str(toml_str).unwrap();
    let entry = cfg.llm.providers.get("customlab").unwrap();
    assert_eq!(entry.api_key.as_deref(), Some("sk-inline-dev-key"));
}

/// Inline `api_key` must not serialize back out (no round-trip of secrets
/// to disk). The docstring guarantees this.
#[test]
fn test_provider_entry_inline_api_key_does_not_serialize() {
    let entry = ProviderEntry {
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://example.com/v1".into(),
        api_key_env: None,
        api_key: Some("sk-should-be-hidden".into()),
        model: None,
        auth_scheme: AuthScheme::Bearer,
        quirks: ProviderQuirks::default(),
    };
    let out = toml::to_string(&entry).unwrap();
    assert!(
        !out.contains("sk-should-be-hidden"),
        "inline api_key must never round-trip through serialize: {out}"
    );
    assert!(
        !out.contains("api_key ="),
        "api_key field must be absent from serialized TOML: {out}"
    );
}

/// `validate()` no longer emits the "No LLM API key configured" warning
/// when the selected provider entry declares an env-var-backed key.
/// This is a smoke-level check — we only assert the call returns Ok.
/// (The warning goes to `tracing`; suppressing it completely keeps users
/// who adopted the declarative shape from seeing spurious noise.)
#[test]
fn test_validation_ok_when_provider_entry_declares_api_key_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Ensure no API key is present on the flat config.
    let mut cfg = AlmsConfig::default();
    cfg.llm.api_key = None;
    cfg.llm.provider = "mylab".into();
    cfg.llm.providers.insert(
        "mylab".into(),
        ProviderEntry {
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://mylab.example.com/v1".into(),
            api_key_env: Some("MYLAB_API_KEY".into()),
            api_key: None,
            model: None,
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        },
    );
    assert!(cfg.validate().is_ok());
}
