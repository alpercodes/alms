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
fn test_toml_stream_chunk_timeout() {
    let toml = r#"
[llm]
stream_chunk_timeout_secs = 120
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.llm.stream_chunk_timeout_secs, 120);
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
