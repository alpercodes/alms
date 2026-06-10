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

// Test-only env-var mutexes — disjointness contract.
//
// `ENV_LOCK` and `BUDGET_ENV_LOCK` (defined further down) each guard a
// disjoint var-set so a test holding one mutex cannot observe a partial
// mutation guarded by the other. The contract is:
//
//   - `ENV_LOCK`         covers `LLM_ENV_VARS` (LLM API keys + provider tag).
//   - `BUDGET_ENV_LOCK`  covers `ALMS_LLM_BUDGET_VALIDATION` only.
//
// The same disjointness rule applies cross-crate: the gateway's
// `BUDGET_VALIDATION_ENV_LOCK` (in `crates/alms-gateway/src/lib.rs::test_env_locks`)
// guards the same `ALMS_LLM_BUDGET_VALIDATION` var but a different test
// process (each crate's `cargo test` runs in its own process), so the
// two never need to interlock — just stay disjoint with their respective
// `ENV_LOCK`s. A future test that needs to span both var-sets in one
// crate must acquire the locks in a fixed order (e.g. `ENV_LOCK` first,
// then `BUDGET_ENV_LOCK`) to avoid deadlock; today no such test exists.
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
    // #869: recent_window is gone; the new defaults are threshold-based.
    assert_eq!(config.context.compact_trigger_pct, 0.80);
    assert_eq!(config.context.compact_retain_pct, 0.40);
    assert!(config.llm.api_key.is_none());
}

#[test]
fn test_config_from_toml() {
    // #869: legacy fields (`recent_window`, strategy = "sliding-summary")
    // remain accepted on the wire so existing alms.toml files don't break.
    // The deserialiser rewrites `"sliding-summary"` → `"compact"` and
    // drops `recent_window` with a one-time WARN.
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
    // Alias rewritten by ContextConfig::Deserialize.
    assert_eq!(config.context.strategy, "compact");
    assert_eq!(config.context.max_input_tokens, 16000);
    // Defaults preserved for unset fields
    assert_eq!(config.tools.timeout_secs, 30);
}

#[test]
fn test_shell_engine_defaults_to_system_bash() {
    // #1143: the new engine knob must default to today's behavior so the
    // feature is strictly additive — no existing deployment changes.
    let config = AlmsConfig::default();
    assert_eq!(config.tools.shell_engine, ShellEngine::SystemBash);

    // Absent from TOML → default.
    let config: AlmsConfig = toml::from_str("[tools]\ntimeout_secs = 10\n").unwrap();
    assert_eq!(config.tools.shell_engine, ShellEngine::SystemBash);
}

#[test]
fn test_shell_engine_parses_from_toml() {
    // #1143: kebab-case wire values per the issue spec.
    let config: AlmsConfig = toml::from_str("[tools]\nshell_engine = \"builtin\"\n").unwrap();
    assert_eq!(config.tools.shell_engine, ShellEngine::Builtin);

    let config: AlmsConfig = toml::from_str("[tools]\nshell_engine = \"system-bash\"\n").unwrap();
    assert_eq!(config.tools.shell_engine, ShellEngine::SystemBash);

    // Anything else is a hard parse error, not a silent fallback.
    assert!(toml::from_str::<AlmsConfig>("[tools]\nshell_engine = \"pwsh\"\n").is_err());
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
    // #869: recent_window is gone; the new compact_* knobs default to
    // 0.80 / 0.40 (Claude Code parity).
    assert_eq!(config.context.compact_trigger_pct, 0.80);
    assert_eq!(config.context.compact_retain_pct, 0.40);
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

// ---------------------------------------------------------------------------
// Token-budget cross-validation (#919)
// ---------------------------------------------------------------------------

const ALMS_LLM_BUDGET_VALIDATION: &str = "ALMS_LLM_BUDGET_VALIDATION";

/// Test-only mutex serialising every token-budget validation test that
/// reads `ALMS_LLM_BUDGET_VALIDATION`. `SingleEnvGuard` is per-var (no
/// cross-test mutual exclusion); without this lock parallel tests would
/// race each other's env-var mutations and produce flaky results.
///
/// **Disjointness contract:** `BUDGET_ENV_LOCK` guards
/// `ALMS_LLM_BUDGET_VALIDATION` only — its var-set is disjoint from
/// `ENV_LOCK` (`LLM_ENV_VARS`). See the module-level comment near
/// `ENV_LOCK` for the full disjointness story (including the
/// cross-crate sibling in `alms-gateway::test_env_locks`).
static BUDGET_ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_token_budget_default_config_passes() {
    // Default config: provider=openrouter, model=moonshotai/kimi-k2.6.
    // The table doesn't know that pair → validator must skip → ok.
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let config = AlmsConfig::default();
    assert!(
        config.validate().is_ok(),
        "default config (unknown openrouter model) must pass token-budget validation"
    );
}

#[test]
fn test_token_budget_strict_rejects_overshoot_anthropic() {
    // Anthropic Claude Haiku 4.5 caps at 200K (Opus 4.7 / Sonnet 4.6 are
    // 1M post-2026-05-09 verification round). 250K input + 32K default
    // output = 282K > 200K → overshoots.
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-haiku-4-5".into();
    config.context.max_input_tokens = 250_000;
    config.session.max_context_tokens = 250_000;

    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("max_input_tokens"),
        "error must mention max_input_tokens: {msg}"
    );
    assert!(
        msg.contains("max_tokens"),
        "error must mention max_tokens: {msg}"
    );
    assert!(
        msg.contains("anthropic"),
        "error must mention provider: {msg}"
    );
    assert!(
        msg.contains("200000"),
        "error must mention provider cap: {msg}"
    );
}

#[test]
fn test_token_budget_strict_accepts_fitting_anthropic() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-haiku-4-5".into();
    // 128K input + 32K default output = 160K — fits 200K cap.
    config.context.max_input_tokens = 128_000;
    assert!(config.validate().is_ok());
}

#[test]
fn test_token_budget_strict_rejects_overshoot_gemini() {
    // gemini-2.5-pro publishes a 1,048,576-token cap. 1.5M input + 32K
    // default output = ~1.532M → overshoots.
    //
    // Replaces the prior deepseek_v3_64k overshoot fixture: under the
    // 2026-05-09 verification round DeepSeek V4 Flash sits at 1M, so the
    // old 64K / 128K aliases no longer exist in the table.
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.provider = "gemini".into();
    config.llm.model = "gemini-2.5-pro".into();
    config.context.max_input_tokens = 1_500_000;
    // session.max_context_tokens has its own ≥ max_input_tokens validator
    // — bump it to keep that check satisfied so the budget validator
    // gets to run.
    config.session.max_context_tokens = 1_500_000;
    // The default provider table has no `gemini` entry — register one so
    // `validate()`'s "provider must be defined" check passes and the
    // budget validator runs.
    config.llm.providers.insert(
        "gemini".into(),
        ProviderEntry {
            kind: ProviderKind::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            api_key_env: None,
            api_key: None,
            model: None,
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        },
    );
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("1048576"),
        "error must mention provider cap: {err}"
    );
}

#[test]
fn test_token_budget_warn_mode_accepts_overshoot() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "warn");
    let mut config = AlmsConfig::default();
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-haiku-4-5".into();
    config.context.max_input_tokens = 250_000;
    config.session.max_context_tokens = 250_000;
    assert!(
        config.validate().is_ok(),
        "warn mode must accept overshooting configs"
    );
}

#[test]
fn test_token_budget_unknown_pair_skips() {
    // Provider in the table but model unknown → skip silently regardless
    // of size. Pre-#919 this was the entire validation surface.
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-2.1".into(); // not in the table
    config.context.max_input_tokens = 1_000_000;
    config.session.max_context_tokens = 1_000_000;
    assert!(
        config.validate().is_ok(),
        "unknown model must skip the budget check rather than false-positive"
    );
}

#[test]
fn test_token_budget_skipped_for_mock_mode() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.mock = true;
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-opus-4-7".into();
    config.context.max_input_tokens = 200_000;
    config.session.max_context_tokens = 200_000;
    assert!(
        config.validate().is_ok(),
        "mock mode must bypass token-budget validation entirely"
    );
}

#[test]
fn test_token_budget_provider_entry_model_override_wins() {
    // The validator must use the resolved per-provider-entry model
    // (`[llm.providers.<name>].model`) when set, mirroring the runtime
    // adapter's resolution. With anthropic/claude-opus-4-7 + 200K input
    // the budget overshoots, but if the entry overrides to a non-table
    // model id the check should fall through to the unknown-model skip.
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    let mut config = AlmsConfig::default();
    config.llm.provider = "anthropic".into();
    config.llm.model = "claude-opus-4-7".into();
    config.context.max_input_tokens = 200_000;
    config.session.max_context_tokens = 200_000;
    config.llm.ensure_builtin_providers();
    if let Some(entry) = config.llm.providers.get_mut("anthropic") {
        // Override to an unknown model so the table lookup misses and
        // the validator skips.
        entry.model = Some("claude-99-future-model".into());
    }
    assert!(
        config.validate().is_ok(),
        "provider-entry model override must take precedence at the validator"
    );
}

#[test]
fn test_validation_mode_from_env_default_strict() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    assert_eq!(ValidationMode::from_env(), ValidationMode::Strict);
}

#[test]
fn test_validation_mode_from_env_warn() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "warn");
    assert_eq!(ValidationMode::from_env(), ValidationMode::Warn);
}

#[test]
fn test_validation_mode_from_env_warn_case_insensitive() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "WARN");
    assert_eq!(ValidationMode::from_env(), ValidationMode::Warn);
}

#[test]
fn test_validation_mode_from_env_unknown_falls_back_to_strict() {
    let _lock = BUDGET_ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "yolo");
    assert_eq!(
        ValidationMode::from_env(),
        ValidationMode::Strict,
        "unknown values must fall back to strict so a typoed opt-out doesn't \
         silently disable enforcement"
    );
}

#[test]
fn test_agents_dir_default_uses_resolved_project_root() {
    // After #945 the agent metadata directory lives under the project
    // root rather than the data directory: `<project>/.alms/agents/`.
    // The default `ServerConfig` resolves the project root from
    // `current_dir()`, so the agents-dir suffix is what we pin here.
    let _lock = ENV_LOCK.lock().unwrap();
    let _ws_guard = SingleEnvGuard::remove("ALMS_WORKSPACE_DIR");
    let _project_guard = SingleEnvGuard::remove("ALMS_PROJECT_ROOT");

    let config = ServerConfig::default();
    let agents = config.agents_dir();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(agents, cwd.join(".alms").join("agents"));
}

#[test]
fn test_agents_dir_legacy_workspace_env_override_still_wins() {
    // `ALMS_WORKSPACE_DIR` is the legacy override knob; #945 keeps it as
    // the highest-precedence agents-dir override so operators can pin
    // metadata to a custom location even after the default layout flip.
    let _lock = ENV_LOCK.lock().unwrap();
    let _ws_guard = SingleEnvGuard::set("ALMS_WORKSPACE_DIR", "/custom/workspace");

    let config = ServerConfig::default();
    assert_eq!(config.agents_dir(), PathBuf::from("/custom/workspace"));
}

#[test]
fn test_project_root_precedence_explicit_field_wins() {
    // Precedence: `self.project_root` (CLI) > env > current_dir.
    // The CLI flag writes the absolute path into the field directly,
    // so a non-empty field beats the env var.
    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = SingleEnvGuard::set("ALMS_PROJECT_ROOT", "/from/env");

    let config = ServerConfig {
        project_root: "/from/cli".into(),
        ..Default::default()
    };
    assert_eq!(
        config.resolved_project_root(),
        PathBuf::from("/from/cli"),
        "explicit project_root field (CLI flag) must beat the env var"
    );
}

#[test]
fn test_project_root_precedence_env_wins_over_cwd() {
    // No CLI override (field empty) → env var wins over `current_dir`.
    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = SingleEnvGuard::set("ALMS_PROJECT_ROOT", "/from/env");

    let config = ServerConfig::default();
    assert_eq!(
        config.resolved_project_root(),
        PathBuf::from("/from/env"),
        "ALMS_PROJECT_ROOT must beat the current_dir fallback"
    );
}

#[test]
fn test_project_root_precedence_cwd_fallback() {
    // No CLI override and no env var → `current_dir` fallback.
    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = SingleEnvGuard::remove("ALMS_PROJECT_ROOT");

    let config = ServerConfig::default();
    let resolved = config.resolved_project_root();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        resolved, cwd,
        "with no CLI override and no env var, project_root falls back to current_dir"
    );
}

#[test]
fn test_project_root_precedence_empty_env_falls_through_to_cwd() {
    // An explicit empty `ALMS_PROJECT_ROOT="" ` should NOT pin the project
    // root to `""` — the resolver treats an empty env value the same as
    // an unset variable so the cwd fallback wins. Mirrors the empty-field
    // discipline (`if !self.project_root.is_empty()` in the resolver).
    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = SingleEnvGuard::set("ALMS_PROJECT_ROOT", "");

    let config = ServerConfig::default();
    let resolved = config.resolved_project_root();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(resolved, cwd);
}

#[test]
fn test_agents_dir_follows_project_root() {
    // With no `ALMS_WORKSPACE_DIR` and an explicit project_root, the
    // agents directory anchors at `<project_root>/.alms/agents/`.
    let _lock = ENV_LOCK.lock().unwrap();
    let _ws_guard = SingleEnvGuard::remove("ALMS_WORKSPACE_DIR");
    let _project_guard = SingleEnvGuard::remove("ALMS_PROJECT_ROOT");

    let config = ServerConfig {
        project_root: "/my/project".into(),
        ..Default::default()
    };
    assert_eq!(
        config.agents_dir(),
        PathBuf::from("/my/project").join(".alms").join("agents")
    );
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
    // #924: post-fail-fast, `load_or_default()` runs `validate()`. The
    // default config (and env-overridden config in this test) passes
    // validate without needing mock mode — `validate()` only WARNS
    // about a missing API key and does not return Err. Mock mode is
    // irrelevant here.
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
    // #872 changed the default from Some("minimax/minimax-m2.7") + None
    // to None + None so the shipped baseline doesn't sit in the
    // asymmetric (model-only) shape that the new pair-only validator
    // would otherwise reject. Operators opt into a dedicated summary
    // task by setting both fields together.
    //
    // Atlas's "default summary model = kimi-k2.6" directive is
    // satisfied implicitly: when both are None, the summary task
    // inherits the agent's (provider, model) — and the chat default
    // is now `(openrouter, moonshotai/kimi-k2.6)`, so summarisation
    // hits kimi-k2.6 on a fresh boot without an explicit summary pair.
    assert_eq!(
        config.summary_model, None,
        "summary_model defaults to None — when unset, the summary task \
         inherits the agent's primary (provider, model), which is now \
         kimi-k2.6 on openrouter by default"
    );
    assert_eq!(
        config.summary_provider, None,
        "summary_provider mirrors summary_model — both default to None \
         post-#872 so the pair-only invariant holds out of the box"
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

// --------------------------------------------------------------------------
// Anthropic extended-thinking config (issue #767)
// --------------------------------------------------------------------------

/// `[llm.anthropic].thinking_budget_tokens` deserializes correctly from
/// TOML and round-trips through the canonical `LlmConfig` default path.
#[test]
fn test_anthropic_thinking_budget_toml_roundtrip() {
    let toml_str = r#"
[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"

[llm.anthropic]
thinking_budget_tokens = 8192
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("valid TOML");
    assert_eq!(cfg.llm.anthropic.thinking_budget_tokens, 8192);
    // Round-trip: serialize and deserialize again — the field must survive.
    let re = toml::to_string(&cfg).expect("serialize");
    let cfg2: AlmsConfig = toml::from_str(&re).expect("reparse");
    assert_eq!(cfg2.llm.anthropic.thinking_budget_tokens, 8192);
}

/// Omitted `[llm.anthropic]` section defaults to the compiled
/// `AnthropicConfig::default()` value for `thinking_budget_tokens`.
/// The default flipped from 0 to 2048 (extended thinking enabled
/// out of the box) — this test pins the new default so a regression
/// back to 0 surfaces here.
#[test]
fn test_anthropic_thinking_budget_default_matches_compiled() {
    let toml_str = r#"
[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("valid TOML");
    assert_eq!(
        cfg.llm.anthropic.thinking_budget_tokens,
        crate::config::AnthropicConfig::default().thinking_budget_tokens,
    );
    // And the chosen default is 2048 — pinned so a silent flip to a
    // different non-zero value (e.g. 1024) also surfaces here.
    assert_eq!(cfg.llm.anthropic.thinking_budget_tokens, 2048);
}

// --------------------------------------------------------------------------
// OpenAI-compat reasoning-effort config (issue #768)
// --------------------------------------------------------------------------

use crate::config::ReasoningEffort;

/// `[llm.openai].reasoning_effort = "high"` deserializes correctly and
/// round-trips through `toml::to_string` → reparse.
#[test]
fn test_openai_reasoning_effort_toml_roundtrip() {
    let toml_str = r#"
[llm]
provider = "openai"
model = "o3-mini"

[llm.openai]
reasoning_effort = "high"
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("valid TOML");
    assert_eq!(cfg.llm.openai.reasoning_effort, Some(ReasoningEffort::High));
    let re = toml::to_string(&cfg).expect("serialize");
    let cfg2: AlmsConfig = toml::from_str(&re).expect("reparse");
    assert_eq!(
        cfg2.llm.openai.reasoning_effort,
        Some(ReasoningEffort::High)
    );
}

/// Omitted `[llm.openai]` section defaults to `reasoning_effort = None`
/// (no param on the wire), preserving behaviour for existing configs
/// that don't opt in.
#[test]
fn test_openai_reasoning_effort_default_none() {
    let toml_str = r#"
[llm]
provider = "openai"
model = "gpt-4o"
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("valid TOML");
    assert!(cfg.llm.openai.reasoning_effort.is_none());
}

/// Each valid wire value (`low`/`medium`/`high`/`minimal`) deserializes
/// into the right enum variant.
#[test]
fn test_openai_reasoning_effort_all_values_parse() {
    for (wire, expected) in [
        ("low", ReasoningEffort::Low),
        ("medium", ReasoningEffort::Medium),
        ("high", ReasoningEffort::High),
        ("minimal", ReasoningEffort::Minimal),
    ] {
        let toml_str = format!(
            r#"
[llm]
provider = "openai"
model = "o3-mini"

[llm.openai]
reasoning_effort = "{wire}"
"#
        );
        let cfg: AlmsConfig =
            toml::from_str(&toml_str).unwrap_or_else(|e| panic!("failed to parse '{wire}': {e}"));
        assert_eq!(cfg.llm.openai.reasoning_effort, Some(expected));
    }
}

/// Unknown reasoning_effort values fail TOML parsing at load time —
/// operators see the typo rather than silently falling back.
#[test]
fn test_openai_reasoning_effort_invalid_value_rejected() {
    let toml_str = r#"
[llm]
provider = "openai"
model = "o3-mini"

[llm.openai]
reasoning_effort = "extreme"
"#;
    let result: Result<AlmsConfig, _> = toml::from_str(toml_str);
    assert!(
        result.is_err(),
        "invalid reasoning_effort should fail to parse, got: {:?}",
        result.map(|c| c.llm.openai.reasoning_effort)
    );
}

// ---- Symmetric pair-only validation for [context].summary_* (#877) ----
//
// The PATCH /settings layer in alms-gateway already rejects asymmetric
// updates with `SUMMARY_PROVIDER_REQUIRES_MODEL` /
// `SUMMARY_MODEL_REQUIRES_PROVIDER`. The HTTP `POST /agents` /
// `PUT /agents/{id}` validators do the same for per-agent overrides.
// Issue #877 closed the remaining gap: a hand-edited `alms.toml` with
// only one of the pair set used to start the daemon successfully and
// only fail at run time. These tests pin the load-time rejection so
// the validation gap can't sneak back in.

#[test]
fn test_validation_rejects_summary_provider_without_model() {
    let mut config = AlmsConfig::default();
    config.context.summary_provider = Some("openrouter".into());
    config.context.summary_model = None;
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("summary_provider is set") && msg.contains("summary_model is empty"),
        "expected pair-only error message about provider-without-model, got: {msg}"
    );
}

#[test]
fn test_validation_rejects_summary_model_without_provider() {
    let mut config = AlmsConfig::default();
    config.context.summary_provider = None;
    config.context.summary_model = Some("minimax/minimax-m2.7".into());
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("summary_model is set") && msg.contains("summary_provider is empty"),
        "expected pair-only error message about model-without-provider, got: {msg}"
    );
}

#[test]
fn test_validation_accepts_both_summary_fields_set() {
    let mut config = AlmsConfig::default();
    config.context.summary_provider = Some("openrouter".into());
    config.context.summary_model = Some("minimax/minimax-m2.7".into());
    assert!(
        config.validate().is_ok(),
        "both fields set together is the valid 'opt-in' shape"
    );
}

#[test]
fn test_validation_accepts_both_summary_fields_none() {
    // Default config — both None — is the shipped baseline. Must
    // continue to validate as OK so the daemon starts out of the box.
    let config = AlmsConfig::default();
    assert!(
        config.context.summary_provider.is_none() && config.context.summary_model.is_none(),
        "default config has both fields None"
    );
    assert!(config.validate().is_ok());
}

/// End-to-end: a hand-edited `alms.toml` with `[context]` set to the
/// asymmetric (provider, no-model) shape must be rejected at config
/// load — not later as a run-time error. The full `validate()` chain
/// runs, so we call `ensure_builtin_providers()` first so the
/// `[llm.providers.anthropic]` sugar entry is registered (mirroring
/// the production `from_*()` paths in `mod.rs`).
#[test]
fn test_asymmetric_summary_toml_fails_at_load_time() {
    let toml_str = r#"
[llm]
provider = "anthropic"
model = "claude-sonnet"

[context]
summary_provider = "openrouter"
"#;
    let mut cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    cfg.llm.ensure_builtin_providers();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("summary_provider is set"),
        "asymmetric TOML must be rejected at validate(), got: {err}"
    );
}

#[test]
fn test_asymmetric_summary_toml_other_direction_fails_at_load_time() {
    let toml_str = r#"
[llm]
provider = "anthropic"
model = "claude-sonnet"

[context]
summary_model = "minimax/minimax-m2.7"
"#;
    let mut cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    cfg.llm.ensure_builtin_providers();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("summary_model is set"),
        "asymmetric TOML must be rejected at validate(), got: {err}"
    );
}

// ---- #924: load_or_default fail-fast on validate() failure ----
//
// `AlmsConfig::load_or_default()` is the path the gateway entrypoint
// (`alms gateway`) uses (via `crates/alms-cli/src/main.rs:137`). Pre-#924
// it caught every `load()` error — including `validate()` violations —
// and silently fell back to compiled defaults with a stderr warning.
// That meant a hand-edited `alms.toml` with one of the pair-only
// `[context]` summary fields set (or any other `validate()` violation)
// produced a daemon that:
//   1. Printed an easily-missed stderr warning;
//   2. Started with default settings, silently discarding the operator's
//      intent;
//   3. Ran every subsequent run against the wrong config.
//
// #877's stated acceptance criterion ("Daemon refuses to start on
// hand-edited asymmetric `alms.toml`") was therefore not literally
// satisfied — `AlmsConfig::load()` did refuse, but `load_or_default()`
// was the one actually wired into the gateway boot path.
//
// The fix splits two error classes:
//   - File-load failures (IO / TOML parse) → warn + use defaults.
//     First-run / fresh-install / corrupted-file scenarios still
//     produce a working daemon.
//   - Validation failures → fatal, process exits non-zero with a clear
//     stderr message pointing at the bad field.
//
// Tests use `load_or_default_fallible()` — the inner `Result`-returning
// shape — so they can assert on the error path without aborting the
// test process. The public `load_or_default()` wraps it with
// `std::process::exit(1)` on Err. The two CWD-based tests serialise
// against `ENV_LOCK` because `find_config_file()` reads from the
// process cwd, which is shared across parallel tests.

/// Verify the fail-fast contract directly at the helper boundary:
/// `load_or_default_fallible()` must return `Err(InvalidConfig(_))`
/// when `[context]` is in the partial-pair shape #877 rejects.
///
/// We construct the partial-pair config in a temp directory and chdir
/// into it so `find_config_file()` picks up the test toml. Holding
/// `ENV_LOCK` for the whole test serialises against any other test
/// that touches process cwd or `ALMS_*` env vars.
#[test]
fn test_load_or_default_fail_fast_on_partial_summary_pair() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Clear any LLM env overrides that could otherwise force the
    // config into a different invalid shape (we want to pin the
    // pair-only invariant specifically).
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");

    let tempdir = tempfile::tempdir().expect("tempdir creates");
    let toml_path = tempdir.path().join("alms.toml");
    std::fs::write(
        &toml_path,
        r#"
[llm]
provider = "anthropic"

[context]
summary_provider = "openrouter"
"#,
    )
    .expect("toml write");

    // chdir into the tempdir so `find_config_file()` picks up our
    // partial-pair toml. Save the current cwd so we can restore it
    // even if the assertion below panics.
    let saved_cwd = std::env::current_dir().expect("cwd readable");
    std::env::set_current_dir(tempdir.path()).expect("chdir to tempdir");

    let result = AlmsConfig::load_or_default_fallible();

    // Restore cwd BEFORE asserting so a failure doesn't poison the
    // test process for other tests acquiring `ENV_LOCK`.
    std::env::set_current_dir(&saved_cwd).expect("restore cwd");

    let err = result.expect_err(
        "load_or_default_fallible must return Err on a partial-pair \
         [context] config (#924 fail-fast contract). Pre-#924 the \
         function would silently fall back to defaults and discard \
         the operator's intent.",
    );
    assert!(
        matches!(err, AlmsError::InvalidConfig(_)),
        "expected InvalidConfig variant on validate() failure, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("summary_provider is set") && msg.contains("summary_model is empty"),
        "expected pair-only error message, got: {msg}"
    );
}

/// Counterpart to the partial-pair test: when no config file is
/// present, `load_or_default_fallible()` must succeed (returns
/// defaults + env overrides). This pins the bootstrapping case so the
/// fail-fast change does not regress first-run / fresh-install.
///
/// We point cwd at an empty tempdir AND clear `HOME` so
/// `find_config_file()` cannot find the user's home `~/.config/alms/`
/// either. The result must be a valid default config.
#[test]
fn test_load_or_default_succeeds_with_no_config_file() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    // Point HOME at an empty tempdir so `~/.config/alms/config.toml`
    // resolves to a non-existent path and `find_config_file()` returns
    // None for the home branch. On Windows, `dirs_path()` reads
    // `USERPROFILE` rather than `HOME` — set both so the test works
    // cross-platform.
    let empty_home_dir = tempfile::tempdir().expect("tempdir creates");
    let _home_guard = SingleEnvGuard::set("HOME", empty_home_dir.path().to_str().unwrap());
    let _userprofile_guard =
        SingleEnvGuard::set("USERPROFILE", empty_home_dir.path().to_str().unwrap());

    let tempdir = tempfile::tempdir().expect("tempdir creates");
    // The tempdir is intentionally empty — no `alms.toml` to find.

    let saved_cwd = std::env::current_dir().expect("cwd readable");
    std::env::set_current_dir(tempdir.path()).expect("chdir to tempdir");

    let result = AlmsConfig::load_or_default_fallible();

    std::env::set_current_dir(&saved_cwd).expect("restore cwd");

    let cfg = result.expect(
        "load_or_default_fallible must succeed when no config file is \
         present (bootstrapping / first-run case). The fail-fast \
         change for #924 must not regress this path.",
    );
    // Spot-check that the defaults shape is intact.
    assert!(
        cfg.context.summary_provider.is_none() && cfg.context.summary_model.is_none(),
        "default config has both pair fields None"
    );
    assert!(
        std::path::Path::new(&cfg.server.data_dir).is_absolute(),
        "data_dir must still be resolved to an absolute path"
    );
}

/// Verify a corrupt / unparseable `alms.toml` still falls back to
/// defaults rather than aborting. This pins the
/// "file-load-failures-are-recoverable" axis of the #924 split: only
/// `validate()` errors are fatal, not IO / TOML-parse errors.
///
/// (Whether to make TOML parse errors fatal is an open design
/// question in the issue; the chosen behaviour is to keep them
/// recoverable so a corrupted file doesn't lock the operator out of
/// running the daemon at all. The `validate()` axis, post-fallback, is
/// guaranteed to pass on defaults.)
#[test]
fn test_load_or_default_falls_back_to_defaults_on_unparseable_toml() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let empty_home_dir = tempfile::tempdir().expect("tempdir creates");
    let _home_guard = SingleEnvGuard::set("HOME", empty_home_dir.path().to_str().unwrap());
    let _userprofile_guard =
        SingleEnvGuard::set("USERPROFILE", empty_home_dir.path().to_str().unwrap());

    let tempdir = tempfile::tempdir().expect("tempdir creates");
    let toml_path = tempdir.path().join("alms.toml");
    std::fs::write(&toml_path, "not valid toml { } [unclosed").expect("toml write");

    let saved_cwd = std::env::current_dir().expect("cwd readable");
    std::env::set_current_dir(tempdir.path()).expect("chdir to tempdir");

    let result = AlmsConfig::load_or_default_fallible();

    std::env::set_current_dir(&saved_cwd).expect("restore cwd");

    // Corrupt-file fallback returns Ok(defaults), not Err — the file
    // axis is recoverable. The validate-axis is fatal (covered by
    // `test_load_or_default_fail_fast_on_partial_summary_pair`).
    let cfg = result.expect(
        "an unparseable alms.toml must fall back to defaults, not \
         abort — only validate() failures are fatal under #924",
    );
    assert!(
        cfg.context.summary_provider.is_none() && cfg.context.summary_model.is_none(),
        "fallback config should have default summary pair (both None)"
    );
}

// ── #947: SecurityConfig (allow_full_os_access) ──────────────────────

/// Default `[security]` is empty — no agent has full-OS access.
#[test]
fn security_config_default_is_empty() {
    let security = SecurityConfig::default();
    assert!(
        security.allow_full_os_access.is_empty(),
        "default allow_full_os_access list must be empty"
    );
    // The matcher rejects every conceivable name — including the empty
    // string — when the list is empty.
    assert!(!security.is_full_os_access_agent(""));
    assert!(!security.is_full_os_access_agent("alice"));
}

/// `is_full_os_access_agent` is exact-match on the agent's registry name.
#[test]
fn security_config_match_is_exact() {
    let security = SecurityConfig {
        allow_full_os_access: vec!["alice".into(), "bob".into()],
    };
    assert!(security.is_full_os_access_agent("alice"));
    assert!(security.is_full_os_access_agent("bob"));
    assert!(!security.is_full_os_access_agent("carol"));
    // No prefix / case-insensitivity / substring matches.
    assert!(!security.is_full_os_access_agent("ali"));
    assert!(!security.is_full_os_access_agent("Alice"));
    assert!(!security.is_full_os_access_agent("alicebot"));
    // Empty input never matches even when the list is non-empty —
    // unnamed/ephemeral agents cannot be on the list by construction.
    assert!(!security.is_full_os_access_agent(""));
}

/// `[security]` parses cleanly from TOML and survives a full `AlmsConfig`
/// round-trip, including under `validate()`.
#[test]
fn security_config_parses_from_toml() {
    let toml = r#"
        [security]
        allow_full_os_access = ["operator-shell", "deploy-bot"]
    "#;
    let cfg: AlmsConfig = toml::from_str(toml).expect("parse alms.toml fragment");
    assert_eq!(
        cfg.security.allow_full_os_access,
        vec!["operator-shell".to_string(), "deploy-bot".to_string()],
    );
    // The remaining sections still default cleanly so a TOML that only
    // contains `[security]` doesn't fail validation.
    let mut cfg = cfg;
    // Mock provider so `validate()` doesn't trip on the missing API key.
    cfg.llm.mock = true;
    cfg.llm.ensure_builtin_providers();
    cfg.validate()
        .expect("validate should accept valid security config");
}

/// Empty entries in `allow_full_os_access` fail `validate()` rather than
/// silently disabling the entry. A blank string would never match
/// anything (the matcher shortcircuits) but it signals an operator
/// error in the config file, so we surface it loudly.
#[test]
fn security_config_rejects_empty_entries() {
    let mut cfg = AlmsConfig::default();
    cfg.llm.mock = true;
    cfg.security.allow_full_os_access = vec!["valid".into(), "".into()];
    let err = cfg
        .validate()
        .expect_err("validate must reject empty allow_full_os_access entries");
    let msg = err.to_string();
    assert!(
        msg.contains("allow_full_os_access[1]"),
        "error must point at the bad index: got `{msg}`"
    );

    // Whitespace-only is also invalid — same operator-error class.
    cfg.security.allow_full_os_access = vec!["   ".into()];
    cfg.validate()
        .expect_err("whitespace-only entry must also fail validation");
}

/// `[security]` is NOT in `apply_env_overrides` — the threat model says
/// the knob is config-file-only, so an `ALMS_*` env var must not be a
/// PATCH-mutation back door. Pin the absence with a sanity check.
#[test]
fn security_config_has_no_env_var_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_SECURITY_ALLOW_FULL_OS_ACCESS", "alice,bob");

    let mut cfg = AlmsConfig::default();
    cfg.apply_env_overrides();
    assert!(
        cfg.security.allow_full_os_access.is_empty(),
        "apply_env_overrides must not interpret env vars for security knobs — \
         the threat model demands config-file-only mutability"
    );
}

// ───────────────────────────────────────────────────────────────────────
// #869: context strategy redesign — threshold-based compact knobs +
// deprecation aliases for `recent_window` / `summary_interval` /
// `strategy = "sliding-summary"`.
// ───────────────────────────────────────────────────────────────────────

/// Old `alms.toml` files set `recent_window`. The deserialiser must accept
/// the field on the wire (so the daemon doesn't fail to start) but drop
/// the value — the struct no longer has a place to store it. The one-time
/// boot WARN is fired by `warn_recent_window_once`; we don't capture
/// tracing output here (the static `OnceLock` doesn't reset between tests
/// in the same binary).
#[test]
fn test_recent_window_in_toml_is_ignored() {
    let toml = r#"
[context]
strategy = "compact"
max_input_tokens = 64000
recent_window = 5
summary_interval = 10
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.context.strategy, "compact");
    assert_eq!(config.context.max_input_tokens, 64000);
    // The legacy fields are accepted on the wire and dropped from the
    // struct. The runtime sees only the new compact_* knobs.
    assert_eq!(
        config.context.compact_trigger_pct, 0.80,
        "compact_trigger_pct should hold the default after a TOML with only \
         legacy fields parses"
    );
    assert_eq!(config.context.compact_retain_pct, 0.40);
}

/// `strategy = "sliding-summary"` is rewritten to `"compact"` by the
/// `ContextConfig::Deserialize` impl so the rest of the system speaks one
/// canonical strategy name. Pin the alias resolution at the deserialise
/// boundary.
#[test]
fn test_sliding_summary_alias_resolves_to_compact() {
    let toml = r#"
[context]
strategy = "sliding-summary"
"#;
    let config: AlmsConfig = toml::from_str(toml).unwrap();
    assert_eq!(
        config.context.strategy, "compact",
        "ContextConfig::Deserialize must rewrite the legacy alias"
    );
}

/// `compact_trigger_pct` out of range is clamped by `normalize_episodic`
/// to the supported `[0.50, 0.95]` band. Verify both directions.
#[test]
fn test_compact_trigger_pct_clamped_to_range() {
    // Above range — clamps to upper bound.
    let mut cfg = ContextConfig {
        compact_trigger_pct: 1.50,
        ..Default::default()
    };
    cfg.normalize_episodic();
    assert_eq!(cfg.compact_trigger_pct, 0.95);

    // Below range — clamps to lower bound. Pair with a small retain so
    // the gap floor doesn't kick in.
    let mut cfg = ContextConfig {
        compact_trigger_pct: 0.10,
        compact_retain_pct: 0.20,
        ..Default::default()
    };
    cfg.normalize_episodic();
    assert_eq!(cfg.compact_trigger_pct, 0.50);

    // NaN — replaced with the 0.80 default.
    let mut cfg = ContextConfig {
        compact_trigger_pct: f32::NAN,
        ..Default::default()
    };
    cfg.normalize_episodic();
    assert_eq!(cfg.compact_trigger_pct, 0.80);
}

/// Retain too close to trigger violates the `retain + 0.10 <= trigger`
/// floor. `normalize_episodic` lowers retain to keep the gap so
/// compaction always measurably reduces context size.
///
/// We pick values that are inside each knob's individual range
/// (`[0.50, 0.95]` for trigger, `[0.20, 0.60]` for retain) so the
/// gap-floor logic — not the per-knob clamps — is what fires.
#[test]
fn test_compact_retain_too_close_to_trigger_clamped() {
    let mut cfg = ContextConfig {
        compact_trigger_pct: 0.55, // in [0.50, 0.95]
        compact_retain_pct: 0.50,  // in [0.20, 0.60]; gap = 0.05 < 0.10
        ..Default::default()
    };
    cfg.normalize_episodic();
    // retain dropped to trigger - 0.10 = 0.45.
    assert!(
        (cfg.compact_retain_pct - 0.45).abs() < 1e-6,
        "expected retain ≈ 0.45 (trigger 0.55 − 0.10 floor), got {}",
        cfg.compact_retain_pct
    );
    assert_eq!(cfg.compact_trigger_pct, 0.55);

    // The default pair (0.80 / 0.40) is well inside the gap floor.
    let mut cfg = ContextConfig::default();
    cfg.normalize_episodic();
    assert_eq!(cfg.compact_trigger_pct, 0.80);
    assert_eq!(cfg.compact_retain_pct, 0.40);
}

/// `validate()` accepts both `"compact"` and `"sliding-summary"` so a
/// hand-edited TOML that bypasses both rewrite paths still passes the
/// hard validation step.
#[test]
fn test_validate_accepts_compact_and_alias() {
    let mut cfg = AlmsConfig::default();
    cfg.context.strategy = "compact".into();
    assert!(cfg.validate().is_ok());

    cfg.context.strategy = "sliding-summary".into();
    assert!(
        cfg.validate().is_ok(),
        "validate must accept the legacy alias as a back-compat affordance"
    );

    cfg.context.strategy = "nope".into();
    assert!(cfg.validate().is_err());
}

/// `ALMS_CONTEXT_STRATEGY=sliding-summary` lands the alias in
/// `ContextConfig::strategy` directly (no Deserialize round-trip).
/// `normalize_episodic` is the second rewrite layer that catches it.
#[test]
fn test_env_strategy_sliding_summary_normalises_to_compact() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = SingleEnvGuard::set("ALMS_CONTEXT_STRATEGY", "sliding-summary");

    let mut cfg = AlmsConfig::default();
    cfg.apply_env_overrides();
    assert_eq!(
        cfg.context.strategy, "sliding-summary",
        "apply_env_overrides leaves the raw value as-is"
    );
    cfg.context.normalize_episodic();
    assert_eq!(
        cfg.context.strategy, "compact",
        "normalize_episodic rewrites the alias for env-var-fed configs"
    );
}
