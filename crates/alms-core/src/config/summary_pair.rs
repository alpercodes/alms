// SPDX-License-Identifier: Apache-2.0

//! The summary provider/model pair rule.
//!
//! `summary_provider` and `summary_model` are one policy unit: both set
//! (explicitly select the summary wire namespace) or both unset (inherit
//! the primary LLM configuration). Exactly one set is the broken shape — a
//! provider with no model has nothing to send, and a model with no provider
//! would fall through to the agent's primary provider, whose namespace may
//! not contain it.
//!
//! Every surface that accepts the pair — `[context]` in `alms.toml` at
//! load, `PATCH /settings`, `POST` / `PUT /agents`, and `alms agent create`
//! / `config` — applies this one rule and then adds only what it alone can
//! check (the gateway verifies the provider exists and has a key; the CLI
//! cannot, and leaves that to the daemon).
//!
//! Two normalisation policies sit in front of the rule, and they are kept
//! distinct on purpose. The surfaces where an empty value means *unset* —
//! `alms.toml` (the `""` clear sentinel), `PATCH /settings` and `POST
//! /agents` — share [`normalize_summary_field`], so a trimmed non-empty
//! value and `None` for empty are one behaviour, not three copies of it.
//! The surfaces with an explicit clear operation — `PUT /agents`'s
//! `clear_*` sentinels and the CLI's `--clear-*` flags — reject an empty
//! value instead, because there it can only be a mistake.

/// Why a summary pair was rejected.
///
/// [`code`](Self::code) is the stable error code shared by the HTTP and CLI
/// surfaces; `Display` is the operator-facing sentence, which each surface
/// may extend with its own remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SummaryPairError {
    /// `summary_provider` is set but `summary_model` is not.
    #[error(
        "summary_provider is set but summary_model is empty; set both fields together or clear both"
    )]
    ProviderRequiresModel,
    /// `summary_model` is set but `summary_provider` is not.
    #[error(
        "summary_model is set but summary_provider is empty; set both fields together or clear both"
    )]
    ModelRequiresProvider,
}

impl SummaryPairError {
    /// The stable error code for this rejection.
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProviderRequiresModel => "SUMMARY_PROVIDER_REQUIRES_MODEL",
            Self::ModelRequiresProvider => "SUMMARY_MODEL_REQUIRES_PROVIDER",
        }
    }
}

/// Trim one half of the pair and map an empty or whitespace-only value to
/// `None` (unset).
///
/// This is the empty-means-unset policy, shared by every surface that has
/// no explicit clear operation so that `PATCH /settings`, `POST /agents`
/// and `alms.toml` cannot drift into accepting different inputs for the
/// same field.
pub fn normalize_summary_field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Apply the pair rule: both set or both unset.
///
/// `None` means unset. Callers normalise first — with
/// [`normalize_summary_field`] where empty means unset, or by rejecting
/// empty values outright where a clear operation exists.
pub fn check_summary_pair(
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), SummaryPairError> {
    match (provider, model) {
        (Some(_), None) => Err(SummaryPairError::ProviderRequiresModel),
        (None, Some(_)) => Err(SummaryPairError::ModelRequiresProvider),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_maps_empty_to_unset() {
        assert_eq!(normalize_summary_field(None), None);
        assert_eq!(normalize_summary_field(Some("")), None);
        assert_eq!(normalize_summary_field(Some("  \t\n")), None);
        assert_eq!(
            normalize_summary_field(Some(" openrouter ")).as_deref(),
            Some("openrouter")
        );
    }

    #[test]
    fn both_or_neither_is_the_rule_and_the_codes_name_the_missing_half() {
        assert_eq!(check_summary_pair(None, None), Ok(()));
        assert_eq!(check_summary_pair(Some("openrouter"), Some("m")), Ok(()));

        let err = check_summary_pair(Some("openrouter"), None).unwrap_err();
        assert_eq!(err, SummaryPairError::ProviderRequiresModel);
        assert_eq!(err.code(), "SUMMARY_PROVIDER_REQUIRES_MODEL");

        let err = check_summary_pair(None, Some("m")).unwrap_err();
        assert_eq!(err, SummaryPairError::ModelRequiresProvider);
        assert_eq!(err.code(), "SUMMARY_MODEL_REQUIRES_PROVIDER");

        // The sentence names the half that is set and the half that is
        // missing, in that order — every surface's tests read it this way.
        assert!(
            SummaryPairError::ProviderRequiresModel
                .to_string()
                .starts_with("summary_provider is set but summary_model is empty")
        );
        assert!(
            SummaryPairError::ModelRequiresProvider
                .to_string()
                .starts_with("summary_model is set but summary_provider is empty")
        );
    }
}
