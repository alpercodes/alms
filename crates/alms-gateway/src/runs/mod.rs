//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md
//!
//! This module is split into focused submodules:
//! - [`lifecycle`] — run creation, execution, completion
//! - [`dm_lifecycle`] — consolidated DM post-run lifecycle (ignore_message detection, conversation end)
//! - [`streaming`] — SSE event streaming (per-run and per-session)
//! - [`notifications`] — DM notification routing, scheduler integration, trigger loops
//! - [`tools`] — runtime event forwarding bridge

pub(crate) mod dm_lifecycle;
#[cfg(test)]
mod integration_tests;
pub(crate) mod lifecycle;
pub(crate) mod markers;
pub(crate) mod notifications;
pub(crate) mod streaming;
mod tools;

// ---------------------------------------------------------------------------
// Re-exports — preserve the public API surface of the former single-file module
// ---------------------------------------------------------------------------

pub use dm_lifecycle::cancel_dm;
pub use lifecycle::{
    ListRunsQuery, cancel_run, create_run, get_run_status, get_run_tool_calls, list_runs,
};
pub(crate) use notifications::{
    completion_notification_loop, dm_event_loop, run_trigger_loop, scheduler_fire_loop,
};
pub use streaming::{
    SessionEventsQuery, stream_agent_events, stream_run_events, stream_session_events,
};

// Public struct and function used by gateway.rs
pub use self::config::{ResolvedAgentConfig, resolve_agent_config};

// ---------------------------------------------------------------------------
// Shared types (used by multiple submodules)
// ---------------------------------------------------------------------------

use crate::api_error;
use alms_core::{RunId, SessionId};
use axum::{Json, http::StatusCode};
use tokio_util::sync::CancellationToken;

/// Valid LLM provider identifiers accepted in per-run overrides.
///
/// This is intentionally separate from `alms_core::secrets::VALID_PROVIDERS`
/// which also includes non-LLM keys like `"telegram"`.
const VALID_LLM_PROVIDERS: &[&str] = &["openai", "anthropic", "openrouter"];

/// Validate that a provider string is a known LLM provider.
///
/// Returns `Ok(())` if valid, or an API error tuple suitable for returning
/// from an Axum handler if the provider is unrecognised.
fn validate_provider(provider: &str) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if VALID_LLM_PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::BAD_REQUEST,
            "INVALID_PROVIDER",
            format!(
                "Unknown provider '{}'. Valid providers: {}",
                provider,
                VALID_LLM_PROVIDERS.join(", ")
            ),
        ))
    }
}

/// Per-run overrides that can be sent by the client to customise a single run.
#[derive(Debug, Default)]
struct RunOverrides {
    model: Option<String>,
    max_tokens: Option<u32>,
    posture: Option<String>,
    provider: Option<String>,
    debug_mode: Option<bool>,
    /// Per-run Anthropic extended-thinking budget override. `Some(0)`
    /// explicitly disables extended thinking for just this run even when
    /// both the per-agent and server default would enable it.
    thinking_budget_tokens: Option<u32>,
    /// Per-run OpenAI-compat reasoning-effort override (#768). Three-layer
    /// precedence: per-run > per-agent > server default from `[llm.openai]`.
    /// Silently ignored when the effective provider is not OpenAI-compatible
    /// or the model isn't a reasoning model.
    reasoning_effort: Option<alms_core::config::ReasoningEffort>,
    /// Per-run Gemini extended-thinking budget override (#794). `Some(0)`
    /// explicitly disables extended thinking for just this run even when
    /// both the per-agent and server default would enable it. Three-layer
    /// precedence: per-run > per-agent > server default from
    /// `[llm.gemini].thinking_budget`. Silently ignored when the effective
    /// provider is not Gemini.
    gemini_thinking_budget: Option<u32>,
}

/// Bundled parameters for [`lifecycle::execute_run`], avoiding a long positional argument list.
struct RunParams {
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
    overrides: RunOverrides,
    context_id: String,
    cancel_token: CancellationToken,
    /// When true, the input message has already been persisted to the session
    /// by the MessageBus. The agent loop uses `run_on_session` to look up the
    /// shared session by `SessionId` directly and skips re-persisting the input.
    is_peer_message: bool,
    /// When true, the run was created by the system (not a user-initiated HTTP
    /// request). All runs from `enqueue_triggered_run` are system-triggered:
    /// peer DM messages, notification runs, subagent completions. These runs
    /// have no human watching, so Guarded posture would hang forever waiting
    /// for approval -- the posture is overridden to Autonomous.
    is_system_triggered: bool,
    /// When true, the input message has already been persisted to the session
    /// by the HTTP handler (before enqueue). The agent loop uses
    /// `run_on_session` so it does not duplicate the message.
    ///
    /// Set by `create_run` to ensure the user's message is visible after a
    /// page reload even when the run is still queued (a reload during the
    /// queued-wait would otherwise find the session history empty).
    input_pre_persisted: bool,
}

/// Prefixes that identify internal (non-user-facing) sessions.
///
/// Sessions whose `context_id` starts with any of these prefixes are excluded
/// when searching for the user's web-chat session and from the session list
/// API (sidebar).  This list is the single source of truth for
/// [`find_user_facing_session`], the `GET /sessions` endpoint,
/// [`notifications::notify_job_completion`], and
/// [`notifications::notify_dm_ended_to_webchat`].
const INTERNAL_SESSION_PREFIXES: &[&str] =
    &["job_", "subagent_", "dm:", "notifications:", "episodic:"];

/// Returns `true` if the given `context_id` belongs to an internal session
/// that should not be targeted by user-facing notifications.
pub(crate) fn is_internal_context_id(context_id: &str) -> bool {
    INTERNAL_SESSION_PREFIXES
        .iter()
        .any(|prefix| context_id.starts_with(prefix))
}

// `classify_session_type` lives in `alms_core` as the single source of truth.
// Re-exported here so that crate-internal callers (routes.rs) can still use the
// `crate::runs::classify_session_type` path unchanged.
pub(crate) use alms_core::classify_session_type;

/// Find the most recent user-facing session for the given agent.
///
/// Returns `None` if the agent has no non-internal sessions.  Sessions are
/// returned in last-activity order by `list_all`, so the first match is the
/// most recently active one.
fn find_user_facing_session(
    session_manager: &alms_session::SessionManager,
    agent_id: alms_core::AgentId,
) -> Option<alms_session::Session> {
    session_manager
        .list_all()
        .into_iter()
        .find(|s| s.agent_id == agent_id && !is_internal_context_id(&s.context_id))
}

// ---------------------------------------------------------------------------
// Agent config resolution (public API used by gateway.rs)
// ---------------------------------------------------------------------------

mod config {
    use tracing::{info, warn};

    /// Result of resolving per-agent config from the agent registry.
    pub struct ResolvedAgentConfig {
        pub agent_config: alms_runtime::AgentConfig,
        pub llm: alms_runtime::LlmClient,
        /// Agent name from registry (None if record not found).
        pub agent_name: Option<String>,
    }

    /// Resolve per-agent config overrides from the agent registry.
    ///
    /// Looks up the agent record by ID, applies model/posture overrides on top
    /// of the base config. Returns the merged config, LLM client with model
    /// override, and agent name for workspace resolution.
    /// No per-run overrides are applied — callers layer those on top.
    pub fn resolve_agent_config(
        agent_id: alms_core::AgentId,
        session_manager: &alms_session::SessionManager,
        base_config: &alms_runtime::AgentConfig,
        llm: &alms_runtime::LlmClient,
        secrets: Option<&alms_core::secrets::SecretsStore>,
    ) -> ResolvedAgentConfig {
        let agent_record =
            session_manager
                .store()
                .and_then(|store| match store.load_agent_by_id(agent_id) {
                    Ok(record) => record,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load agent record for {}, using server defaults: {}",
                            agent_id,
                            e
                        );
                        None
                    }
                });

        let agent_name = agent_record.as_ref().map(|r| r.name.clone());

        let merged = super::apply_overrides(
            base_config.clone(),
            agent_record.as_ref(),
            &super::RunOverrides::default(),
        );

        // Apply per-agent provider override first (changes base_url + api_key),
        // then ALWAYS re-resolve the API key from secrets for the effective
        // provider. This ensures keys set at runtime (via UI or CLI) are picked
        // up even for the default agent which has no per-agent provider field.
        //
        // Snapshot the previous provider AND model so the leak guard below
        // can detect a per-agent provider switch where neither layer (per-
        // agent model nor provider-entry model) supplies a model for the new
        // provider. In that case `apply_provider` leaves `default_model`
        // unchanged from the OLD provider's default -- and that wrong-
        // provider model name reaches the wire (#860).
        let previous_provider = llm.provider().to_string();
        let previous_default_model = llm.default_model().to_string();
        let mut llm = llm.clone();
        if let Some(ref record) = agent_record
            && let Some(ref provider) = record.provider
        {
            info!(
                agent_id = %agent_id,
                provider = %provider,
                "Applying per-agent provider override with secrets resolution"
            );
            llm = if let Some(s) = secrets {
                llm.with_provider_and_secrets(provider, s)
            } else {
                warn!(
                    agent_id = %agent_id,
                    provider = %provider,
                    "No secrets store available for per-agent provider override — API key may be missing"
                );
                llm.with_provider(provider)
            };
        } else if let Some(s) = secrets {
            // No per-agent provider override — re-resolve the key for the
            // server-default provider from the live secrets store.
            info!(
                agent_id = %agent_id,
                provider = %llm.provider(),
                "Re-resolving API key from secrets for default provider"
            );
            llm = llm.with_secrets(s);
        } else {
            warn!(
                agent_id = %agent_id,
                "No secrets store and no per-agent provider — using base LLM client key as-is"
            );
        }

        // Apply the per-agent model. When set this overrides any value
        // `apply_provider` left on `default_model` -- the per-agent override
        // is the user's explicit choice and must reach the wire.
        if let Some(model) = merged.model_override {
            llm = llm.with_model(model);
        } else if llm.provider() != previous_provider
            && llm.default_model() == previous_default_model
        {
            // #860 leak guard: per-agent provider switch fired AND neither
            // a per-agent model nor a `[llm.providers.<new_provider>].model`
            // entry supplied a model for the new provider, so `default_model`
            // is still the OLD provider's default. Clear it so the agent
            // loop's wire request fails fast with a clear "missing model"
            // error rather than 404'ing on the new provider with the wrong
            // model name (the original symptom in #860 was Anthropic 404 on
            // an OpenRouter model name).
            warn!(
                agent_id = %agent_id,
                old_provider = %previous_provider,
                new_provider = %llm.provider(),
                stale_model = %previous_default_model,
                "Per-agent provider switch with no per-agent model and no \
                 provider-entry model -- clearing inherited model so the run \
                 fails fast instead of sending the previous provider's default \
                 model to the new provider's wire (#860)"
            );
            llm = llm.with_model(String::new());
        }

        ResolvedAgentConfig {
            agent_config: merged.agent_config,
            llm,
            agent_name,
        }
    }
}

/// Result of merging server defaults + per-agent + per-run overrides.
struct MergedConfig {
    agent_config: alms_runtime::AgentConfig,
    /// If set, override the LLM client's default model.
    model_override: Option<String>,
}

/// Pure config merging: server defaults -> per-agent overrides -> per-run overrides.
///
/// Returns the merged `AgentConfig` and an optional model override string.
/// The caller is responsible for applying the model override to the `LlmClient`.
fn apply_overrides(
    base: alms_runtime::AgentConfig,
    agent_record: Option<&alms_core::AgentRecord>,
    overrides: &RunOverrides,
) -> MergedConfig {
    let mut cfg = base;

    // -- Model: per-run > per-agent (server default is in LlmClient) --
    let model_override = if overrides.model.is_some() {
        overrides.model.clone()
    } else {
        agent_record.and_then(|r| r.model.clone())
    };

    // -- Per-agent overrides (middle layer) --
    if let Some(record) = agent_record {
        if let Some(ref p) = record.posture
            && let Ok(posture) = p.parse::<alms_runtime::Posture>()
        {
            cfg.posture = posture;
        }
        // Per-agent Anthropic thinking budget. `Some(0)` is a legitimate
        // per-agent override meaning "disable extended thinking for this
        // agent even when the server default enables it", so we honour any
        // `Some` value here.
        if let Some(budget) = record.thinking_budget_tokens {
            cfg.anthropic_thinking_budget = budget;
        }
        // Per-agent OpenAI-compat reasoning effort (#768). `Some(effort)`
        // wins over the server default; `None` falls through.
        if let Some(effort) = record.reasoning_effort {
            cfg.openai_reasoning_effort = Some(effort);
        }
        // Per-agent Gemini thinking budget (#794). `Some(n)` (including
        // `Some(0)`) is a legitimate per-agent override meaning "disable
        // extended thinking for this agent even when the server default
        // enables it", so we honour any `Some` value here — same shape as
        // Anthropic `thinking_budget_tokens` above.
        if let Some(budget) = record.gemini_thinking_budget {
            cfg.gemini_thinking_budget = Some(budget);
        }
        // Per-agent summary provider/model overrides (#872). `Some(_)`
        // overrides the server-level `[context].summary_provider` /
        // `summary_model` for this agent's runs. The validator on
        // `POST /agents` / `PATCH /agents/{id}` enforces the pair-only
        // invariant (both fields set together or both unset) so by the
        // time we get here the per-agent values are guaranteed
        // symmetric. `None` falls through to the server-level setting
        // already on `cfg.context_config`.
        if let Some(ref provider) = record.summary_provider {
            cfg.context_config.summary_provider = Some(provider.clone());
        }
        if let Some(ref model) = record.summary_model {
            cfg.context_config.summary_model = Some(model.clone());
        }
    }

    // -- Per-run overrides (highest precedence) --
    if let Some(m) = overrides.max_tokens.filter(|&m| m > 0) {
        cfg.max_tokens = m;
    }
    if let Some(ref p) = overrides.posture
        && let Ok(posture) = p.parse::<alms_runtime::Posture>()
    {
        cfg.posture = posture;
    }
    if let Some(debug) = overrides.debug_mode {
        cfg.debug_mode = debug;
    }
    // Same semantics as per-agent: `Some(0)` is an explicit per-run
    // disable, not a "use default" sentinel.
    if let Some(budget) = overrides.thinking_budget_tokens {
        cfg.anthropic_thinking_budget = budget;
    }
    // Per-run OpenAI-compat reasoning effort (#768) wins over per-agent
    // and server default.
    if let Some(effort) = overrides.reasoning_effort {
        cfg.openai_reasoning_effort = Some(effort);
    }
    // Per-run Gemini thinking budget (#794) wins over per-agent and
    // server default. Same semantics as Anthropic `thinking_budget_tokens`:
    // `Some(0)` is an explicit per-run disable, not a "use default"
    // sentinel.
    if let Some(budget) = overrides.gemini_thinking_budget {
        cfg.gemini_thinking_budget = Some(budget);
    }

    MergedConfig {
        agent_config: cfg,
        model_override,
    }
}

/// Apply per-run LLM overrides (provider + model) on top of the resolved
/// per-agent `LlmClient`, preserving the three-layer precedence
/// (per-run > per-agent > server default) for the model field.
///
/// The non-trivial bit is the interaction between `provider` and `model`:
///
/// 1. `with_provider_and_secrets` calls `apply_provider`, which rewrites
///    `LlmConfig::default_model` to the new provider entry's `model` field
///    when one is configured in `[llm.providers.<name>]`.
/// 2. Without protection, that silently drops a per-agent model that
///    `resolve_agent_config` had already applied to the resolved client.
///
/// We snapshot the resolved client's `default_model()` before the provider
/// switch and re-apply it afterwards if the switch clobbered the value.
/// The per-run model still wins last when set, so explicit overrides are
/// honored regardless. Closes #833.
///
/// #860 leak guard: when a per-run `provider` override fires AND neither a
/// per-run model nor a per-agent model is set AND the new provider entry
/// has no `model` field, the snapshot is the OLD provider's server default
/// — a model name that does not belong to the new provider. We clear
/// `default_model` in that case so the wire request fails fast with a
/// clear "missing model" error rather than a confusing 404 from the new
/// provider naming the wrong model. The per-agent path plugs the same leak
/// inside `resolve_agent_config`; the guard here covers the case where the
/// switch happens at the per-run layer with no per-agent override.
fn apply_per_run_llm_overrides(
    resolved_llm: alms_runtime::LlmClient,
    overrides: &RunOverrides,
    secrets: &alms_core::secrets::SecretsStore,
) -> alms_runtime::LlmClient {
    let resolved_model = resolved_llm.default_model().to_string();
    let resolved_provider = resolved_llm.provider().to_string();
    let mut llm = resolved_llm;
    if let Some(ref provider) = overrides.provider {
        llm = llm.with_provider_and_secrets(provider, secrets);
        let provider_changed = llm.provider() != resolved_provider;
        if llm.default_model() != resolved_model {
            // #833 path: `apply_provider` overwrote `default_model` with the
            // new provider entry's `model`. Restore the previously resolved
            // model so per-agent overrides are not silently dropped. The
            // per-run model still wins last when set.
            tracing::info!(
                provider = %provider,
                clobbered_model = %llm.default_model(),
                preserved_model = %resolved_model,
                "Per-run provider switch rewrote default_model from a provider entry; \
                 restoring resolved per-agent / server-default model (#833)"
            );
            llm = llm.with_model(resolved_model.clone());
        } else if provider_changed && overrides.model.is_none() && !resolved_model.is_empty() {
            // #860 path: provider changed but `apply_provider` left
            // `default_model` unchanged because the new provider entry has
            // no `model` field. The snapshot is the OLD provider's default
            // -- wrong for the new provider's wire. Clear `default_model`
            // so the wire request fails fast with a clear "missing model"
            // error instead of 404'ing on the new provider with the wrong
            // model name.
            tracing::warn!(
                old_provider = %resolved_provider,
                new_provider = %llm.provider(),
                stale_model = %resolved_model,
                "Per-run provider switch with no per-run / per-agent model and no \
                 provider-entry model -- clearing inherited model so the run \
                 fails fast instead of sending the previous provider's default \
                 model to the new provider's wire (#860)"
            );
            llm = llm.with_model(String::new());
        }
    }
    if let Some(ref model) = overrides.model {
        llm = llm.with_model(model.clone());
    }
    llm
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::{AgentId, AgentRecord};
    use alms_runtime::{AgentConfig, Posture};
    use alms_tools::message_sender::ConversationEndReason;
    use chrono::Utc;
    use lifecycle::{extract_peer_from_dm_context, resolve_posture_for_run};
    use notifications::{
        DM_HISTORY_MAX_CHARS, format_dm_conversation_history, format_dm_ended_notification,
    };

    fn base_config() -> AgentConfig {
        AgentConfig {
            system_prompt: "server default prompt".into(),
            max_tokens: 100_000,
            posture: Posture::FullControl,
            ..AgentConfig::default()
        }
    }

    fn test_agent(model: Option<&str>, posture: Option<&str>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "test-agent".into(),
            description: String::new(),
            model: model.map(String::from),
            posture: posture.map(String::from),
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_no_overrides() {
        let base = base_config();
        let merged = apply_overrides(base.clone(), None, &RunOverrides::default());
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
        assert_eq!(merged.agent_config.max_tokens, 100_000);
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
        assert!(merged.model_override.is_none());
    }

    #[test]
    fn test_per_agent_overrides() {
        let agent = test_agent(Some("custom-model"), Some("guarded"));
        let merged = apply_overrides(base_config(), Some(&agent), &RunOverrides::default());
        assert!(matches!(merged.agent_config.posture, Posture::Guarded));
        assert_eq!(merged.model_override.as_deref(), Some("custom-model"));
        // max_tokens not overridden by agent
        assert_eq!(merged.agent_config.max_tokens, 100_000);
        // system_prompt is never overridden by agent — always server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_per_run_overrides_beat_per_agent() {
        let agent = test_agent(Some("agent-model"), Some("guarded"));
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(8192),
            posture: Some("full_control".into()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), Some(&agent), &overrides);
        // Per-run model wins over per-agent
        assert_eq!(merged.model_override.as_deref(), Some("run-model"));
        // Per-run posture wins over per-agent
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
        // Per-run max_tokens applied
        assert_eq!(merged.agent_config.max_tokens, 8192);
        // system_prompt always stays as server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_per_run_only() {
        let overrides = RunOverrides {
            model: Some("run-model".into()),
            max_tokens: Some(256),
            posture: Some("guarded".into()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        assert_eq!(merged.model_override.as_deref(), Some("run-model"));
        assert_eq!(merged.agent_config.max_tokens, 256);
        assert!(matches!(merged.agent_config.posture, Posture::Guarded));
        // system_prompt stays as server default
        assert_eq!(merged.agent_config.system_prompt, "server default prompt");
    }

    #[test]
    fn test_max_tokens_zero_ignored() {
        let overrides = RunOverrides {
            max_tokens: Some(0),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        assert_eq!(merged.agent_config.max_tokens, 100_000); // unchanged
    }

    #[test]
    fn test_unknown_posture_ignored() {
        let agent = test_agent(None, Some("yolo"));
        let merged = apply_overrides(base_config(), Some(&agent), &RunOverrides::default());
        // Unknown posture keeps server default
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
    }

    #[test]
    fn test_unknown_posture_per_run_ignored() {
        let overrides = RunOverrides {
            posture: Some("yolo".to_string()),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        // Unknown per-run posture keeps server default
        assert!(matches!(merged.agent_config.posture, Posture::FullControl));
    }

    #[test]
    fn test_debug_mode_override() {
        // Off by default
        let merged = apply_overrides(base_config(), None, &RunOverrides::default());
        assert!(!merged.agent_config.debug_mode);

        // Enabled via per-run override
        let overrides = RunOverrides {
            debug_mode: Some(true),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config(), None, &overrides);
        assert!(merged.agent_config.debug_mode);
    }

    // -----------------------------------------------------------------
    // Anthropic extended-thinking three-layer precedence (issue #767)
    // -----------------------------------------------------------------

    fn base_config_with_thinking(budget: u32) -> AgentConfig {
        AgentConfig {
            system_prompt: "server default prompt".into(),
            max_tokens: 100_000,
            posture: Posture::FullControl,
            anthropic_thinking_budget: budget,
            ..AgentConfig::default()
        }
    }

    fn agent_with_thinking(budget: Option<u32>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "thinker".into(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: budget,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_thinking_server_default_inherited() {
        // No per-agent or per-run override: server default (4096) wins.
        let merged = apply_overrides(
            base_config_with_thinking(4096),
            None,
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.anthropic_thinking_budget, 4096);
    }

    #[test]
    fn test_thinking_per_agent_overrides_server_default() {
        // Server default = 0 (disabled), agent opts in with 8192.
        let agent = agent_with_thinking(Some(8192));
        let merged = apply_overrides(
            base_config_with_thinking(0),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.anthropic_thinking_budget, 8192);
    }

    #[test]
    fn test_thinking_per_agent_zero_disables() {
        // Server default = 4096 (enabled). Agent explicitly sets 0 to
        // opt out. This must WIN over the server default.
        let agent = agent_with_thinking(Some(0));
        let merged = apply_overrides(
            base_config_with_thinking(4096),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.anthropic_thinking_budget, 0,
            "per-agent Some(0) must disable thinking even when server default enables it"
        );
    }

    #[test]
    fn test_thinking_per_run_beats_per_agent() {
        let agent = agent_with_thinking(Some(2048));
        let overrides = RunOverrides {
            thinking_budget_tokens: Some(16384),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(base_config_with_thinking(4096), Some(&agent), &overrides);
        assert_eq!(merged.agent_config.anthropic_thinking_budget, 16384);
    }

    #[test]
    fn test_thinking_per_agent_none_falls_through_to_server() {
        // Agent doesn't care — server default (4096) wins.
        let agent = agent_with_thinking(None);
        let merged = apply_overrides(
            base_config_with_thinking(4096),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.anthropic_thinking_budget, 4096);
    }

    // ------------------------------------------------------------------
    // Reasoning-effort precedence (issue #768)
    //
    // Three-layer chain (per-run > per-agent > server default) mirrors the
    // Anthropic `thinking_budget_tokens` path above. `None` at any layer
    // falls through; `Some(effort)` wins.
    // ------------------------------------------------------------------

    fn base_config_with_reasoning(
        effort: Option<alms_core::config::ReasoningEffort>,
    ) -> AgentConfig {
        AgentConfig {
            system_prompt: "server default prompt".into(),
            max_tokens: 100_000,
            posture: Posture::FullControl,
            openai_reasoning_effort: effort,
            ..AgentConfig::default()
        }
    }

    fn agent_with_reasoning(effort: Option<alms_core::config::ReasoningEffort>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "reasoner".into(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: effort,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_reasoning_server_default_inherited() {
        use alms_core::config::ReasoningEffort;
        let merged = apply_overrides(
            base_config_with_reasoning(Some(ReasoningEffort::Medium)),
            None,
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.openai_reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn test_reasoning_per_agent_overrides_server_default() {
        use alms_core::config::ReasoningEffort;
        let agent = agent_with_reasoning(Some(ReasoningEffort::High));
        let merged = apply_overrides(
            base_config_with_reasoning(Some(ReasoningEffort::Low)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.openai_reasoning_effort,
            Some(ReasoningEffort::High),
            "per-agent reasoning_effort must override server default"
        );
    }

    #[test]
    fn test_reasoning_per_run_beats_per_agent() {
        use alms_core::config::ReasoningEffort;
        let agent = agent_with_reasoning(Some(ReasoningEffort::Low));
        let overrides = RunOverrides {
            reasoning_effort: Some(ReasoningEffort::High),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(
            base_config_with_reasoning(Some(ReasoningEffort::Medium)),
            Some(&agent),
            &overrides,
        );
        assert_eq!(
            merged.agent_config.openai_reasoning_effort,
            Some(ReasoningEffort::High),
            "per-run override must win over per-agent and server"
        );
    }

    #[test]
    fn test_reasoning_per_agent_none_falls_through_to_server() {
        use alms_core::config::ReasoningEffort;
        let agent = agent_with_reasoning(None);
        let merged = apply_overrides(
            base_config_with_reasoning(Some(ReasoningEffort::Medium)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.openai_reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn test_reasoning_all_none_leaves_none() {
        let merged = apply_overrides(
            base_config_with_reasoning(None),
            None,
            &RunOverrides::default(),
        );
        assert!(merged.agent_config.openai_reasoning_effort.is_none());
    }

    // ------------------------------------------------------------------
    // Gemini thinking-budget precedence (issue #794)
    //
    // Mirrors the Anthropic `thinking_budget_tokens` path above: three-
    // layer chain (per-run > per-agent > server default), `Some(n)` at
    // any layer wins (including `Some(0)` as an explicit disable), `None`
    // falls through. `None` on the `AgentConfig` is a valid terminal
    // state — it means "inherit the server default at wire-emission
    // time", matching the behaviour documented on
    // `AgentConfig::gemini_thinking_budget`.
    // ------------------------------------------------------------------

    fn base_config_with_gemini_thinking(budget: Option<u32>) -> AgentConfig {
        AgentConfig {
            system_prompt: "server default prompt".into(),
            max_tokens: 100_000,
            posture: Posture::FullControl,
            gemini_thinking_budget: budget,
            ..AgentConfig::default()
        }
    }

    fn agent_with_gemini_thinking(budget: Option<u32>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "gemini-thinker".into(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: budget,
            summary_provider: None,
            summary_model: None,
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_gemini_thinking_server_default_inherited() {
        // No per-agent or per-run override: server default (4096) wins.
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            None,
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.gemini_thinking_budget, Some(4096));
    }

    #[test]
    fn test_gemini_thinking_per_agent_overrides_server_default() {
        // Server default = None (disabled), agent opts in with 8192.
        let agent = agent_with_gemini_thinking(Some(8192));
        let merged = apply_overrides(
            base_config_with_gemini_thinking(None),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.gemini_thinking_budget, Some(8192));
    }

    #[test]
    fn test_gemini_thinking_per_agent_zero_disables() {
        // Server default = 4096 (enabled). Agent explicitly sets 0 to
        // opt out. This must WIN over the server default.
        let agent = agent_with_gemini_thinking(Some(0));
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.gemini_thinking_budget,
            Some(0),
            "per-agent Some(0) must disable Gemini thinking even when server default enables it"
        );
    }

    #[test]
    fn test_gemini_thinking_per_run_beats_per_agent() {
        let agent = agent_with_gemini_thinking(Some(2048));
        let overrides = RunOverrides {
            gemini_thinking_budget: Some(16384),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            Some(&agent),
            &overrides,
        );
        assert_eq!(
            merged.agent_config.gemini_thinking_budget,
            Some(16384),
            "per-run override must win over per-agent and server"
        );
    }

    #[test]
    fn test_gemini_thinking_per_run_zero_disables_over_everything() {
        // Server + per-agent both enable; per-run explicitly disables
        // with `Some(0)` — must win.
        let agent = agent_with_gemini_thinking(Some(8192));
        let overrides = RunOverrides {
            gemini_thinking_budget: Some(0),
            ..RunOverrides::default()
        };
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            Some(&agent),
            &overrides,
        );
        assert_eq!(
            merged.agent_config.gemini_thinking_budget,
            Some(0),
            "per-run Some(0) must disable Gemini thinking even when both lower layers enable it"
        );
    }

    #[test]
    fn test_gemini_thinking_per_agent_none_falls_through_to_server() {
        // Agent doesn't care — server default (4096) wins.
        let agent = agent_with_gemini_thinking(None);
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(merged.agent_config.gemini_thinking_budget, Some(4096));
    }

    #[test]
    fn test_gemini_thinking_all_none_leaves_none() {
        // Everyone defers to the next layer — ends as `None` (which the
        // Gemini adapter treats as "no thinkingConfig on the wire").
        let merged = apply_overrides(
            base_config_with_gemini_thinking(None),
            None,
            &RunOverrides::default(),
        );
        assert!(merged.agent_config.gemini_thinking_budget.is_none());
    }

    // ------------------------------------------------------------------
    // Per-agent summary provider/model overlay (issue #872)
    //
    // `apply_overrides` overlays the per-agent `summary_provider` /
    // `summary_model` from the `AgentRecord` onto the inherited
    // `context_config`. Resolution: per-agent ?? server-level. When
    // both are None at the per-agent layer the server-level setting
    // (already on `cfg.context_config`) shines through — preserving
    // the back-compat path described in #872. The pair-only invariant
    // is enforced upstream in the HTTP handler so by the time the
    // record reaches `apply_overrides` the two fields are guaranteed
    // symmetric.
    // ------------------------------------------------------------------

    fn base_config_with_server_summary(
        summary_provider: Option<&str>,
        summary_model: Option<&str>,
    ) -> AgentConfig {
        AgentConfig {
            context_config: alms_core::config::ContextConfig {
                summary_provider: summary_provider.map(str::to_string),
                summary_model: summary_model.map(str::to_string),
                ..alms_core::config::ContextConfig::default()
            },
            ..AgentConfig::default()
        }
    }

    fn agent_with_summary(provider: Option<&str>, model: Option<&str>) -> AgentRecord {
        let now = Utc::now();
        AgentRecord {
            id: AgentId::new(),
            name: "summary-agent".into(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: provider.map(str::to_string),
            summary_model: model.map(str::to_string),
            is_default: false,
            created_at: now,
            last_active: now,
        }
    }

    #[test]
    fn test_summary_per_agent_overrides_server_default() {
        // Server default points the summary task at OpenRouter; agent
        // overrides to Anthropic. Per-agent must win.
        let agent = agent_with_summary(Some("anthropic"), Some("claude-haiku-4"));
        let merged = apply_overrides(
            base_config_with_server_summary(Some("openrouter"), Some("minimax/minimax-m2.7")),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged
                .agent_config
                .context_config
                .summary_provider
                .as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            merged.agent_config.context_config.summary_model.as_deref(),
            Some("claude-haiku-4")
        );
    }

    #[test]
    fn test_summary_per_agent_none_falls_through_to_server() {
        // Per-agent both-None: the server-level summary config inherited
        // via `context_config.clone()` wins. Identical to today's
        // server-level path — the back-compat guarantee from #872.
        let agent = agent_with_summary(None, None);
        let merged = apply_overrides(
            base_config_with_server_summary(Some("openrouter"), Some("minimax/minimax-m2.7")),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged
                .agent_config
                .context_config
                .summary_provider
                .as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            merged.agent_config.context_config.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );
    }

    #[test]
    fn test_summary_all_none_stays_none() {
        // Both layers unset → the merged config has both fields None.
        // The runtime-side `build_summary_client` short-circuits on
        // `summary_provider = None` to a clone of the agent's main
        // LLM client (the back-compat path), so no separate summary
        // task runs.
        let agent = agent_with_summary(None, None);
        let merged = apply_overrides(
            base_config_with_server_summary(None, None),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert!(
            merged
                .agent_config
                .context_config
                .summary_provider
                .is_none()
        );
        assert!(merged.agent_config.context_config.summary_model.is_none());
    }

    #[test]
    fn test_summary_per_agent_overrides_when_server_unset() {
        // Server default is unset (the post-#872 default); per-agent
        // opts in. Per-agent reaches the merged config exactly as set.
        let agent = agent_with_summary(Some("openrouter"), Some("minimax/minimax-m2.7"));
        let merged = apply_overrides(
            base_config_with_server_summary(None, None),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged
                .agent_config
                .context_config
                .summary_provider
                .as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            merged.agent_config.context_config.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );
    }

    // ------------------------------------------------------------------
    // Clear-sentinel precedence (#809)
    //
    // After `PATCH /agents/{id}` with `clear_*: true`, the stored
    // `AgentRecord` has `None` for that knob. A subsequent `POST /runs`
    // with no per-run override must therefore resolve to the server
    // default — NOT the cleared per-agent value, and NOT `None` on the
    // wire. These tests lock down that behaviour for all three knobs.
    // The SQLite round-trip that produces the `None` record is covered
    // in `agents.rs::tests::clear_sentinels_round_trip_through_sqlite`.
    // ------------------------------------------------------------------

    #[test]
    fn test_clear_sentinel_thinking_budget_falls_through_to_server_default() {
        // Simulates the post-clear state: agent record has
        // `thinking_budget_tokens = None`. A subsequent run with no
        // per-run override must resolve to the server default (4096).
        let agent = agent_with_thinking(None);
        let merged = apply_overrides(
            base_config_with_thinking(4096),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.anthropic_thinking_budget, 4096,
            "after clear, per-agent None must fall through to server default"
        );
    }

    #[test]
    fn test_clear_sentinel_reasoning_effort_falls_through_to_server_default() {
        use alms_core::config::ReasoningEffort;
        // Simulates the post-clear state: agent record has
        // `reasoning_effort = None`. Server default (Medium) must win
        // on the next run.
        let agent = agent_with_reasoning(None);
        let merged = apply_overrides(
            base_config_with_reasoning(Some(ReasoningEffort::Medium)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.openai_reasoning_effort,
            Some(ReasoningEffort::Medium),
            "after clear, per-agent None must fall through to server default"
        );
    }

    #[test]
    fn test_clear_sentinel_gemini_thinking_budget_falls_through_to_server_default() {
        // Simulates the post-clear state: agent record has
        // `gemini_thinking_budget = None`. Server default (4096) must
        // win on the next run.
        let agent = agent_with_gemini_thinking(None);
        let merged = apply_overrides(
            base_config_with_gemini_thinking(Some(4096)),
            Some(&agent),
            &RunOverrides::default(),
        );
        assert_eq!(
            merged.agent_config.gemini_thinking_budget,
            Some(4096),
            "after clear, per-agent None must fall through to server default"
        );
    }

    #[test]
    fn test_validate_provider_accepts_valid_providers() {
        assert!(validate_provider("openai").is_ok());
        assert!(validate_provider("anthropic").is_ok());
        assert!(validate_provider("openrouter").is_ok());
    }

    #[test]
    fn test_validate_provider_rejects_unknown() {
        let err = validate_provider("anthrpoic").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let body = err.1.0;
        assert_eq!(body["error"]["code"], "INVALID_PROVIDER");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("anthrpoic"),
            "error message should mention the invalid provider"
        );
    }

    #[test]
    fn test_validate_provider_rejects_telegram() {
        // telegram is a valid secret key but NOT a valid LLM provider
        let err = validate_provider("telegram").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_system_triggered_overrides_guarded_to_autonomous() {
        let result = resolve_posture_for_run(Posture::Guarded, true);
        assert_eq!(
            result,
            Posture::Autonomous,
            "Guarded posture should be overridden to Autonomous for system-triggered runs"
        );
    }

    #[test]
    fn test_system_triggered_does_not_override_full_control() {
        let result = resolve_posture_for_run(Posture::FullControl, true);
        assert_eq!(
            result,
            Posture::FullControl,
            "FullControl posture should NOT be overridden for system-triggered runs"
        );
    }

    #[test]
    fn test_system_triggered_does_not_override_autonomous() {
        let result = resolve_posture_for_run(Posture::Autonomous, true);
        assert_eq!(
            result,
            Posture::Autonomous,
            "Autonomous posture should remain unchanged for system-triggered runs"
        );
    }

    #[test]
    fn test_user_initiated_keeps_guarded() {
        let result = resolve_posture_for_run(Posture::Guarded, false);
        assert_eq!(
            result,
            Posture::Guarded,
            "Guarded posture should be preserved for user-initiated runs"
        );
    }

    #[test]
    fn test_per_run_provider_override_wires_through() {
        // Verify that setting provider in RunOverrides is carried through
        // to execute_run's LlmClient reconfiguration. We test the building
        // block (with_provider_and_secrets) since execute_run requires full
        // AppState; the wiring in execute_run is:
        //   if let Some(ref provider) = overrides.provider {
        //       llm = llm.with_provider_and_secrets(provider, &secrets);
        //   }
        use alms_runtime::LlmClient;
        use alms_runtime::llm_types::LlmConfig;

        let config = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            ..LlmConfig::default()
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.provider(), "openai");

        // Simulate what execute_run does when overrides.provider is Some
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let mut secrets = alms_core::secrets::SecretsStore::load(secrets_path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-override").unwrap();

        let overrides = RunOverrides {
            provider: Some("anthropic".into()),
            ..RunOverrides::default()
        };

        // Apply provider override the same way execute_run does
        let mut llm = client;
        if let Some(ref provider) = overrides.provider {
            llm = llm.with_provider_and_secrets(provider, &secrets);
        }

        assert_eq!(llm.provider(), "anthropic");
    }

    // -----------------------------------------------------------------------
    // Model-override end-to-end tests (#833)
    //
    // These exercise resolve_agent_config -> lifecycle.rs:569-578 layering at
    // the LlmClient::default_model() level so a regression in the three-layer
    // precedence (per-run > per-agent > server default) cannot slip past the
    // apply_overrides-only unit tests like test_per_run_overrides_beat_per_agent.
    // -----------------------------------------------------------------------

    /// Build an `LlmClient` whose snapshot matches a typical server default
    /// so the model-override regression tests can layer per-agent and
    /// per-run overrides on top.
    fn server_default_llm(model: &str) -> alms_runtime::LlmClient {
        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let config = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: model.into(),
            providers,
            ..LlmConfig::default()
        };
        alms_runtime::LlmClient::new(config).unwrap()
    }

    fn empty_secrets() -> alms_core::secrets::SecretsStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = alms_core::secrets::SecretsStore::load(path)
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        std::mem::forget(dir);
        store
    }

    fn manager_with_agent(record: &AgentRecord) -> alms_session::SessionManager {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        store.create_agent(record).unwrap();
        alms_session::SessionManager::with_store(alms_session::SessionConfig::default(), store)
            .unwrap()
    }

    /// Per-agent model lands on the resolved `LlmClient::default_model()`
    /// the value `loop_impl.rs:153` reads for every wire `CompletionRequest`.
    /// This layer has no `info!` log on success (only the per-run side logs at
    /// `lifecycle.rs:577`), so it is the most likely to silently regress.
    #[test]
    fn test_per_agent_model_lands_on_wire_via_resolve_agent_config() {
        let mut record = test_agent(Some("claude-haiku-4-5-20251001"), None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("server-default-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        assert_eq!(
            resolved.llm.default_model(),
            "claude-haiku-4-5-20251001",
            "per-agent model must land on the LlmClient -- the value loop_impl.rs:153 reads"
        );
    }

    /// Per-run model override beats the per-agent model in the resolved
    /// `LlmClient`, mirroring the layering at `lifecycle.rs:569-578`.
    #[test]
    fn test_per_run_model_beats_per_agent_at_llm_client() {
        let mut record = test_agent(Some("per-agent-model"), None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("server-default-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        let overrides = RunOverrides {
            model: Some("per-run-model".into()),
            ..RunOverrides::default()
        };
        let llm = apply_per_run_llm_overrides(resolved.llm, &overrides, &secrets);
        assert_eq!(
            llm.default_model(),
            "per-run-model",
            "per-run model must beat per-agent model"
        );
    }

    /// No overrides means the resolved client falls back to server default.
    #[test]
    fn test_no_overrides_falls_through_to_server_default_at_llm_client() {
        let mut record = test_agent(None, None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("server-default-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));
        let overrides = RunOverrides::default();
        let llm = apply_per_run_llm_overrides(resolved.llm, &overrides, &secrets);
        assert_eq!(
            llm.default_model(),
            "server-default-model",
            "no overrides at any layer must fall through to server default"
        );
    }

    /// Regression test for #833 -- when a per-run `provider` override is set
    /// without a per-run `model`, the per-agent model must still reach the
    /// wire. Before the fix this scenario silently dropped the per-agent
    /// model because `with_provider_and_secrets` re-runs `apply_provider`,
    /// which can overwrite `default_model` from a custom
    /// `[llm.providers.<name>].model` entry, and the lifecycle only
    /// re-applies `with_model` when the per-run `model` field is set.
    #[test]
    fn test_per_run_provider_does_not_drop_per_agent_model() {
        let mut record = test_agent(Some("claude-haiku-4-5-20251001"), None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();

        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // The bug trigger: provider entry has its own model that
                // would overwrite per-agent model when the per-run code
                // re-applies the provider via with_provider_and_secrets.
                model: Some("claude-sonnet-from-provider-entry".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "openai".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "server-default-model".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));
        assert_eq!(resolved.llm.default_model(), "claude-haiku-4-5-20251001");

        let overrides = RunOverrides {
            provider: Some("anthropic".into()),
            ..RunOverrides::default()
        };
        let llm = apply_per_run_llm_overrides(resolved.llm, &overrides, &secrets);

        assert_eq!(
            llm.default_model(),
            "claude-haiku-4-5-20251001",
            "per-run provider override must NOT clobber per-agent model"
        );
        // Sanity: provider switch did happen.
        assert_eq!(llm.provider(), "anthropic");
    }

    /// Regression test for #860 -- when a per-agent `provider` override is
    /// set together with a per-agent `model` override, the per-agent model
    /// must reach the wire even though the server-default provider entry
    /// has its own `model` field set.
    ///
    /// Bug scenario from the issue: server default provider is `openrouter`
    /// with `[llm.providers.openrouter].model = "moonshotai/kimi-k2.6"`.
    /// Agent has `provider = "anthropic"` and `model = "claude-sonnet-4-6"`.
    /// The expected wire model is `claude-sonnet-4-6` (per-agent), but the
    /// bug surfaced as a 404 from Anthropic referencing `moonshotai/kimi-k2.6`
    /// because the per-agent model was being clobbered before reaching the
    /// wire (parallel of #833 but on the per-agent path).
    #[test]
    fn test_per_agent_provider_does_not_drop_per_agent_model() {
        let mut record = test_agent(Some("claude-sonnet-4-6"), None);
        record.is_default = true;
        record.provider = Some("anthropic".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                // Server-default provider entry pinned to a specific model.
                // This is the value that surfaced on the wire in #860 when
                // the per-agent model was clobbered.
                model: Some("moonshotai/kimi-k2.6".into()),
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openrouter".into(),
            api_key: "openrouter-key".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            default_model: "moonshotai/kimi-k2.6".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        assert_eq!(
            resolved.llm.default_model(),
            "claude-sonnet-4-6",
            "per-agent provider override must NOT clobber per-agent model (#860)"
        );
        assert_eq!(resolved.llm.provider(), "anthropic");
    }

    /// Companion regression for #860 -- the most pernicious shape, where the
    /// new provider's entry has its own `model` field that would clobber
    /// `default_model` inside `apply_provider`. The per-agent `with_model`
    /// must still win because `resolve_agent_config` re-applies it after the
    /// provider switch.
    #[test]
    fn test_per_agent_provider_with_entry_model_does_not_drop_per_agent_model() {
        let mut record = test_agent(Some("claude-sonnet-4-6"), None);
        record.is_default = true;
        record.provider = Some("anthropic".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: Some("moonshotai/kimi-k2.6".into()),
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // Bug trigger -- anthropic entry pinned to its own model so
                // `apply_provider` inside `with_provider_and_secrets`
                // overwrites `default_model` to this value before the
                // per-agent `with_model` step runs.
                model: Some("claude-sonnet-from-anthropic-entry".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openrouter".into(),
            api_key: "openrouter-key".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            default_model: "moonshotai/kimi-k2.6".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        assert_eq!(
            resolved.llm.default_model(),
            "claude-sonnet-4-6",
            "per-agent provider override must NOT clobber per-agent model \
             even when the new provider entry has its own model (#860)"
        );
        assert_eq!(resolved.llm.provider(), "anthropic");
    }

    /// Hypothetical: per-agent provider only, no per-agent model. The
    /// server-default model leaks through to the new provider when the new
    /// provider's `[llm.providers.<name>]` entry has no `model` field.
    /// This is the specific scenario surfaced in #860.
    #[test]
    fn test_per_agent_provider_only_no_per_agent_model_does_not_leak_server_default() {
        let mut record = test_agent(None, None); // no per-agent model
        record.is_default = true;
        record.provider = Some("anthropic".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                // No model field on the anthropic entry, so apply_provider
                // leaves default_model untouched. Without an explicit
                // restore step the server-default kimi model leaks through.
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openrouter".into(),
            api_key: "openrouter-key".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            // The smoking gun -- this is what surfaces on the wire to
            // anthropic in #860, even though the user wanted nothing
            // openrouter-related when they switched providers per-agent.
            default_model: "moonshotai/kimi-k2.6".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        // Pre-fix: default_model() == "moonshotai/kimi-k2.6" (the leak).
        // Post-fix: the leak guard clears default_model so the wire request
        // fails fast with a clear missing-model error rather than 404'ing
        // on Anthropic with the wrong (OpenRouter) model name.
        assert_ne!(
            resolved.llm.default_model(),
            "moonshotai/kimi-k2.6",
            "per-agent provider switch must NOT leak the server-default model \
             of the previous provider to the new provider's wire (#860)"
        );
        assert_eq!(
            resolved.llm.default_model(),
            "",
            "leak guard clears default_model when a per-agent provider switch \
             produces no model from any layer (#860)"
        );
        assert_eq!(resolved.llm.provider(), "anthropic");
    }

    /// Sibling of the per-agent leak: when a per-run `provider` override
    /// is set with no per-run model and the agent has no per-agent model,
    /// the server-default model of the OLD provider leaks through to the
    /// new provider when the new provider entry has no `model` field.
    /// Pre-fix this passes only because #841's snapshot+restore happens to
    /// also paper over the leak; the post-fix behaviour pins the property.
    #[test]
    fn test_per_run_provider_only_no_model_does_not_leak_server_default() {
        let record = test_agent(None, None); // no per-agent model, no per-agent provider
        let mgr = manager_with_agent(&record);
        let base = base_config();

        use alms_runtime::llm_types::LlmConfig;
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        providers.insert(
            "anthropic".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Anthropic,
                base_url: "https://api.anthropic.com/v1".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openrouter".into(),
            api_key: "openrouter-key".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            default_model: "moonshotai/kimi-k2.6".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        let overrides = RunOverrides {
            provider: Some("anthropic".into()),
            ..RunOverrides::default()
        };
        let llm = apply_per_run_llm_overrides(resolved.llm, &overrides, &secrets);

        // Same property as #860 but on the per-run path. Pre-fix this also
        // leaks moonshotai/kimi-k2.6 because the new anthropic entry has no
        // model field of its own. Post-fix the per-run leak guard clears
        // default_model so the wire request fails fast.
        assert_ne!(
            llm.default_model(),
            "moonshotai/kimi-k2.6",
            "per-run provider switch with no model overrides must NOT leak \
             the server-default model of the previous provider (#860)"
        );
        assert_eq!(
            llm.default_model(),
            "",
            "per-run leak guard clears default_model when no layer supplies \
             a model for the new provider (#860)"
        );
        assert_eq!(llm.provider(), "anthropic");
    }

    // -----------------------------------------------------------------------
    // extract_peer_from_dm_context tests (#387)
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_peer_agent_name_is_first() {
        // Context: dm:alice:bob, agent is alice -> peer is bob
        let peer = extract_peer_from_dm_context("dm:alice:bob", "alice");
        assert_eq!(peer.as_deref(), Some("bob"));
    }

    #[test]
    fn test_extract_peer_agent_name_is_second() {
        // Context: dm:alice:bob, agent is bob -> peer is alice
        let peer = extract_peer_from_dm_context("dm:alice:bob", "bob");
        assert_eq!(peer.as_deref(), Some("alice"));
    }

    #[test]
    fn test_extract_peer_agent_name_not_found() {
        // Agent name not in the context_id at all
        let peer = extract_peer_from_dm_context("dm:alice:bob", "charlie");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_non_dm_context() {
        // Not a DM context ID
        let peer = extract_peer_from_dm_context("notifications:alice", "alice");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_malformed_context() {
        // Missing second name
        let peer = extract_peer_from_dm_context("dm:alice", "alice");
        assert!(peer.is_none());
    }

    #[test]
    fn test_extract_peer_empty_context() {
        let peer = extract_peer_from_dm_context("", "alice");
        assert!(peer.is_none());
    }

    // -----------------------------------------------------------------------
    // ignore_message detection integration tests (#387)
    //
    // These test the detection conditions that determine whether
    // end_conversation should be called. Since execute_run requires
    // a full AppState, we test the condition logic directly.
    // -----------------------------------------------------------------------

    /// Build a minimal `ToolCallRecord` with the given role, tool name,
    /// tool_id, and optional result.
    fn make_tool_record(
        role: alms_core::ToolCallRole,
        tool_name: &str,
        tool_id: &str,
        result: Option<&str>,
    ) -> alms_core::ToolCallRecord {
        alms_core::ToolCallRecord {
            seq: 0,
            role,
            tool_name: Some(tool_name.to_string()),
            tool_id: Some(tool_id.to_string()),
            params: None,
            result: result.map(String::from),
            timestamp: Utc::now(),
            from_agent: None,
        }
    }

    /// Helper: delegates to the canonical `should_signal_dm_end` in
    /// `dm_lifecycle` -- the single source of truth for the three-way
    /// ignore_message detection condition (#628).
    fn should_signal_ignore(
        is_peer_message: bool,
        tool_calls: &[alms_core::ToolCallRecord],
        context_id: &str,
    ) -> bool {
        dm_lifecycle::should_signal_dm_end(is_peer_message, tool_calls, context_id)
    }

    #[test]
    fn test_ignore_in_dm_context_triggers_end_conversation() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            should_signal_ignore(true, &records, "dm:alice:bob"),
            "ignore_message in DM context should trigger end_conversation"
        );
    }

    #[test]
    fn test_ignore_in_non_dm_context_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "session:abc123"),
            "ignore_message outside DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_send_message_in_dm_does_not_trigger() {
        // A DM run that ends with send_message should NOT trigger
        // end_conversation -- this was the false-positive bug introduced
        // by the Bug 1 fix in PR #412.
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "send_message in DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_no_tool_calls_in_dm_does_not_trigger() {
        // Empty tool_calls (degenerate LLM response) should NOT trigger
        // end_conversation -- there was no explicit ignore_message call.
        let records: Vec<alms_core::ToolCallRecord> = vec![];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "empty tool_calls in DM context should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_non_peer_message_in_dm_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "dm:alice:bob"),
            "non-peer-message run should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_notification_context_does_not_trigger() {
        // Notification sessions have context_id = "notifications:agent_name"
        // which does NOT start with "dm:", so no false positive.
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "notifications:bob"),
            "notification session should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_job_context_does_not_trigger() {
        let records = vec![
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "call_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "call_1",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(false, &records, "job_some-uuid"),
            "job session should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_tool_role_result_does_not_trigger() {
        // A Tool-role record (tool result) with name "ignore_message" should
        // NOT be counted -- only Assistant-role records (the actual call)
        // paired with a successful Tool-role result.
        let records = vec![make_tool_record(
            alms_core::ToolCallRole::Tool,
            "ignore_message",
            "call_1",
            Some(r#"{"ok":true}"#),
        )];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "Tool-role ignore_message record should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_conflict_batch_does_not_trigger_end_conversation() {
        // Both send_message and ignore_message were called in the same batch.
        // Both are blocked with DM conflict errors.
        // Agent retries with just send_message and succeeds.
        // The old blocked ignore_message should NOT trigger end_conversation.
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            // First batch: conflict -- both tools blocked
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just send_message -- success
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_2",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "conflict-batch followed by clean send_message should NOT trigger end_conversation"
        );
    }

    #[test]
    fn test_conflict_batch_then_clean_ignore_triggers() {
        // Both tools called in conflict batch (both blocked), then agent
        // retries with just ignore_message and succeeds.
        // The clean ignore_message SHOULD trigger end_conversation.
        let conflict_error = format!("Error: {}", alms_core::DM_CONFLICT_MSG);
        let records = vec![
            // First batch: conflict -- both tools blocked
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "send_message",
                "tc_send_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_1",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "send_message",
                "tc_send_1",
                Some(&conflict_error),
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_1",
                Some(&conflict_error),
            ),
            // Second batch: agent retried with just ignore_message -- success
            make_tool_record(
                alms_core::ToolCallRole::Assistant,
                "ignore_message",
                "tc_ignore_2",
                None,
            ),
            make_tool_record(
                alms_core::ToolCallRole::Tool,
                "ignore_message",
                "tc_ignore_2",
                Some(r#"{"ok":true}"#),
            ),
        ];
        assert!(
            should_signal_ignore(true, &records, "dm:alice:bob"),
            "conflict-batch followed by clean ignore_message SHOULD trigger end_conversation"
        );
    }

    #[test]
    fn test_ignore_without_tool_result_does_not_trigger() {
        // Assistant-role ignore_message record exists but no corresponding
        // Tool-role result -- should not trigger.
        let records = vec![make_tool_record(
            alms_core::ToolCallRole::Assistant,
            "ignore_message",
            "call_1",
            None,
        )];
        assert!(
            !should_signal_ignore(true, &records, "dm:alice:bob"),
            "ignore_message without matching Tool result should NOT trigger end_conversation"
        );
    }

    // -----------------------------------------------------------------------
    // format_dm_ended_notification tests (#388, #429)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dm_ended_notification_ignored_no_history() {
        let msg = format_dm_ended_notification("alice", ConversationEndReason::Ignored, None);
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("alice"),
            "notification should mention the agent who ended the conversation"
        );
        assert!(
            msg.contains("chose not to reply"),
            "Ignored reason should explain the agent chose not to reply"
        );
        assert!(
            msg.contains("read_messages"),
            "fallback (no history) should hint at read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_depth_exceeded_no_history() {
        let msg = format_dm_ended_notification("bob", ConversationEndReason::DepthExceeded, None);
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("bob"),
            "notification should mention the peer agent"
        );
        assert!(
            msg.contains("maximum message depth"),
            "DepthExceeded reason should mention the depth limit"
        );
        assert!(
            msg.contains("read_messages"),
            "fallback (no history) should hint at read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_is_not_empty() {
        let msg = format_dm_ended_notification("x", ConversationEndReason::Ignored, None);
        assert!(
            msg.len() > 50,
            "notification should be a substantive message, not a stub"
        );
    }

    #[test]
    fn test_dm_ended_notification_with_history() {
        let history = "[10:00] alice: Hello Bob\n[10:01] bob: Hi Alice, what's up?";
        let msg =
            format_dm_ended_notification("alice", ConversationEndReason::Ignored, Some(history));
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "notification should start with the DM ended prefix"
        );
        assert!(
            msg.contains("Hello Bob"),
            "notification should include conversation content"
        );
        assert!(
            msg.contains("Hi Alice"),
            "notification should include both sides of the conversation"
        );
        assert!(
            msg.contains("full conversation history"),
            "notification with history should mention it contains the transcript"
        );
        assert!(
            !msg.contains("read_messages"),
            "notification with history should NOT suggest read_messages"
        );
    }

    #[test]
    fn test_dm_ended_notification_empty_history_falls_back() {
        let msg = format_dm_ended_notification("alice", ConversationEndReason::Ignored, Some(""));
        assert!(
            msg.contains("read_messages"),
            "empty history string should fall back to read_messages hint"
        );
    }

    // -----------------------------------------------------------------------
    // format_dm_conversation_history tests (#429)
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_dm_history_basic() {
        use alms_session::{Content, Message, Role};

        let messages = vec![
            Message {
                id: "1".into(),
                role: Role::User,
                content: Content::Text("Hello from alice".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            },
            Message {
                id: "2".into(),
                role: Role::User,
                content: Content::Text("Hi alice, I got your message".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "bob"})),
            },
        ];

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.contains("alice: Hello from alice"),
            "should include alice's message with sender label"
        );
        assert!(
            result.contains("bob: Hi alice"),
            "should include bob's message with sender label"
        );
    }

    #[test]
    fn test_format_dm_history_skips_empty_and_markers() {
        use alms_session::{Content, Message, Role};

        let messages = vec![
            Message {
                id: "1".into(),
                role: Role::User,
                content: Content::Text("Real message".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            },
            // Empty text (dm_ended marker body)
            Message {
                id: "2".into(),
                role: Role::User,
                content: Content::Text(String::new()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"message_type": "dm_ended"})),
            },
            // Tool call
            Message {
                id: "3".into(),
                role: Role::Tool,
                content: Content::ToolResult {
                    tool_id: "t1".into(),
                    result: serde_json::json!({"ok": true}),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
            // Synthetic notification
            Message {
                id: "4".into(),
                role: Role::System,
                content: Content::Text("synthetic marker".into()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"synthetic": true})),
            },
        ];

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.contains("Real message"),
            "should include the real message"
        );
        assert!(!result.contains("dm_ended"), "should skip dm_ended markers");
        assert!(!result.contains("Tool result"), "should skip tool results");
        assert!(
            !result.contains("synthetic marker"),
            "should skip synthetic markers"
        );
    }

    #[test]
    fn test_format_dm_history_empty_session() {
        let result = format_dm_conversation_history(&[]);
        assert!(
            result.is_empty(),
            "empty session should produce empty string"
        );
    }

    #[test]
    fn test_format_dm_history_truncation() {
        use alms_session::{Content, Message, Role};

        // Create enough messages to exceed DM_HISTORY_MAX_CHARS
        let messages: Vec<Message> = (0..200)
            .map(|i| Message {
                id: format!("msg_{i}"),
                role: Role::User,
                content: Content::Text(format!("Message number {i} with some padding text to make it longer and ensure truncation kicks in properly")),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "alice"})),
            })
            .collect();

        let result = format_dm_conversation_history(&messages);
        assert!(
            result.len() <= DM_HISTORY_MAX_CHARS,
            "truncated output should be within budget (got {} chars, max {})",
            result.len(),
            DM_HISTORY_MAX_CHARS,
        );
        assert!(
            result.contains("earlier message(s) omitted"),
            "truncated output should indicate messages were omitted"
        );
        // The last message should always be present (most recent)
        assert!(
            result.contains("Message number 199"),
            "truncated output should include the most recent message"
        );
    }

    // -----------------------------------------------------------------------
    // find_user_facing_session / is_internal_context_id tests (#428)
    // -----------------------------------------------------------------------

    #[test]
    fn test_internal_prefixes_detected() {
        for prefix in &["job_", "subagent_", "dm:", "notifications:", "episodic:"] {
            let context_id = format!("{prefix}something");
            assert!(
                is_internal_context_id(&context_id),
                "context_id '{context_id}' should be classified as internal"
            );
        }
    }

    #[test]
    fn test_user_facing_context_ids() {
        // Plain context IDs (web chat, telegram, etc.) are user-facing.
        for ctx in &["web", "default", "telegram_123", "my-custom-context"] {
            assert!(
                !is_internal_context_id(ctx),
                "context_id '{ctx}' should NOT be classified as internal"
            );
        }
    }

    // classify_session_type tests live in alms-core (single source of truth).

    #[test]
    fn test_find_user_facing_session_excludes_internal() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_id = AgentId::new();

        // Create several internal sessions
        mgr.get_or_create(agent_id, "dm:alice:bob");
        mgr.get_or_create(agent_id, "subagent_task_1");
        mgr.get_or_create(agent_id, "job_abc");
        mgr.get_or_create(agent_id, "notifications:alice");
        mgr.get_or_create(agent_id, "episodic:main");

        // No user-facing session yet
        assert!(
            find_user_facing_session(&mgr, agent_id).is_none(),
            "should return None when only internal sessions exist"
        );

        // Create a user-facing session
        let user_session = mgr.get_or_create(agent_id, "web");

        let found = find_user_facing_session(&mgr, agent_id);
        assert!(found.is_some(), "should find the user-facing session");
        assert_eq!(found.unwrap().id, user_session.id);
    }

    #[test]
    fn test_find_user_facing_session_ignores_other_agents() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        // Create a user-facing session for agent B only
        mgr.get_or_create(agent_b, "web");

        assert!(
            find_user_facing_session(&mgr, agent_a).is_none(),
            "should not return sessions belonging to a different agent"
        );
    }

    #[test]
    fn test_find_user_facing_session_no_sessions() {
        let mgr = alms_session::SessionManager::new(alms_session::SessionConfig::default());
        let agent_id = AgentId::new();

        assert!(
            find_user_facing_session(&mgr, agent_id).is_none(),
            "should return None when no sessions exist at all"
        );
    }
}
