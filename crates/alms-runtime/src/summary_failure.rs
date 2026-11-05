// SPDX-License-Identifier: Apache-2.0

//! Why a conversation summary failed, in the words the user reads.
//!
//! Both summary paths — post-run episodic summaries (`episodic.rs`) and
//! compaction (`agent/context.rs`) — keep the run going when the summary call
//! fails. That is right for the run and silent for the user: the agent carries
//! on with a heuristic line or a truncated tail, and the only trace is a log
//! line. A [`SummaryFailure`] is what those paths hand the gateway instead, so
//! it can tell the user in the session they are looking at.
//!
//! The wording is decided here rather than in the UI so it is built from the
//! client that made the call: the provider named is the one actually
//! configured, and a key is only offered as the fix when the provider's answer
//! was about a key. The provider's response body is never quoted, because it
//! can echo the prompt, and the prompt here is the user's conversation.

use crate::LlmClient;
use alms_core::AlmsError;
use serde::Serialize;

/// What kind of failure a summary call hit. Serialized on the `summary_status`
/// SSE event; `frontend/contracts.ts` holds the same list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SummaryFailureCode {
    /// The provider refused the call (401/403) and no key was sent.
    MissingKey,
    /// The provider refused the call (401/403) and a key was sent.
    RejectedKey,
    /// The call never got an HTTP answer: connection refused, DNS, timeout.
    Unreachable,
    /// The provider refused the request itself, typically an unknown model.
    RejectedRequest,
    /// The provider answered 429.
    RateLimited,
    /// The provider answered 5xx.
    ProviderError,
    /// The call succeeded, but what came back cannot be used as a summary.
    UnusableOutput,
}

/// A failed summary call, ready to show the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SummaryFailure {
    pub code: SummaryFailureCode,
    /// The configured provider name the call went to.
    pub provider: String,
    /// The model requested. Empty when none is configured for the provider.
    pub model: String,
    /// Plain wording for the user, naming the provider and the way out.
    pub message: String,
}

/// The fix that always applies, whatever went wrong: use a different model.
const CHANGE_MODEL: &str = "change the summary model in Settings (Summary section, or the Agents \
                            panel if this agent overrides it; unset it to use the agent's own \
                            model)";

/// The same fix for a key failure. A different model on the same provider
/// would need the same key, so it has to be a different provider; and
/// `PATCH /settings` refuses a provider that needs a key it does not have.
const CHANGE_PROVIDER: &str = "switch summaries to a provider that has a key or needs none, in \
                               Settings (Summary section, or the Agents panel if this agent \
                               overrides it; unset both fields to use the agent's own model)";

/// Display name and article for a provider with a key row in Settings.
///
/// `None` for anything else: a `[llm.providers.<name>]` entry takes its key
/// from `alms.toml`, and `PUT /auth/keys` rejects its name (the same split as
/// `llm_auth_error_hint` in `alms-core`).
fn settings_key_provider(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "openrouter" => Some(("OpenRouter", "an")),
        "openai" => Some(("OpenAI", "an")),
        "anthropic" => Some(("Anthropic", "an")),
        "gemini" => Some(("Gemini", "a")),
        _ => None,
    }
}

impl SummaryFailure {
    /// Classify the error a summary call returned.
    ///
    /// Reads `client` for the provider and for whether a key was sent, so call
    /// it with the client that made the call.
    pub fn from_error(err: &AlmsError, client: &LlmClient, model: &str) -> Self {
        let code = match err {
            AlmsError::LlmApiError {
                status: 401 | 403, ..
            } if !client.has_api_key() => SummaryFailureCode::MissingKey,
            AlmsError::LlmApiError {
                status: 401 | 403, ..
            } => SummaryFailureCode::RejectedKey,
            AlmsError::LlmApiError { status: 429, .. } => SummaryFailureCode::RateLimited,
            AlmsError::LlmApiError {
                status: 500..=599, ..
            } => SummaryFailureCode::ProviderError,
            AlmsError::LlmApiError { .. } => SummaryFailureCode::RejectedRequest,
            AlmsError::Runtime(msg) if msg.starts_with("HTTP request failed") => {
                SummaryFailureCode::Unreachable
            }
            _ => SummaryFailureCode::UnusableOutput,
        };
        let status = match err {
            AlmsError::LlmApiError { status, .. } => Some(*status),
            _ => None,
        };
        Self::new(code, client.provider(), model, status)
    }

    /// The provider needs a key and the client has none, so the call was not
    /// made: sending it would put the conversation on the wire to be refused.
    pub fn missing_key(client: &LlmClient, model: &str) -> Self {
        Self::new(
            SummaryFailureCode::MissingKey,
            client.provider(),
            model,
            None,
        )
    }

    /// The call succeeded but returned nothing usable as a summary: no text,
    /// or text `episodic::screen_summary_output` refused.
    pub fn unusable_output(client: &LlmClient, model: &str) -> Self {
        Self::new(
            SummaryFailureCode::UnusableOutput,
            client.provider(),
            model,
            None,
        )
    }

    fn new(code: SummaryFailureCode, provider: &str, model: &str, status: Option<u16>) -> Self {
        Self {
            code,
            provider: provider.to_string(),
            model: model.to_string(),
            message: message(code, provider, model, status),
        }
    }
}

fn message(code: SummaryFailureCode, provider: &str, model: &str, status: Option<u16>) -> String {
    use SummaryFailureCode as C;

    let keyed = settings_key_provider(provider);
    let name = keyed.map_or(provider, |(display, _)| display);
    let http = status.map_or_else(String::new, |s| format!(" (HTTP {s})"));
    let model_name = if model.is_empty() {
        "no summary model set".to_string()
    } else {
        format!("summary model: {model}")
    };
    // The second way out on a key failure, which depends on where the key lives.
    let add_key = match keyed {
        Some((display, article)) => format!("add {article} {display} key in Settings (API Keys)"),
        None => format!(
            "add the key under [llm.providers.{provider}] (api_key_env or api_key) in alms.toml \
             and restart the gateway"
        ),
    };
    let replace_key = match keyed {
        Some((display, _)) => format!("replace the {display} key in Settings (API Keys)"),
        None => format!(
            "replace the key under [llm.providers.{provider}] (api_key_env or api_key) in \
             alms.toml and restart the gateway"
        ),
    };
    let retries_itself =
        format!("Summaries try again after the next run. If this continues, {CHANGE_MODEL}");

    let (what, fix) = match (code, status) {
        (C::MissingKey, _) => (
            format!("no API key is set for {name}"),
            format!("To fix this, {CHANGE_PROVIDER}, or {add_key}"),
        ),
        (C::RejectedKey, _) => (
            format!("{name} rejected the API key{http}"),
            format!("To fix this, {CHANGE_PROVIDER}, or {replace_key}"),
        ),
        (C::Unreachable, _) => (
            format!("could not reach {name}, or it did not respond in time"),
            format!("Check that it is reachable, or {CHANGE_MODEL}"),
        ),
        (C::RejectedRequest, Some(402)) => (
            format!("{name} reported a billing or credit problem{http}"),
            format!("Check the account, or {CHANGE_MODEL}"),
        ),
        (C::RejectedRequest, Some(400 | 404 | 422)) => (
            format!(
                "{name} rejected the request{http}, which usually means it does not recognise \
                 the model"
            ),
            format!("To fix this, {CHANGE_MODEL}"),
        ),
        (C::RejectedRequest, _) => (
            format!("{name} rejected the request{http}"),
            format!("Check the provider, or {CHANGE_MODEL}"),
        ),
        (C::RateLimited, _) => (
            format!("{name} is rate limiting the requests{http}"),
            retries_itself,
        ),
        (C::ProviderError, _) => (
            format!("{name} returned a server error{http}"),
            retries_itself,
        ),
        (C::UnusableOutput, _) => (
            format!("{name} did not return a usable summary"),
            format!("To fix this, {CHANGE_MODEL}"),
        ),
    };
    format!("Conversation summaries failed: {what} ({model_name}). {fix}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::LlmConfig;

    fn client(provider: &str, key: &str) -> LlmClient {
        LlmClient::new(LlmConfig {
            provider: provider.into(),
            api_key: key.into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            ..LlmConfig::default()
        })
        .unwrap()
    }

    fn api_error(provider: &str, status: u16) -> AlmsError {
        AlmsError::llm_api_error(
            provider.to_string(),
            status,
            "echo of the prompt".to_string(),
        )
    }

    const MODEL: &str = "google/gemma-4-31b-it";

    /// The maintainer's case: an agent with no OpenRouter key, on the default
    /// summary pair. Both ways out must be named.
    #[test]
    fn missing_openrouter_key_names_both_ways_out() {
        let f = SummaryFailure::from_error(
            &api_error("openrouter", 401),
            &client("openrouter", ""),
            MODEL,
        );
        assert_eq!(f.code, SummaryFailureCode::MissingKey);
        assert_eq!(f.provider, "openrouter");
        assert_eq!(f.model, MODEL);
        assert!(
            f.message.contains("no API key is set for OpenRouter"),
            "{}",
            f.message
        );
        assert!(
            f.message
                .contains("needs none, in Settings (Summary section"),
            "{}",
            f.message
        );
        assert!(
            f.message
                .contains("add an OpenRouter key in Settings (API Keys)"),
            "{}",
            f.message
        );
    }

    #[test]
    fn a_key_that_was_sent_and_refused_is_a_rejected_key() {
        let f = SummaryFailure::from_error(
            &api_error("openrouter", 403),
            &client("openrouter", "k"),
            MODEL,
        );
        assert_eq!(f.code, SummaryFailureCode::RejectedKey);
        assert!(
            f.message
                .contains("OpenRouter rejected the API key (HTTP 403)"),
            "{}",
            f.message
        );
        assert!(
            f.message.contains("replace the OpenRouter key"),
            "{}",
            f.message
        );
        assert!(
            f.message.contains("switch summaries to a provider"),
            "{}",
            f.message
        );
    }

    /// A custom provider must not be told about OpenRouter, or about a
    /// Settings key row its name would be refused by.
    #[test]
    fn custom_provider_is_pointed_at_its_own_config_entry() {
        let f =
            SummaryFailure::from_error(&api_error("my-proxy", 401), &client("my-proxy", ""), "m");
        assert_eq!(f.code, SummaryFailureCode::MissingKey);
        assert!(
            f.message.contains("no API key is set for my-proxy"),
            "{}",
            f.message
        );
        assert!(
            f.message.contains("[llm.providers.my-proxy]"),
            "{}",
            f.message
        );
        assert!(!f.message.contains("OpenRouter"), "{}", f.message);
        assert!(!f.message.contains("API Keys"), "{}", f.message);
    }

    #[test]
    fn other_named_providers_use_their_own_name_and_article() {
        let f = SummaryFailure::from_error(&api_error("gemini", 401), &client("gemini", ""), "m");
        assert!(f.message.contains("add a Gemini key"), "{}", f.message);
        assert!(!f.message.contains("OpenRouter"), "{}", f.message);
    }

    /// Whatever the failure, if it is not about a key the message must not
    /// claim a key is the fix — even when no key was set.
    #[test]
    fn failures_that_are_not_about_a_key_do_not_offer_one() {
        let c = client("openrouter", "");
        let cases = [
            (
                api_error("openrouter", 404),
                SummaryFailureCode::RejectedRequest,
                "HTTP 404",
            ),
            (
                api_error("openrouter", 400),
                SummaryFailureCode::RejectedRequest,
                "does not recognise",
            ),
            (
                api_error("openrouter", 402),
                SummaryFailureCode::RejectedRequest,
                "billing",
            ),
            (
                api_error("openrouter", 418),
                SummaryFailureCode::RejectedRequest,
                "HTTP 418",
            ),
            (
                api_error("openrouter", 429),
                SummaryFailureCode::RateLimited,
                "rate limiting",
            ),
            (
                api_error("openrouter", 503),
                SummaryFailureCode::ProviderError,
                "server error",
            ),
            (
                AlmsError::Runtime("HTTP request failed: error sending request for url".into()),
                SummaryFailureCode::Unreachable,
                "could not reach OpenRouter",
            ),
            (
                AlmsError::Runtime("LLM response parse failed: x".into()),
                SummaryFailureCode::UnusableOutput,
                "did not return a usable summary",
            ),
        ];
        for (err, code, needle) in cases {
            let f = SummaryFailure::from_error(&err, &c, MODEL);
            assert_eq!(f.code, code, "{err}");
            assert!(f.message.contains(needle), "{}", f.message);
            assert!(
                f.message.contains("change the summary model"),
                "{}",
                f.message
            );
            assert!(!f.message.to_lowercase().contains(" key"), "{}", f.message);
        }
    }

    /// The body can echo the prompt, which is the user's conversation.
    #[test]
    fn the_provider_body_is_never_quoted() {
        for status in [400, 401, 403, 404, 429, 500] {
            let f = SummaryFailure::from_error(
                &api_error("openrouter", status),
                &client("openrouter", "k"),
                MODEL,
            );
            assert!(!f.message.contains("echo of the prompt"), "{}", f.message);
        }
    }

    /// The #871 leak guard clears the model, so "" reaches the wire.
    #[test]
    fn an_empty_model_reads_as_no_model_set() {
        let f = SummaryFailure::from_error(
            &api_error("openrouter", 400),
            &client("openrouter", "k"),
            "",
        );
        assert!(f.message.contains("no summary model set"), "{}", f.message);
        assert!(!f.message.contains("summary model: )"), "{}", f.message);
    }

    #[test]
    fn unusable_output_reads_as_a_sentence() {
        let f = SummaryFailure::unusable_output(&client("openrouter", "k"), MODEL);
        assert_eq!(f.code, SummaryFailureCode::UnusableOutput);
        assert_eq!(
            f.message,
            "Conversation summaries failed: OpenRouter did not return a usable summary (summary \
             model: google/gemma-4-31b-it). To fix this, change the summary model in Settings \
             (Summary section, or the Agents panel if this agent overrides it; unset it to use \
             the agent's own model)."
        );
    }

    /// The two key codes suggest another provider, because another model on
    /// the same provider needs the same key; the others suggest another model.
    #[test]
    fn key_failures_suggest_another_provider() {
        let f = SummaryFailure::missing_key(&client("openrouter", ""), MODEL);
        assert_eq!(f.code, SummaryFailureCode::MissingKey);
        assert!(
            f.message
                .contains("switch summaries to a provider that has a key or needs none"),
            "{}",
            f.message
        );
        assert!(f.message.contains("unset both fields"), "{}", f.message);
        let f = SummaryFailure::unusable_output(&client("openrouter", "k"), MODEL);
        assert!(!f.message.contains("switch summaries"), "{}", f.message);
    }

    #[test]
    fn serializes_the_code_as_the_wire_constant() {
        let f = SummaryFailure::unusable_output(&client("openrouter", "k"), MODEL);
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["code"], "UNUSABLE_OUTPUT");
        assert_eq!(v["provider"], "openrouter");
    }
}
