//! Request-side helpers for the LLM client.
//!
//! Collected here so the pure transforms that `LlmClient::build_request`
//! applies before serialization (auth header attachment, provider quirks,
//! reasoning-model gating) live in one place and can be unit-tested
//! without spinning up an `LlmClient`.

use crate::llm_types::CompletionRequest;
use alms_core::config::AuthScheme;
use reqwest::RequestBuilder;

/// Attach an API key to a request according to the configured auth scheme.
///
/// Returns the builder unmodified when the key is empty — callers will see
/// a 401 from the upstream, which is the right signal. Silently adding an
/// empty header would make missing-key errors harder to diagnose.
pub(crate) fn apply_auth(
    builder: RequestBuilder,
    scheme: &AuthScheme,
    api_key: &str,
) -> RequestBuilder {
    if api_key.is_empty() {
        return builder;
    }
    match scheme {
        AuthScheme::Bearer => builder.header("Authorization", format!("Bearer {api_key}")),
        AuthScheme::Header { name } => builder.header(name.as_str(), api_key),
    }
}

/// Apply [`alms_core::config::ProviderQuirks`] to an OpenAI-format request.
///
/// See the individual `quirks.*` fields for semantics. Called from
/// `build_request` just before serialization; writes through `&mut` so the
/// caller can compose multiple request transforms without cloning for each.
pub(crate) fn apply_quirks(
    request: &mut CompletionRequest,
    quirks: &alms_core::config::ProviderQuirks,
) {
    // Order matters: `drop_empty_content` runs first so it only sees the
    // user's original history. `tool_gap_fill` then deliberately inserts
    // empty-user separators between consecutive tool results — a shape that
    // `drop_empty_content` would have stripped if it ran afterwards. The
    // two quirks compose cleanly only in this order.
    if quirks.drop_empty_content {
        request.messages.retain(|m| {
            let has_content = m.content.as_deref().is_some_and(|c| !c.is_empty());
            let has_tool_calls = m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());
            let is_tool_result = m.role == "tool";
            has_content || has_tool_calls || is_tool_result
        });
    }

    if quirks.tool_gap_fill && request.messages.len() > 1 {
        // Walk from back to front inserting an empty user turn between two
        // consecutive `tool` messages. Back-to-front avoids re-scanning
        // indexes we've already visited.
        let mut i = request.messages.len() - 1;
        while i > 0 {
            if request.messages[i].role == "tool" && request.messages[i - 1].role == "tool" {
                request
                    .messages
                    .insert(i, crate::llm_types::LlmMessage::user(""));
            }
            i -= 1;
        }
    }
}

/// Returns `true` if the given model+base_url combination accepts the
/// OpenAI-compat `reasoning_effort` request field (#768).
///
/// Detection is model-name + base-URL based:
/// - DeepSeek endpoints (base URL contains `deepseek`) always return
///   `false` — `deepseek-reasoner` reasons automatically and rejects
///   the param.
/// - OpenAI reasoning models: the `o1`/`o3`/`o4-mini`/`o5` o-series
///   families and the `gpt-5` family all accept the param. Detection
///   checks for model name prefixes like `o1-`, `o3-`, `o4-mini`,
///   `gpt-5`, case-insensitive and with an optional provider prefix
///   (e.g. OpenRouter uses `openai/o3-mini`, `openai/gpt-5`).
/// - xAI Grok reasoning variants: `grok-*-reasoning`, `grok-3-mini`,
///   `grok-4`. Detected by the `grok-` prefix combined with the
///   `-reasoning` suffix or an explicit reasoning tier name.
/// - Everything else (gpt-4o, claude-sonnet via proxy, non-reasoning
///   Grok, etc.) returns `false` so the field gets stripped.
///
/// The heuristic is intentionally conservative: false negatives (param
/// stripped for a model that would accept it) just disable the feature
/// for that model, while false positives (param sent to a model that
/// rejects it) produce a hard 400. If new reasoning models ship, extend
/// this list rather than widening the matcher.
pub(crate) fn is_openai_reasoning_model(model: &str, base_url: &str) -> bool {
    // DeepSeek strips `reasoning_effort` regardless of model name because
    // even if the user points a DeepSeek base URL at a non-reasoner model,
    // the param is still DeepSeek-incompatible.
    let base_lower = base_url.to_ascii_lowercase();
    if base_lower.contains("deepseek") {
        return false;
    }

    // Strip an optional `<provider>/` prefix (OpenRouter-style) before
    // model-name matching. The model name itself is case-insensitive.
    let model_lower = model.to_ascii_lowercase();
    let name = model_lower
        .rsplit_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(&model_lower);

    // OpenAI o-series / GPT-5 reasoning families.
    if name.starts_with("o1")
        || name.starts_with("o3")
        || name.starts_with("o4")
        || name.starts_with("o5")
        || name.starts_with("gpt-5")
    {
        return true;
    }

    // xAI Grok reasoning variants. The SKUs listed here accept the
    // `reasoning_effort` param per xAI's documentation; other Grok
    // models do not.
    if name.starts_with("grok-")
        && (name.contains("-reasoning")
            || name.starts_with("grok-3-mini")
            || name.starts_with("grok-4"))
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_types::{CompletionRequest, LlmMessage, ToolCall};

    /// Inspect the `Authorization` header a `RequestBuilder` would send
    /// without actually dispatching a request.
    fn header_value(builder: reqwest::RequestBuilder, name: &str) -> Option<String> {
        let req = builder.build().expect("build request");
        req.headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[test]
    fn test_apply_auth_bearer() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let builder = apply_auth(builder, &AuthScheme::Bearer, "secret-key");
        assert_eq!(
            header_value(builder, "authorization"),
            Some("Bearer secret-key".to_string())
        );
    }

    #[test]
    fn test_apply_auth_custom_header() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let scheme = AuthScheme::Header {
            name: "x-api-key".into(),
        };
        let builder = apply_auth(builder, &scheme, "anthropic-key");
        let req = builder.build().unwrap();
        assert_eq!(
            req.headers().get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("anthropic-key")
        );
        // Bearer scheme should NOT have been applied.
        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn test_apply_auth_empty_key_skips_header() {
        let client = reqwest::Client::new();
        let builder = client.post("https://example.com");
        let builder = apply_auth(builder, &AuthScheme::Bearer, "");
        // Empty key -> no header at all (caller will see a 401 upstream).
        assert!(header_value(builder, "authorization").is_none());
    }

    #[test]
    fn test_apply_quirks_drop_empty_content() {
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("hello"),
            LlmMessage {
                role: "assistant".into(),
                content: Some(String::new()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage::assistant("response"),
        ]);
        apply_quirks(&mut req, &quirks);
        // Empty assistant turn is dropped; the two real messages remain.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].content.as_deref(), Some("hello"));
        assert_eq!(req.messages[1].content.as_deref(), Some("response"));
    }

    #[test]
    fn test_apply_quirks_drop_empty_content_keeps_tool_calls() {
        // An assistant message with no content but present tool_calls must
        // NOT be dropped — the tool call IS the payload.
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let tc = ToolCall::new("call_1", "echo", r#"{"text":"hi"}"#);
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("call echo"),
            LlmMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![tc]),
                tool_call_id: None,
            },
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 2);
        assert!(req.messages[1].tool_calls.is_some());
    }

    #[test]
    fn test_apply_quirks_drop_empty_content_keeps_tool_results() {
        // Tool-result messages carry `role == "tool"` and must be kept
        // even though their content might legitimately be empty for
        // no-op tools.
        let quirks = alms_core::config::ProviderQuirks {
            drop_empty_content: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("go"),
            LlmMessage::tool_result("call_1", ""),
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[1].role, "tool");
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_between_consecutive_tools() {
        // Two back-to-back tool-results get split by an empty user turn.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do both"),
            LlmMessage::tool_result("call_1", "result 1"),
            LlmMessage::tool_result("call_2", "result 2"),
        ]);
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), 4);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[1].role, "tool");
        assert_eq!(req.messages[2].role, "user");
        assert_eq!(req.messages[2].content.as_deref(), Some(""));
        assert_eq!(req.messages[3].role, "tool");
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_noop_on_non_adjacent() {
        // A single tool-result isn't followed by another tool-result,
        // so no gap-fill insertion should happen.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do one"),
            LlmMessage::tool_result("call_1", "result 1"),
            LlmMessage::assistant("ok"),
        ]);
        let before = req.messages.len();
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), before);
    }

    #[test]
    fn test_apply_quirks_tool_gap_fill_three_in_a_row() {
        // Three consecutive tool-results get two separators.
        let quirks = alms_core::config::ProviderQuirks {
            tool_gap_fill: true,
            ..Default::default()
        };
        let mut req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::user("do three"),
            LlmMessage::tool_result("c1", "r1"),
            LlmMessage::tool_result("c2", "r2"),
            LlmMessage::tool_result("c3", "r3"),
        ]);
        apply_quirks(&mut req, &quirks);
        // user, tool, USER, tool, USER, tool  = 6 messages
        assert_eq!(req.messages.len(), 6);
        let roles: Vec<&str> = req.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "tool", "user", "tool", "user", "tool"]);
    }

    #[test]
    fn test_apply_quirks_default_is_noop() {
        let quirks = alms_core::config::ProviderQuirks::default();
        let messages = vec![
            LlmMessage::user("hello"),
            LlmMessage::tool_result("call_1", "ok"),
            LlmMessage::tool_result("call_2", "ok"),
        ];
        let mut req = CompletionRequest::new("test").with_messages(messages.clone());
        apply_quirks(&mut req, &quirks);
        assert_eq!(req.messages.len(), messages.len());
    }

    /// `is_openai_reasoning_model` recognises the families listed in
    /// the issue body and rejects everything else.
    #[test]
    fn test_is_openai_reasoning_model_matches_known_families() {
        // OpenAI o-series / GPT-5 — accept.
        let base = "https://api.openai.com/v1";
        for m in &[
            "o1",
            "o1-mini",
            "o1-preview",
            "o3",
            "o3-mini",
            "o4-mini",
            "o5",
            "gpt-5",
            "gpt-5-preview",
            "openai/o3-mini", // OpenRouter-style prefix
            "openai/gpt-5",
            "O3-Mini", // case-insensitive
        ] {
            assert!(
                is_openai_reasoning_model(m, base),
                "'{m}' should be recognised as an OpenAI reasoning model"
            );
        }
        // Non-reasoning OpenAI models.
        for m in &["gpt-4o", "gpt-4", "gpt-3.5-turbo", "openai/gpt-4o"] {
            assert!(
                !is_openai_reasoning_model(m, base),
                "'{m}' should NOT be recognised as a reasoning model"
            );
        }
        // xAI Grok reasoning variants — accept.
        let xai_base = "https://api.x.ai/v1";
        for m in &["grok-3-mini", "grok-3-reasoning", "grok-4"] {
            assert!(
                is_openai_reasoning_model(m, xai_base),
                "'{m}' should be recognised as a Grok reasoning variant"
            );
        }
        // Non-reasoning Grok variants.
        for m in &["grok-2", "grok-vision"] {
            assert!(
                !is_openai_reasoning_model(m, xai_base),
                "'{m}' should NOT be recognised as a Grok reasoning variant"
            );
        }
        // DeepSeek — always false regardless of model name.
        let ds_base = "https://api.deepseek.com/v1";
        for m in &["deepseek-reasoner", "o3-mini", "gpt-5"] {
            assert!(
                !is_openai_reasoning_model(m, ds_base),
                "'{m}' against DeepSeek base_url must return false"
            );
        }
    }
}
