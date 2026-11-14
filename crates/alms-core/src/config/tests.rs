// SPDX-License-Identifier: Apache-2.0

//! Tests for the config module.

use super::*;

/// `alms.toml.example` says "All values shown are defaults", and it showed
/// `run_summary_mode = "off"` with "(default)" while the compiled default is
/// `llm`. Pin the line to the real default.
#[test]
fn example_config_shows_the_compiled_run_summary_mode() {
    let example = include_str!("../../../../alms.toml.example");
    let shown: Vec<&str> = example
        .lines()
        .filter_map(|l| l.trim().strip_prefix("run_summary_mode = "))
        .collect();
    let default = format!("\"{}\"", ContextConfig::default().run_summary_mode);
    assert_eq!(shown, vec![default.as_str()]);
    assert!(
        !example.contains("disabled (default)"),
        "the example must not call `off` the default"
    );
}

/// What `provider_requires_api_key` reads as "needs a key": the hosted
/// secrets-store slots, and any entry that declares a key source. A local
/// endpoint declared without one, as `docs/config.md` sets up Ollama, needs
/// none; `telegram` is a channel token, not an LLM provider.
#[test]
fn provider_requires_api_key_reads_the_slots_and_the_entry() {
    let bare = ProviderEntry::default();
    for slot in ["openrouter", "openai", "anthropic", "gemini"] {
        let stock = ProviderEntry {
            base_url: builtin_base_url(slot).unwrap().into(),
            ..ProviderEntry::default()
        };
        assert!(provider_requires_api_key(slot, Some(&stock)), "{slot}");
        assert!(provider_requires_api_key(slot, None), "{slot}");
    }
    assert!(!provider_requires_api_key("ollama", Some(&bare)));
    assert!(!provider_requires_api_key("ollama", None));
    assert!(!provider_requires_api_key("telegram", None));

    let with_env = ProviderEntry {
        api_key_env: Some("SOME_VAR".into()),
        ..ProviderEntry::default()
    };
    assert!(provider_requires_api_key("my-proxy", Some(&with_env)));
    let with_inline = ProviderEntry {
        api_key: Some("k".into()),
        ..ProviderEntry::default()
    };
    assert!(provider_requires_api_key("my-proxy", Some(&with_inline)));
}

/// A built-in slot counts as hosted only while it points at its stock URL
/// (trailing slash or not). Re-pointed at a local server it is the
/// operator's endpoint, judged by whether it declares a key.
#[test]
fn a_re_pointed_slot_is_judged_by_its_entry() {
    let mut cfg = LlmConfig::default();
    cfg.ensure_builtin_providers();
    for slot in ["openrouter", "openai", "anthropic", "gemini"] {
        let entry = cfg.providers.get(slot).unwrap();
        assert_eq!(Some(entry.base_url.as_str()), builtin_base_url(slot));
        assert!(provider_requires_api_key(slot, Some(entry)), "{slot}");
        let slashed = ProviderEntry {
            base_url: format!("{}/", entry.base_url),
            ..entry.clone()
        };
        assert!(provider_requires_api_key(slot, Some(&slashed)), "{slot}/");

        let local = ProviderEntry {
            base_url: "http://localhost:1234/v1".into(),
            ..entry.clone()
        };
        assert!(
            !provider_requires_api_key(slot, Some(&local)),
            "{slot} local"
        );
        let local_with_key = ProviderEntry {
            api_key_env: Some("LOCAL_KEY".into()),
            ..local
        };
        assert!(
            provider_requires_api_key(slot, Some(&local_with_key)),
            "{slot} local+key"
        );
    }
}

/// `ALMS_LLM_BASE_URL` writes into the default slot's entry, so it re-points
/// the slot for this rule too.
#[test]
fn alms_llm_base_url_re_points_the_default_slot() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "http://127.0.0.1:1234/v1");
    let _legacy = SingleEnvGuard::remove("LLM_BASE_URL");
    let _provider = SingleEnvGuard::set("ALMS_LLM_PROVIDER", "openai");

    let mut cfg = AlmsConfig::default();
    cfg.llm.ensure_builtin_providers();
    cfg.apply_env_overrides();

    assert!(!provider_requires_api_key(
        "openai",
        cfg.llm.providers.get("openai")
    ));
    assert!(provider_requires_api_key(
        "openrouter",
        cfg.llm.providers.get("openrouter")
    ));
}

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

// Process env and working directory.
//
// Every test here that changes an env var or the working directory, or reads
// one another test changes, holds `env_lock()` for its whole body: one lock
// per test binary, shared with every other module's tests and recovered
// rather than propagated if a holder panicked. See
// `alms_test_support::process_env` for why a single lock replaced the
// `ENV_LOCK` / `BUDGET_ENV_LOCK` pair and their disjoint-var-set contract.
use alms_test_support::process_env::{
    CurrentDirGuard, EnvLock, VarGuard as SingleEnvGuard, lock as env_lock,
};

const LLM_ENV_VARS: [&str; 4] = [
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ALMS_LLM_PROVIDER",
];

/// Takes the env lock and clears `LLM_ENV_VARS` apart from `overrides`,
/// putting every one of them back on drop.
struct EnvGuard {
    // Restored newest first (see `Drop`), then the lock is released.
    vars: Vec<SingleEnvGuard>,
    _lock: EnvLock,
}

impl EnvGuard {
    fn set(overrides: &[(&'static str, Option<&str>)]) -> Self {
        let lock = env_lock();
        let mut vars: Vec<SingleEnvGuard> =
            LLM_ENV_VARS.iter().map(SingleEnvGuard::remove).collect();
        for (name, value) in overrides {
            vars.push(match value {
                Some(v) => SingleEnvGuard::set(name, v),
                None => SingleEnvGuard::remove(name),
            });
        }
        Self { vars, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Newest first: a var cleared and then overridden must end on the
        // value it had before the clear.
        while let Some(var) = self.vars.pop() {
            drop(var);
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

#[test]
fn test_token_budget_default_config_passes() {
    // Default config: provider=openrouter, model=z-ai/glm-5.2.
    // The table doesn't know that pair → validator must skip → ok.
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::remove(ALMS_LLM_BUDGET_VALIDATION);
    assert_eq!(ValidationMode::from_env(), ValidationMode::Strict);
}

#[test]
fn test_validation_mode_from_env_warn() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "warn");
    assert_eq!(ValidationMode::from_env(), ValidationMode::Warn);
}

#[test]
fn test_validation_mode_from_env_warn_case_insensitive() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set(ALMS_LLM_BUDGET_VALIDATION, "WARN");
    assert_eq!(ValidationMode::from_env(), ValidationMode::Warn);
}

#[test]
fn test_validation_mode_from_env_unknown_falls_back_to_strict() {
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _ws_guard = SingleEnvGuard::set("ALMS_WORKSPACE_DIR", "/custom/workspace");

    let config = ServerConfig::default();
    assert_eq!(config.agents_dir(), PathBuf::from("/custom/workspace"));
}

#[test]
fn test_project_root_precedence_explicit_field_wins() {
    // Precedence: `self.project_root` (CLI) > env > current_dir.
    // The CLI flag writes the absolute path into the field directly,
    // so a non-empty field beats the env var.
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LLM_STREAM_CHUNK_TIMEOUT", "90");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.stream_chunk_timeout_secs, 90);
}

#[test]
fn test_env_override_alms_llm_model() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LLM_MODEL", "claude-sonnet-4-6");
    // Ensure legacy var is not set
    let _legacy_guard = SingleEnvGuard::remove("DEFAULT_MODEL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.model, "claude-sonnet-4-6");
}

#[test]
fn test_env_override_legacy_default_model_still_works() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("DEFAULT_MODEL", "gpt-4o");
    // Ensure new var is not set so legacy is used
    let _new_guard = SingleEnvGuard::remove("ALMS_LLM_MODEL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.model, "gpt-4o");
}

#[test]
fn test_env_override_alms_llm_model_takes_precedence_over_legacy() {
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LLM_BASE_URL", "https://custom.api.com/v1");
    let _legacy_guard = SingleEnvGuard::remove("LLM_BASE_URL");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.llm.base_url, "https://custom.api.com/v1");
}

#[test]
fn test_env_override_legacy_llm_base_url_still_works() {
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LOG_DIR", "/tmp/alms-logs");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.log_dir.as_deref(), Some("/tmp/alms-logs"));
}

#[test]
fn test_env_override_log_file_level() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LOG_FILE_LEVEL", "warn");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.file_level, "warn");
}

#[test]
fn test_env_override_log_rotation() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LOG_ROTATION", "hourly");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.logging.rotation, "hourly");
}

#[test]
fn test_env_override_file_enabled_false() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_LOG_FILE_ENABLED", "false");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert!(!config.logging.file_enabled);
}

#[test]
fn test_env_override_file_enabled_true() {
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
    // Clear data dir env to use the relative default "./.alms".
    let _data_guard = SingleEnvGuard::remove("ALMS_DATA_DIR");
    // #924: post-fail-fast, `load_or_default()` runs `validate()`. The
    // default config (and env-overridden config in this test) passes
    // validate without needing mock mode — `validate()` only WARNS
    // about a missing API key and does not return Err. Mock mode is
    // irrelevant here.
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");

    // Empty working and home directories, through the fallible loader: the
    // exiting `load_or_default()` with the developer's real HOME would end
    // the whole test binary on a broken `~/.config/alms/config.toml`.
    let cwd = tempfile::tempdir().expect("tempdir creates");
    let home = tempfile::tempdir().expect("tempdir creates");
    let config = load_from(cwd.path(), home.path()).expect("defaults load");
    let data_path = std::path::Path::new(&config.server.data_dir);
    assert!(
        data_path.is_absolute(),
        "data_dir should be absolute after load_or_default_fallible(), got: {}",
        config.server.data_dir
    );
}

// ---- Episodic memory config tests ----

#[test]
fn test_context_config_defaults_episodic() {
    let config = ContextConfig::default();
    // #1191 adopted the alms-test-workspace2 settings as code defaults:
    // the shipped baseline is now an explicit SYMMETRIC summary pair —
    // `(openrouter, google/gemma-4-31b-it)` — so a fresh boot summarises
    // with a small non-reasoning model instead of the agent's (possibly
    // heavyweight) chat model. The pair-only invariant from #872/#877
    // still holds: both fields are set together, which the validator
    // accepts; the asymmetric one-of-two shape remains a load-time error.
    assert_eq!(
        config.summary_model.as_deref(),
        Some("google/gemma-4-31b-it"),
        "summary_model defaults to the cheap dedicated summary model (#1191)"
    );
    assert_eq!(
        config.summary_provider.as_deref(),
        Some("openrouter"),
        "summary_provider pairs with summary_model — both default to Some \
         so the pair-only invariant holds out of the box (#1191)"
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_RUN_SUMMARY_MODE", "llm");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.run_summary_mode, RunSummaryMode::Llm);
}

#[test]
fn test_env_override_run_summary_budget() {
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_RUN_SUMMARY_BUDGET", "5000");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.run_summary_budget, 5000);
}

#[test]
fn test_env_override_run_summary_budget_invalid_ignored() {
    let _lock = env_lock();
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
    let _lock = env_lock();
    let _guard = SingleEnvGuard::set("ALMS_SUMMARY_MAX_TOKENS", "1500");

    let mut config = AlmsConfig::default();
    config.apply_env_overrides();
    assert_eq!(config.context.summary_max_tokens, 1500);
}

#[test]
fn test_env_override_summary_max_tokens_invalid_ignored() {
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _unset = SingleEnvGuard::remove("DEFINITELY_UNSET_ENV_VAR_765");
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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
fn test_validation_accepts_default_summary_pair() {
    // #1191: the shipped baseline is now an explicit symmetric pair —
    // `(openrouter, google/gemma-4-31b-it)`. A fresh default config must
    // validate OK so the daemon starts out of the box.
    let config = AlmsConfig::default();
    assert_eq!(
        config.context.summary_provider.as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        config.context.summary_model.as_deref(),
        Some("google/gemma-4-31b-it")
    );
    assert!(
        config.validate().is_ok(),
        "the default symmetric summary pair must pass the pair-only validator"
    );
}

/// PR #1194: `is_compiled_default_summary_pair` distinguishes the stock
/// #1191 default pair (routine — logged at `debug!`) from an
/// operator-configured pair (opt-in — logged at `info!`).
#[test]
fn test_is_compiled_default_summary_pair() {
    let d = ContextConfig::default();
    assert!(ContextConfig::is_compiled_default_summary_pair(
        d.summary_provider.as_deref(),
        d.summary_model.as_deref(),
    ));
    // Any deviation — different provider, different model, or a cleared
    // pair — is an operator choice, not the compiled default.
    assert!(!ContextConfig::is_compiled_default_summary_pair(
        Some("anthropic"),
        d.summary_model.as_deref(),
    ));
    assert!(!ContextConfig::is_compiled_default_summary_pair(
        d.summary_provider.as_deref(),
        Some("minimax/minimax-m2.7"),
    ));
    assert!(!ContextConfig::is_compiled_default_summary_pair(None, None));
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

/// #1191 pair-aware default fill: a `[context]` block that does not
/// mention the summary pair at all inherits the default pair — absence
/// means "use the default", same as every other `[context]` knob.
#[test]
fn test_context_toml_without_summary_pair_inherits_default_pair() {
    let toml_str = r#"
[context]
strategy = "truncate"
max_input_tokens = 64000
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    assert_eq!(cfg.context.summary_provider.as_deref(), Some("openrouter"));
    assert_eq!(
        cfg.context.summary_model.as_deref(),
        Some("google/gemma-4-31b-it")
    );
}

/// #1191 pair-aware default fill: with a `Some` default pair, a naive
/// per-field `raw.or(default)` would backfill the missing half of an
/// asymmetric TOML and silently legalise the exact half-configured
/// state #877's validator rejects. Pin that a one-field-only `[context]`
/// stays asymmetric through deserialization (the validate() rejection
/// itself is covered by the `test_asymmetric_summary_toml_*` tests).
#[test]
fn test_context_toml_partial_summary_pair_is_not_backfilled() {
    let toml_str = r#"
[context]
summary_provider = "openrouter"
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    assert_eq!(cfg.context.summary_provider.as_deref(), Some("openrouter"));
    assert_eq!(
        cfg.context.summary_model, None,
        "the default summary_model must NOT backfill a partial pair"
    );

    let toml_str = r#"
[context]
summary_model = "minimax/minimax-m2.7"
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    assert_eq!(
        cfg.context.summary_provider, None,
        "the default summary_provider must NOT backfill a partial pair"
    );
    assert_eq!(
        cfg.context.summary_model.as_deref(),
        Some("minimax/minimax-m2.7")
    );
}

/// The TOML surface applies the same empty-means-unset normalisation as
/// `PATCH /settings` (`normalize_summary_field`): a non-empty value is
/// trimmed, so `" openrouter "` is the `openrouter` provider entry and not
/// a key that can never resolve.
#[test]
fn test_context_toml_summary_pair_is_trimmed() {
    let toml_str = r#"
[context]
summary_provider = " openrouter "
summary_model = "	google/gemma-4-31b-it "
"#;
    let cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    assert_eq!(cfg.context.summary_provider.as_deref(), Some("openrouter"));
    assert_eq!(
        cfg.context.summary_model.as_deref(),
        Some("google/gemma-4-31b-it")
    );
}

/// #1191: empty strings are the explicit-clear sentinel (mirroring PATCH
/// /settings). Setting both to `""` opts back into inheriting the
/// agent's resolved (provider, model) for summaries — both land as None
/// and the config validates.
#[test]
fn test_context_toml_empty_pair_clears_to_inherit() {
    let toml_str = r#"
[llm]
provider = "openrouter"

[context]
summary_model = ""
summary_provider = ""
"#;
    let mut cfg: AlmsConfig = toml::from_str(toml_str).expect("TOML parses");
    cfg.llm.ensure_builtin_providers();
    assert_eq!(cfg.context.summary_provider, None);
    assert_eq!(cfg.context.summary_model, None);
    assert!(
        cfg.validate().is_ok(),
        "explicitly cleared pair must validate (inherit-agent-model shape)"
    );
}

// ---- #924: load_or_default fail-fast on validate() failure ----
//
// `AlmsConfig::load_or_default()` is the path the gateway entrypoint
// (`alms gateway`) uses (via the early load in `crates/alms-cli/src/main.rs`). Pre-#924
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
// A config file that exists is always fatal if it can't be used: one that
// fails `validate()`, and one that can't be read or parsed. Falling back to
// defaults on the second kind discarded the whole file, `shell_policy` and
// `[security]` included. Only a missing file means defaults (first run).
//
// Tests use `load_or_default_fallible()` — the inner `Result`-returning
// shape — so they can assert on the error path without aborting the
// test process. The public `load_or_default()` wraps it with
// `std::process::exit(1)` on Err. The tests that change the working
// directory serialise against `env_lock()` because `find_config_file()`
// reads from the process cwd, which is shared across parallel tests.

/// Verify the fail-fast contract directly at the helper boundary:
/// `load_or_default_fallible()` must return `Err(InvalidConfig(_))`
/// when `[context]` is in the partial-pair shape #877 rejects.
///
/// We construct the partial-pair config in a temp directory and chdir
/// into it so `find_config_file()` picks up the test toml. Holding
/// `env_lock()` for the whole test serialises against any other test
/// that touches process cwd or `ALMS_*` env vars.
#[test]
fn test_load_or_default_fail_fast_on_partial_summary_pair() {
    let _lock = env_lock();
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
    // partial-pair toml. The guard changes it back even if this panics.
    let result = {
        let _cwd = CurrentDirGuard::set(tempdir.path());
        AlmsConfig::load_or_default_fallible()
    };

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
    let _lock = env_lock();
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

    let result = {
        let _cwd = CurrentDirGuard::set(tempdir.path());
        AlmsConfig::load_or_default_fallible()
    };

    let cfg = result.expect(
        "load_or_default_fallible must succeed when no config file is \
         present (bootstrapping / first-run case). The fail-fast \
         change for #924 must not regress this path.",
    );
    // Spot-check that the defaults shape is intact (#1191: the default
    // summary pair is the explicit `(openrouter, google/gemma-4-31b-it)`).
    assert_eq!(
        cfg.context.summary_provider.as_deref(),
        Some("openrouter"),
        "default config ships the symmetric summary pair"
    );
    assert_eq!(
        cfg.context.summary_model.as_deref(),
        Some("google/gemma-4-31b-it"),
        "default config ships the symmetric summary pair"
    );
    assert!(
        std::path::Path::new(&cfg.server.data_dir).is_absolute(),
        "data_dir must still be resolved to an absolute path"
    );
}

/// Runs `load_or_default_fallible` with `cwd` as the working directory and
/// `home` as the home directory.
fn load_from(cwd: &std::path::Path, home: &std::path::Path) -> AlmsResult<AlmsConfig> {
    let _home_guard = SingleEnvGuard::set("HOME", home.to_str().unwrap());
    let _userprofile_guard = SingleEnvGuard::set("USERPROFILE", home.to_str().unwrap());
    let _cwd = CurrentDirGuard::set(cwd);
    AlmsConfig::load_or_default_fallible()
}

/// An `alms.toml` that won't parse must not be replaced by defaults: that would
/// silently drop every setting in it, including `shell_policy`.
#[test]
fn test_load_or_default_refuses_an_unparseable_config_file() {
    let _lock = env_lock();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let home = tempfile::tempdir().expect("tempdir creates");
    let cwd = tempfile::tempdir().expect("tempdir creates");
    std::fs::write(cwd.path().join("alms.toml"), "not valid toml { } [unclosed")
        .expect("toml write");

    let err = load_from(cwd.path(), home.path()).expect_err("an unparseable file must be fatal");

    assert!(
        matches!(err, AlmsError::InvalidConfig(_)),
        "expected InvalidConfig, got: {err:?}"
    );
    let msg = err.to_string();
    // The full path, so an operator who doesn't know the service's working
    // directory still finds the file.
    let full = std::path::Path::new(cwd.path().file_name().unwrap()).join("alms.toml");
    assert!(
        msg.contains(&*full.to_string_lossy()),
        "must name the file by its full path: {msg}"
    );
    assert!(msg.contains("TOML parse error"), "must say why: {msg}");
}

/// A single bad value fails the whole parse, the same way a syntax error does.
#[test]
fn test_load_or_default_refuses_a_bad_value_in_a_config_file() {
    let _lock = env_lock();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let home = tempfile::tempdir().expect("tempdir creates");
    let cwd = tempfile::tempdir().expect("tempdir creates");
    std::fs::write(
        cwd.path().join("alms.toml"),
        "[llm.providers.x]\nkind = \"not-a-kind\"\nbase_url = \"http://127.0.0.1:9/v1\"\n",
    )
    .expect("toml write");

    let err = load_from(cwd.path(), home.path()).expect_err("a bad value must be fatal");

    assert!(err.to_string().contains("not-a-kind"), "got: {err}");
}

/// The fallback location gets the same treatment as `./alms.toml`.
#[test]
fn test_load_or_default_refuses_an_unparseable_home_config_file() {
    let _lock = env_lock();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let home = tempfile::tempdir().expect("tempdir creates");
    let cwd = tempfile::tempdir().expect("tempdir creates");
    let dir = home.path().join(".config").join("alms");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("config.toml"), "[llm").expect("toml write");

    let err = load_from(cwd.path(), home.path()).expect_err("an unparseable file must be fatal");

    assert!(err.to_string().contains("config.toml"), "got: {err}");
}

/// A file that exists but can't be read is refused too. A directory stands in
/// for an unreadable file, because permission bits don't stop every test user.
#[test]
fn test_load_or_default_refuses_an_unreadable_config_file() {
    let _lock = env_lock();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let home = tempfile::tempdir().expect("tempdir creates");
    let cwd = tempfile::tempdir().expect("tempdir creates");
    std::fs::create_dir(cwd.path().join("alms.toml")).expect("mkdir");

    let err = load_from(cwd.path(), home.path()).expect_err("an unreadable file must be fatal");

    assert!(err.to_string().contains("cannot read"), "got: {err}");
}

/// A dangling `alms.toml` symlink is a file that exists but can't be read, not
/// a missing one. `Path::exists()` follows the link, so it used to count as
/// absent and the load fell through to defaults (or the home config).
#[cfg(unix)]
#[test]
fn test_load_or_default_refuses_a_dangling_config_symlink() {
    let _lock = env_lock();
    let _provider_guard = SingleEnvGuard::remove("ALMS_LLM_PROVIDER");
    let _mock_guard = SingleEnvGuard::remove("ALMS_LLM_MOCK");
    let home = tempfile::tempdir().expect("tempdir creates");
    let cwd = tempfile::tempdir().expect("tempdir creates");
    std::os::unix::fs::symlink(cwd.path().join("gone.toml"), cwd.path().join("alms.toml"))
        .expect("symlink");

    let err = load_from(cwd.path(), home.path()).expect_err("a dangling link must be fatal");

    assert!(err.to_string().contains("alms.toml"), "got: {err}");
}

/// Now that a file that won't parse stops ALMS from starting, the example
/// operators copy has to parse and validate.
#[test]
fn test_alms_toml_example_loads() {
    let mut config: AlmsConfig = toml::from_str(include_str!("../../../../alms.toml.example"))
        .expect("alms.toml.example must parse");
    config.llm.ensure_builtin_providers();
    config.validate().expect("alms.toml.example must validate");
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

/// `is_full_os_access_agent` matches the whole registry name, never a
/// prefix or a substring.
#[test]
fn security_config_match_is_whole_name_only() {
    let security = SecurityConfig {
        allow_full_os_access: vec!["alice".into(), "bob".into()],
    };
    assert!(security.is_full_os_access_agent("alice"));
    assert!(security.is_full_os_access_agent("bob"));
    assert!(!security.is_full_os_access_agent("carol"));
    // No prefix / substring matches.
    assert!(!security.is_full_os_access_agent("ali"));
    assert!(!security.is_full_os_access_agent("alicebot"));
    // Empty input never matches even when the list is non-empty —
    // unnamed/ephemeral agents cannot be on the list by construction.
    assert!(!security.is_full_os_access_agent(""));
}

/// #2: the list is matched case-insensitively, because agent names are
/// unique case-insensitively.
///
/// This does not widen the grant — a case-folded entry still names at most
/// one agent. It closes the near-miss where an operator writes `atlas` in
/// `alms.toml`, creates the agent as `Atlas`, and silently gets no grant at
/// all.
#[test]
fn security_config_match_is_case_insensitive() {
    let security = SecurityConfig {
        allow_full_os_access: vec!["Atlas".into()],
    };
    for spelling in ["Atlas", "atlas", "ATLAS", "aTlAs"] {
        assert!(
            security.is_full_os_access_agent(spelling),
            "{spelling} must match the 'Atlas' entry"
        );
    }
    // Still a whole-name match, not a case-insensitive substring one.
    assert!(!security.is_full_os_access_agent("ATL"));
    assert!(!security.is_full_os_access_agent("AtlasBot"));
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
    let _lock = env_lock();
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
    let _lock = env_lock();
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

fn deprecated_env_warnings(config: &AlmsConfig) -> alms_test_support::CapturedEvents {
    alms_test_support::capture_events(tracing::Level::WARN, || {
        config.warn_deprecated_secret_env_vars();
    })
}

fn config_with_entry_naming(entry_name: &str, var: &str) -> AlmsConfig {
    let mut config = AlmsConfig::default();
    config.llm.providers.insert(
        entry_name.to_string(),
        ProviderEntry {
            api_key_env: Some(var.to_string()),
            ..Default::default()
        },
    );
    config
}

/// `resolve_api_key` reads a var named in `api_key_env`, so "IGNORED" and
/// "no longer used" would both be false for it.
#[test]
fn a_var_named_by_api_key_env_gets_no_deprecation_warning() {
    let _guard = EnvGuard::set(&[("OPENROUTER_API_KEY", Some("sk-named"))]);
    let config = config_with_entry_naming("openrouter", "OPENROUTER_API_KEY");
    assert_eq!(
        config.llm.providers["openrouter"]
            .resolve_api_key()
            .as_deref(),
        Some("sk-named"),
        "the named var must be the key in use"
    );

    let captured = deprecated_env_warnings(&config);

    assert!(
        !captured
            .iter()
            .any(|e| e.field("env_var") == Some("OPENROUTER_API_KEY")),
        "a named var is in use and must not be reported as ignored; got {captured}"
    );
    // Other listed vars can be set in the environment running this test, so
    // the closing summary is only required to be absent when nothing else warned.
    if !captured.iter().any(|e| e.has_field("env_var")) {
        assert!(captured.is_empty(), "got {captured}");
    }
}

#[test]
fn a_bare_export_still_gets_the_deprecation_warning() {
    let _guard = EnvGuard::set(&[("OPENROUTER_API_KEY", Some("sk-bare"))]);
    let config = AlmsConfig::default();

    let captured = deprecated_env_warnings(&config);

    let warning = captured
        .iter()
        .find(|e| e.field("env_var") == Some("OPENROUTER_API_KEY"))
        .unwrap_or_else(|| panic!("a bare export must warn; got {captured}"));
    assert!(warning.message.contains("IGNORED"), "got {captured}");
}

/// With both kinds set, the summary still fires for the bare var and must not
/// claim that API key env vars in general are unused.
#[test]
fn a_named_var_beside_a_bare_export_only_warns_about_the_bare_one() {
    let _guard = EnvGuard::set(&[
        ("OPENROUTER_API_KEY", Some("sk-named")),
        ("ANTHROPIC_API_KEY", Some("sk-bare")),
    ]);
    // Any entry counts, not just the one for the var's usual provider.
    let config = config_with_entry_naming("my-proxy", "OPENROUTER_API_KEY");

    let captured = deprecated_env_warnings(&config);

    assert!(
        captured
            .iter()
            .any(|e| e.field("env_var") == Some("ANTHROPIC_API_KEY")),
        "got {captured}"
    );
    assert!(
        !captured
            .iter()
            .any(|e| e.field("env_var") == Some("OPENROUTER_API_KEY")),
        "got {captured}"
    );
    let summary = captured
        .iter()
        .find(|e| !e.has_field("env_var"))
        .unwrap_or_else(|| panic!("the summary must still fire; got {captured}"));
    assert!(summary.message.contains("api_key_env"), "got {captured}");
    assert!(
        summary.message.contains("TELEGRAM_BOT_TOKEN"),
        "got {captured}"
    );
    assert!(
        !summary.message.contains("no longer used"),
        "got {captured}"
    );
}

/// A test that panics while holding the env lock must not take the rest of
/// this binary with it. Before the lock recovered from poisoning, this one
/// failure showed up as a different set of ten to fifty unrelated
/// `PoisonError` failures on each run. `#[should_panic]` keeps it green
/// while it still poisons the mutex on every run of the suite.
#[test]
#[should_panic(expected = "deliberate panic while holding the env lock")]
fn a_test_that_panics_holding_the_env_lock_fails_alone() {
    let _lock = env_lock();
    let _model = SingleEnvGuard::set("ALMS_LLM_MODEL", "set-by-a-panicking-test");
    let _cwd = CurrentDirGuard::set(std::env::temp_dir());
    panic!("deliberate panic while holding the env lock");
}
