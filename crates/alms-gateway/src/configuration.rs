// SPDX-License-Identifier: Apache-2.0

//! Shared configuration policies used by every mutation surface.

mod resolution;

use alms_core::config::ProviderEntry;
use alms_core::secrets::SecretsStore;
pub(crate) use resolution::{
    ResolveAgentConfigError, ResolveEffectiveModelError, ResolvedAgentConfig,
    build_resolved_config, model_belongs_to_kind, provider_kind_for_name, resolve_agent_config,
    resolve_effective_provider_and_model,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigPolicyError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl std::fmt::Display for ConfigPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedSummaryPair {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
}

/// Validate and normalize the dedicated summary provider/model pair.
///
/// The two values are one policy unit: both inherit from the primary LLM
/// configuration, or both explicitly select the summary wire namespace.
pub(crate) fn validate_summary_pair(
    provider: Option<&str>,
    model: Option<&str>,
    providers: &BTreeMap<String, ProviderEntry>,
    secrets: &SecretsStore,
) -> Result<ValidatedSummaryPair, ConfigPolicyError> {
    let provider = alms_core::config::normalize_summary_field(provider);
    let model = alms_core::config::normalize_summary_field(model);
    alms_core::config::check_summary_pair(provider.as_deref(), model.as_deref()).map_err(|e| {
        ConfigPolicyError {
            code: e.code(),
            message: e.to_string(),
        }
    })?;
    let Some(provider_name) = provider.as_deref() else {
        // Both unset: inherit the primary LLM configuration.
        return Ok(ValidatedSummaryPair { provider, model });
    };
    let Some(entry) = providers.get(provider_name) else {
        return Err(ConfigPolicyError {
            code: "SUMMARY_PROVIDER_UNKNOWN",
            message: format!(
                "summary_provider '{provider_name}' is not configured under [llm.providers.<name>] in alms.toml"
            ),
        });
    };
    // Only a provider that needs a key has to have one: a local endpoint
    // declared without a key (Ollama, LM Studio) is a valid summary provider.
    // The same rule decides whether the runtime sends the call at all, so
    // this cannot accept a pair whose summaries would never be sent.
    if alms_core::config::provider_requires_api_key(provider_name, Some(entry))
        && entry.resolve_api_key().is_none()
        && secrets.resolve_key(provider_name).is_none()
    {
        return Err(ConfigPolicyError {
            code: "SUMMARY_PROVIDER_MISSING_API_KEY",
            message: format!(
                "summary_provider '{provider_name}' has no resolvable API key; use `alms auth set {provider_name}` or configure the provider entry"
            ),
        });
    }

    Ok(ValidatedSummaryPair { provider, model })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::config::{AuthScheme, ProviderKind, ProviderQuirks};

    fn provider(api_key: Option<&str>) -> ProviderEntry {
        ProviderEntry {
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://example.invalid/v1".to_string(),
            api_key_env: None,
            api_key: api_key.map(str::to_owned),
            model: None,
            auth_scheme: AuthScheme::Bearer,
            quirks: ProviderQuirks::default(),
        }
    }

    #[test]
    fn summary_pair_policy_is_symmetric_and_normalizes() {
        let mut providers = BTreeMap::new();
        providers.insert("openrouter".to_string(), provider(Some("test-key")));
        let secrets = SecretsStore::empty();

        let pair = validate_summary_pair(
            Some(" openrouter "),
            Some(" model/one "),
            &providers,
            &secrets,
        )
        .unwrap();
        assert_eq!(pair.provider.as_deref(), Some("openrouter"));
        assert_eq!(pair.model.as_deref(), Some("model/one"));
        assert_eq!(
            validate_summary_pair(Some("openrouter"), None, &providers, &secrets)
                .unwrap_err()
                .code,
            "SUMMARY_PROVIDER_REQUIRES_MODEL"
        );
        assert_eq!(
            validate_summary_pair(None, Some("model/one"), &providers, &secrets)
                .unwrap_err()
                .code,
            "SUMMARY_MODEL_REQUIRES_PROVIDER"
        );
    }

    #[test]
    fn summary_pair_policy_checks_provider_and_key() {
        let secrets = SecretsStore::empty();
        let mut providers = BTreeMap::new();
        assert_eq!(
            validate_summary_pair(Some("missing"), Some("model"), &providers, &secrets)
                .unwrap_err()
                .code,
            "SUMMARY_PROVIDER_UNKNOWN"
        );
        providers.insert("openrouter".to_string(), provider(None));
        assert_eq!(
            validate_summary_pair(Some("openrouter"), Some("model"), &providers, &secrets)
                .unwrap_err()
                .code,
            "SUMMARY_PROVIDER_MISSING_API_KEY"
        );
    }

    /// The summary banner's advice must be something Settings accepts: a
    /// local endpoint declared without a key is a valid summary provider,
    /// while one whose entry names a key source still needs that key.
    #[test]
    fn summary_pair_policy_accepts_a_provider_that_needs_no_key() {
        let secrets = SecretsStore::empty();
        let mut providers = BTreeMap::new();
        providers.insert("ollama".to_string(), provider(None));
        let pair = validate_summary_pair(Some("ollama"), Some("llama3.1:8b"), &providers, &secrets)
            .unwrap();
        assert_eq!(pair.provider.as_deref(), Some("ollama"));

        let mut needs_env = provider(None);
        needs_env.api_key_env = Some("ALMS_TEST_UNSET_SUMMARY_KEY_VAR".into());
        providers.insert("hosted".to_string(), needs_env);
        assert_eq!(
            validate_summary_pair(Some("hosted"), Some("m"), &providers, &secrets)
                .unwrap_err()
                .code,
            "SUMMARY_PROVIDER_MISSING_API_KEY"
        );
    }
}
