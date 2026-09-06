// SPDX-License-Identifier: Apache-2.0

//! `AgentConfig` defaults and the per-agent knobs that thread into the request.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;

#[tokio::test]
async fn test_agent_config_default() {
    let config = AgentConfig::default();
    assert!(!config.system_prompt.is_empty());
    assert_eq!(config.posture, Posture::Guarded);
}

/// `AgentConfig.anthropic_thinking_budget` threads through to every LLM
/// request the agent loop issues. Without this invariant, per-agent
/// overrides would land in config but never be seen by the provider.
///
/// This is a shape-assertion test — we don't run a real LLM; we build
/// the `CompletionRequest` exactly as the agent loop does and check the
/// thinking budget follows.
#[test]
fn test_agent_config_thinking_budget_threads_into_request() {
    let cfg = AgentConfig {
        anthropic_thinking_budget: 4096,
        max_tokens: 2048,
        ..AgentConfig::default()
    };

    // Build the request the same way `agent_loop` does. We can't call
    // `stream_llm_call` here (no mock LLM setup), but the request
    // builder is a one-liner so we replicate it directly.
    let mut request = CompletionRequest::new("claude-sonnet-4-20250514")
        .with_messages(vec![LlmMessage::user("hi")])
        .with_max_tokens(cfg.max_tokens);
    if cfg.anthropic_thinking_budget > 0 {
        request = request.with_thinking_budget(cfg.anthropic_thinking_budget);
    }

    assert_eq!(request.thinking_budget_tokens, Some(4096));

    // And when budget is 0, the field is None (adapter skips the wire
    // field in that case — see anthropic.rs tests).
    let cfg0 = AgentConfig {
        anthropic_thinking_budget: 0,
        ..AgentConfig::default()
    };
    let mut req0 = CompletionRequest::new("x").with_messages(vec![LlmMessage::user("hi")]);
    if cfg0.anthropic_thinking_budget > 0 {
        req0 = req0.with_thinking_budget(cfg0.anthropic_thinking_budget);
    }
    assert!(req0.thinking_budget_tokens.is_none());
}

/// #866: `with_summary_llm` populates the dedicated summary client.
///
/// Default state: `summary_llm` is `None` so the in-loop compact
/// path (renamed from `sliding-summary` in #869) falls back to
/// `self.llm` (pre-#866 behaviour).
///
/// After `with_summary_llm`, the field is `Some(client)` whose provider
/// can differ from `self.llm`'s provider. This is the path the gateway
/// uses when `[context].summary_provider` is configured: it clones the
/// agent's resolved client, calls `with_provider_and_secrets` on the
/// clone, and hands the re-targeted client to the runtime via
/// `with_summary_llm`. The runtime never re-resolves the provider — it
/// just reads back what the gateway plumbed in.
#[tokio::test]
async fn test_with_summary_llm_sets_dedicated_client_for_866() {
    use crate::LlmConfig;

    let agent_llm = LlmClient::new(LlmConfig {
        provider: "anthropic".into(),
        default_model: "claude-sonnet-4-20250514".into(),
        ..LlmConfig::default()
    })
    .unwrap();

    // Build a separate client for the summary task on a different
    // provider — exactly what the gateway does when
    // `[context].summary_provider = "openrouter"` is set.
    let summary_llm = LlmClient::new(LlmConfig {
        provider: "openrouter".into(),
        default_model: "minimax/minimax-m2.7".into(),
        ..LlmConfig::default()
    })
    .unwrap();

    let runtime =
        AgentRuntime::new(AgentId::new(), AgentConfig::default(), agent_llm.clone()).unwrap();

    // Default: no summary client wired -> agent's llm is used.
    assert!(
        runtime.summary_llm.is_none(),
        "AgentRuntime::new must leave summary_llm as None for back-compat"
    );
    assert_eq!(runtime.llm.provider(), "anthropic");

    // After with_summary_llm, the dedicated client is present and points
    // at a different provider than the agent's llm. The agent's primary
    // llm (used for the main run loop) is untouched.
    let runtime = runtime.with_summary_llm(summary_llm);
    let summary_client = runtime
        .summary_llm
        .as_ref()
        .expect("with_summary_llm must populate the field");
    assert_eq!(
        summary_client.provider(),
        "openrouter",
        "summary client must keep the override provider"
    );
    assert_eq!(
        runtime.llm.provider(),
        "anthropic",
        "agent's main llm must NOT be re-targeted by with_summary_llm"
    );
}
