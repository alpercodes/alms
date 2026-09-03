// SPDX-License-Identifier: Apache-2.0

//! Shared helper for building the `LlmClient` used on the per-run summary path
//! (`[context].summary_provider` / `summary_model`).
//!
//! Two call sites need byte-identical leak-guard semantics:
//!
//! 1. `alms-gateway::runs::lifecycle::run_with_runtime` — the top-level run
//!    path, where the agent's resolved client and the live `SecretsStore` are
//!    both in hand.
//! 2. `alms-coordinator::lib::spawn_subagent_runtime` — the subagent inheritance
//!    path, where the subagent's resolved client is built from the parent's
//!    `LlmClient` plus optional secrets.
//!
//! Both paths must apply the same `#866 + #871` rules:
//!
//! - `summary_provider = None` → return a clone of the agent's `LlmClient`
//!   (pre-#866 behaviour, byte-identical wire shape).
//! - `summary_provider = Some(p)` → re-target the cloned client at `p` via
//!   `with_provider_and_secrets` (or `with_provider` when no secrets are
//!   available, preserving the existing key — this matches the coordinator's
//!   pre-extraction branch).
//! - If `summary_model = Some(m)`, apply it last so the explicit override
//!   wins regardless of any `[llm.providers.<p>].model` field.
//! - **#871 leak guard**: if `summary_model = None` AND `apply_provider`
//!   left `default_model` equal to the agent's pre-switch model (i.e. the
//!   new provider entry has no `model` field), clear `default_model` so
//!   the wire fails fast with a missing-model error rather than leaking the
//!   agent provider's model slug onto the summary provider's wire — the
//!   same shape as #860/#861 on the per-agent path.
//!
//! The PATCH validator in `alms-gateway::settings` rejects the
//! `(provider set + model missing)` shape at write time
//! (`SUMMARY_PROVIDER_REQUIRES_MODEL`); this runtime guard is defense-in-depth
//! for hand-edited TOML and any other path that bypasses PATCH validation.
//!
//! Callers wrap their own contextual log lines (agent_id / task_id) around the
//! call. The helper itself emits a structured `warn!` when the leak guard
//! fires so future drift in the rule shows up in a single place.

use crate::LlmClient;
use tracing::warn;

/// Build the LLM client used for the per-run summary task.
///
/// See the module doc-comment for the full rule set. Returns a fresh
/// `LlmClient`; the caller is responsible for plumbing it via
/// `AgentRuntime::with_summary_llm` when `summary_provider` is `Some`.
///
/// `secrets`:
/// - `Some(s)` — re-target via `with_provider_and_secrets(provider, s)` so the
///   API key is re-resolved from the secrets store (with `[llm.providers.<p>]`
///   `api_key_env` / `api_key` fallback inside `with_provider_and_secrets`).
/// - `None` — re-target via `with_provider(provider)`, preserving the existing
///   key. Currently only the coordinator subagent path can hit this branch
///   (when no `Arc<RwLock<SecretsStore>>` was threaded down).
pub fn build_summary_client(
    llm: &LlmClient,
    summary_provider: Option<&str>,
    summary_model: Option<&str>,
    secrets: Option<&alms_core::secrets::SecretsStore>,
) -> LlmClient {
    let Some(provider) = summary_provider else {
        return llm.clone();
    };

    let agent_default_model = llm.default_model().to_string();
    let mut switched = match secrets {
        Some(s) => llm.clone().with_provider_and_secrets(provider, s),
        None => llm.clone().with_provider(provider),
    };

    if let Some(m) = summary_model {
        switched = switched.with_model(m.to_string());
    } else if switched.default_model() == agent_default_model {
        warn!(
            agent_provider = %llm.provider(),
            summary_provider = %provider,
            stale_model = %agent_default_model,
            "summary_provider switch with no summary_model and no \
             provider-entry model — clearing inherited model so the summary \
             task fails fast instead of sending the agent provider's default \
             model to the summary provider's wire (#871)"
        );
        switched = switched.with_model(String::new());
    }

    switched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::LlmConfig;
    use alms_core::config::{AuthScheme, ProviderEntry, ProviderKind, ProviderQuirks};
    use alms_core::secrets::SecretsStore;

    /// Build an `LlmClient` whose resolved config is on Anthropic with a
    /// sonnet model, plus a `[llm.providers.{anthropic, openrouter}]` map
    /// where each entry's `model` field is configurable. Mirrors the fixture
    /// used by the gateway-side leak-guard tests.
    fn fixtures(
        anthropic_entry_model: Option<&str>,
        openrouter_entry_model: Option<&str>,
    ) -> (LlmClient, SecretsStore) {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            ProviderEntry {
                kind: ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: openrouter_entry_model.map(str::to_string),
                auth_scheme: AuthScheme::Bearer,
                quirks: ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            ProviderEntry {
                kind: ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: anthropic_entry_model.map(str::to_string),
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: ProviderQuirks::default(),
            },
        );
        let cfg = LlmConfig {
            provider: "anthropic".into(),
            api_key: "sk-ant-runtime".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            default_model: "claude-sonnet-4-20250514".into(),
            providers,
            ..LlmConfig::default()
        };
        let llm = LlmClient::new(cfg).unwrap();
        // `SecretsStore::set_key` persists to disk via `save()`, which fails
        // for `SecretsStore::empty()` (empty path). Use `load(...)` against a
        // tempdir-backed path so `set_key` has a real file to write to. The
        // tempdir is leaked so it survives the test scope (mirrors the
        // gateway-side fixture pattern in `lifecycle.rs::summary_test_fixtures`).
        let dir = tempfile::tempdir().unwrap();
        let mut secrets = SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        secrets.set_key("openrouter", "sk-or-runtime").unwrap();
        std::mem::forget(dir);
        (llm, secrets)
    }

    /// Pre-#866 behaviour: `summary_provider = None` returns a clone with the
    /// same provider+model+base_url. This is the byte-identical-to-pre-#866
    /// guarantee.
    #[test]
    fn no_provider_returns_agent_llm_clone() {
        let (llm, secrets) = fixtures(None, None);
        let summary = build_summary_client(&llm, None, None, Some(&secrets));
        assert_eq!(summary.provider(), llm.provider());
        assert_eq!(summary.default_model(), llm.default_model());
    }

    /// #866 happy path: provider AND model both set. Both reach the wire.
    #[test]
    fn provider_and_model_both_apply() {
        let (llm, secrets) = fixtures(None, None);
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("minimax/minimax-m2.7"),
            Some(&secrets),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "minimax/minimax-m2.7");
    }

    /// #866 + #871: provider set, no model, AND the new provider entry has its
    /// own `model` field. `apply_provider` rewrites `default_model` to the
    /// entry's model — the leak guard does NOT clear it because that's the
    /// `(provider, model)` pair the user configured.
    #[test]
    fn provider_only_with_entry_model_uses_entry_model() {
        let (llm, secrets) = fixtures(None, Some("provider-default-model"));
        let summary = build_summary_client(&llm, Some("openrouter"), None, Some(&secrets));
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "provider-default-model");
    }

    /// #871 leak guard (the bug Tim flagged on PR #871): provider set, no
    /// model, AND the new provider entry has NO `model` field. `apply_provider`
    /// leaves `default_model` unchanged, so the cloned client carries the
    /// AGENT's model slug into the new provider's wire. Pre-fix: the slug
    /// reaches openrouter and produces a 404 (the same shape #861 prevented on
    /// the agent path). Post-fix: the leak guard clears `default_model`.
    #[test]
    fn provider_only_no_entry_model_clears_to_fail_fast() {
        let (llm, secrets) = fixtures(None, None);
        let agent_model = llm.default_model().to_string();
        let summary = build_summary_client(&llm, Some("openrouter"), None, Some(&secrets));
        assert_eq!(summary.provider(), "openrouter");
        assert_ne!(summary.default_model(), agent_model);
        assert_eq!(summary.default_model(), "");
    }

    /// #866 + #871: provider set, model set, AND the new provider entry also
    /// has its own `model` field. The explicit `summary_model` override must
    /// win — same property as the per-agent #861 test.
    #[test]
    fn explicit_model_wins_over_entry_model() {
        let (llm, secrets) = fixtures(None, Some("provider-default-model"));
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("user-chosen-summary-model"),
            Some(&secrets),
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "user-chosen-summary-model");
    }

    /// `secrets = None` branch: when no `SecretsStore` is threaded down (the
    /// coordinator subagent path's pre-extraction `else` arm), we still
    /// re-target the provider via `with_provider`, preserving the existing
    /// API key. The leak-guard semantics are identical.
    #[test]
    fn no_secrets_uses_with_provider_and_still_applies_leak_guard() {
        let (llm, _secrets) = fixtures(None, None);
        let agent_model = llm.default_model().to_string();
        let summary = build_summary_client(&llm, Some("openrouter"), None, None);
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(
            summary.default_model(),
            "",
            "leak guard must fire on the no-secrets branch too"
        );
        assert_ne!(summary.default_model(), agent_model);
    }

    /// `secrets = None` happy path: with explicit `summary_model`, the model
    /// reaches the wire and the leak guard does not fire.
    #[test]
    fn no_secrets_with_explicit_model_applies_model() {
        let (llm, _secrets) = fixtures(None, None);
        let summary = build_summary_client(
            &llm,
            Some("openrouter"),
            Some("explicit-summary-model"),
            None,
        );
        assert_eq!(summary.provider(), "openrouter");
        assert_eq!(summary.default_model(), "explicit-summary-model");
    }
}
