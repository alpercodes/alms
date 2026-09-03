// SPDX-License-Identifier: Apache-2.0

//! Shared configuration policies used by every mutation surface.

mod resolution;

use alms_core::config::ProviderEntry;
use alms_core::secrets::SecretsStore;
pub(crate) use resolution::{
    ResolveAgentConfigError, ResolveEffectiveModelError, build_resolved_config,
    model_belongs_to_kind, provider_kind_for_name, resolve_agent_config,
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

fn normalized(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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
    let provider = normalized(provider);
    let model = normalized(model);
    match (&provider, &model) {
        (None, None) => {
            return Ok(ValidatedSummaryPair { provider, model });
        }
        (Some(_), None) => {
            return Err(ConfigPolicyError {
                code: "SUMMARY_PROVIDER_REQUIRES_MODEL",
                message: "summary_provider is set but summary_model is empty; set both fields together or clear both".to_string(),
            });
        }
        (None, Some(_)) => {
            return Err(ConfigPolicyError {
                code: "SUMMARY_MODEL_REQUIRES_PROVIDER",
                message: "summary_model is set but summary_provider is empty; set both fields together or clear both".to_string(),
            });
        }
        (Some(_), Some(_)) => {}
    }

    let provider_name = provider.as_deref().expect("validated provider presence");
    let Some(entry) = providers.get(provider_name) else {
        return Err(ConfigPolicyError {
            code: "SUMMARY_PROVIDER_UNKNOWN",
            message: format!(
                "summary_provider '{provider_name}' is not configured under [llm.providers.<name>] in alms.toml"
            ),
        });
    };
    if entry.resolve_api_key().is_none() && secrets.resolve_key(provider_name).is_none() {
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
}
