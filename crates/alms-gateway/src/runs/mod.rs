//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md
//!
//! This module is split into focused submodules:
//! - `lifecycle` — run creation, execution, completion
//! - `read_api` — read/query HTTP handlers and presentation models
//! - `dm_lifecycle` — consolidated DM post-run lifecycle (ignore_message detection, conversation end)
//! - `streaming` — SSE event streaming (per-run and per-session)
//! - `notifications` — DM notification routing, scheduler integration, trigger loops
//! - [`tools`] — runtime event forwarding bridge

pub(crate) mod dm_lifecycle;
#[cfg(test)]
mod integration_tests;
pub(crate) mod job_episode;
pub(crate) mod lifecycle;
pub(crate) mod markers;
pub(crate) mod notifications;
pub(crate) mod read_api;
pub(crate) mod streaming;
#[cfg(test)]
mod subagent_chip_timing_tests;
pub(crate) mod subagent_self_sink;
// `tools` is `pub` (not `pub(super)`) only so that the integration test in
// `tests/sse_golden_tests.rs` can reach `route_bg_event` for the #1105 bg-path
// ordering regression test. The other items in this module are `pub(super)`
// or `pub(crate)` and stay hidden from external crates.
pub mod tools;

// ---------------------------------------------------------------------------
// Re-exports — preserve the public API surface of the former single-file module
// ---------------------------------------------------------------------------

pub use dm_lifecycle::cancel_dm;
pub use lifecycle::{cancel_run, cancel_subagent, create_run};
pub(crate) use notifications::{
    completion_notification_loop, dm_event_loop, job_episode_sweep_loop, run_trigger_loop,
    scheduler_fire_loop,
};
pub use read_api::{
    ListRunsQuery, get_run_reasoning, get_run_status, get_run_text, get_run_tool_calls, list_runs,
};
pub use streaming::{
    SessionEventsQuery, stream_agent_events, stream_run_events, stream_session_activity,
    stream_session_events,
};
pub(crate) use subagent_self_sink::GatewaySubagentSelfSink;

// ---------------------------------------------------------------------------
// Shared types (used by multiple submodules)
// ---------------------------------------------------------------------------

use alms_core::{RunId, SessionId};
use tokio_util::sync::CancellationToken;

/// Bundled parameters for [`lifecycle::execute_run`], avoiding a long positional argument list.
#[derive(Clone)]
struct RunParams {
    run_id: RunId,
    session_id: SessionId,
    agent_id: alms_core::AgentId,
    input: String,
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
    /// The peer whose DM conversation with this run's agent just ended, when
    /// this run is that end's post-end turn (a `ConversationEnded` trigger).
    /// `None` for every other run.
    ///
    /// `MAX_DM_DEPTH` bounds one conversation; nothing bounds conversations
    /// between a pair, because `end_conversation` clears the depth counters.
    /// The post-end turn is the one place where re-opening is *immediate* and
    /// unattended: the agent has just been handed the transcript and is one
    /// `send_message` away from starting the same conversation at depth 1,
    /// forever. So `send_message` is registered folded toward this peer —
    /// the same treatment `is_peer_message` runs get, which these runs were
    /// missing even though `notifications.rs` already withholds the DM
    /// addendum from them on the same "not a peer message" reasoning (#1299).
    ///
    /// The fold removes exactly one recipient for exactly one turn. Every
    /// other capability of the post-end turn (#556, #1215) is untouched, and
    /// nothing stops the pair re-opening later by ordinary means.
    dm_ended_peer: Option<String>,
}

/// Prefixes that identify internal (non-user-facing) sessions.
///
/// Sessions whose `context_id` starts with any of these prefixes are excluded
/// when searching for the user's web-chat session.  This list is the single
/// source of truth for [`find_user_facing_session`], the `GET /sessions`
/// endpoint, [`notifications::notify_job_completion`], and
/// [`notifications::notify_dm_ended_to_webchat`].
///
/// Note the `GET /sessions` (sidebar) endpoint carves out exceptions from
/// this list: `notifications:*` sessions are always returned (Notifications
/// section) and `job_*` sessions are always returned (collapsed Jobs group,
/// #1197).  Both remain internal for notification-*targeting* purposes —
/// a job-completion marker must never land on another job's session.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::{
        ResolveAgentConfigError, build_resolved_config, resolve_agent_config,
    };
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
            worktree_mode: alms_core::WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: now,
            last_active: now,
        }
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

    // -----------------------------------------------------------------------
    // Model-override end-to-end tests (#833 / #860)
    //
    // These exercise `resolve_agent_config` at the `LlmClient::default_model()`
    // level so a regression in the per-agent layering (per-agent > server
    // default) cannot slip past the helpers. Per-run overrides were removed
    // in the #941 pivot; the leak guards that lived in
    // `apply_per_run_llm_overrides` are no longer needed because there is no
    // per-run path for them to guard. The remaining per-agent leak guard
    // (#860) stays inside `resolve_agent_config` and is exercised by
    // `test_per_agent_provider_only_no_per_agent_model_does_not_leak_server_default`.
    // -----------------------------------------------------------------------

    /// Build an `LlmClient` whose snapshot matches a typical server default
    /// so the model-override regression tests can layer per-agent overrides
    /// on top (per-run overrides were removed in #941; the remaining merge
    /// surface is per-agent > server default).
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

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent model in same provider namespace");

        assert_eq!(
            resolved.llm.default_model(),
            "claude-haiku-4-5-20251001",
            "per-agent model must land on the LlmClient -- the value loop_impl.rs:153 reads"
        );
    }

    // Guard: passes pre-fix because `resolve_agent_config` already calls
    // `with_model(merged.model_override)` after the provider switch, so the
    // per-agent model override survives the cross-provider hop. Pinned here
    // to catch a future refactor that drops the post-switch re-application.
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

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent provider+model both in same namespace");

        assert_eq!(
            resolved.llm.default_model(),
            "claude-sonnet-4-6",
            "per-agent provider override must NOT clobber per-agent model (#860)"
        );
        assert_eq!(resolved.llm.provider(), "anthropic");
    }

    // Guard: passes pre-fix for the same reason as the previous test --
    // `resolve_agent_config` re-applies the per-agent `with_model` after the
    // provider switch, so the new provider entry's own `model` field never
    // wins. Pinned to catch a future refactor that reorders those steps.
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

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent model wins over new provider entry's model");

        assert_eq!(
            resolved.llm.default_model(),
            "claude-sonnet-4-6",
            "per-agent provider override must NOT clobber per-agent model \
             even when the new provider entry has its own model (#860)"
        );
        assert_eq!(resolved.llm.provider(), "anthropic");
    }

    // Regression: fails pre-fix on the exact #860 leak shape -- the
    // server-default OpenRouter `kimi-k2.6` model survives the per-agent
    // provider switch to Anthropic and reaches the wire, producing a
    // confusing 404 from Anthropic referencing `moonshotai/kimi-k2.6`.
    //
    // Pre-#863: the leak guard cleared `default_model` to `""` for fail-fast.
    // Post-#863: the same condition surfaces as a structured
    // `ResolveAgentConfigError::MissingModelAfterProviderSwitch` so the
    // gateway can emit a clean `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`
    // before any LLM call.
    /// Hypothetical: per-agent provider only, no per-agent model. The
    /// server-default model leaks through to the new provider when the new
    /// provider's `[llm.providers.<name>]` entry has no `model` field.
    /// This is the specific scenario surfaced in #860 (and now #863).
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

        let result = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        // Pre-#863: default_model() == "" via the leak-guard empty-clear.
        // Post-#863: structured error so the gateway can return 400.
        match result {
            Err(ResolveAgentConfigError::MissingModelAfterProviderSwitch {
                agent_id,
                new_provider,
                prev_provider,
            }) => {
                assert_eq!(agent_id, record.id);
                assert_eq!(new_provider, "anthropic");
                assert_eq!(prev_provider, "openrouter");
            }
            other => panic!(
                "expected MissingModelAfterProviderSwitch, got {:?}",
                other.map(|r| r.llm.default_model().to_string()),
            ),
        }
    }

    // -----------------------------------------------------------------------
    // Cross-namespace per-agent model drop tests (#942)
    //
    // These exercise the post-#860 sibling guard: when a per-agent provider
    // override switches the effective wire kind AND the per-agent `model`
    // field carries a name from the OLD namespace (e.g. agent record has
    // `provider: anthropic` and `model: gpt-4o` after the operator swapped
    // the provider but forgot to update the model), the per-agent model is
    // dropped before reaching `with_model` and the run falls through to
    // either a `[llm.providers.<new>].model` fallback or the #860
    // empty-clear fail-fast.
    //
    // Namespace check is keyed on `ProviderKind` (Anthropic / Gemini are
    // strict, OpenAiCompatible is permissive — see `model_belongs_to_kind`).
    // Closes #942; closes #863 as a side-effect because the post-fix path
    // no longer silently drops an Anthropic-namespace per-agent model.
    // -----------------------------------------------------------------------

    /// The canonical #942 leak shape. Agent record carries `provider:
    /// anthropic` and `model: gpt-4o-mini` (a stale openai-namespace
    /// model — typical operator workflow: swap provider via PATCH /agents,
    /// forget to update the model). Server default is openai with
    /// `gpt-4o-mini`. Pre-fix: the per-agent `with_model("gpt-4o-mini")`
    /// runs after `apply_provider("anthropic")` and the wire request goes
    /// to Anthropic with `model: gpt-4o-mini` → 404. Post-#863: the
    /// cross-namespace check drops the per-agent model, the anthropic
    /// entry has no `model` field, so `resolve_agent_config` returns
    /// `MissingModelAfterProviderSwitch` and the gateway maps it to a
    /// `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH`.
    #[test]
    fn test_per_agent_provider_switch_drops_per_agent_model_from_old_provider_namespace() {
        let mut record = test_agent(Some("gpt-4o-mini"), None);
        record.is_default = true;
        record.provider = Some("anthropic".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

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
                // No model field on the anthropic entry, so after the
                // per-agent stale-model is dropped the #860 guard fires.
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let result = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets));

        // Pre-fix: default_model() == "gpt-4o-mini" (the per-agent model
        // applied via `with_model` even though it is from the openai
        // namespace and the new provider is anthropic). Wire request 404s.
        // Post-#863: cross-namespace drop fires, no fallback model on the
        // anthropic entry, the missing-model error fires so the gateway
        // returns 400 instead of letting the agent loop send `model: ""`.
        match result {
            Err(ResolveAgentConfigError::MissingModelAfterProviderSwitch {
                agent_id,
                new_provider,
                prev_provider,
            }) => {
                assert_eq!(agent_id, record.id);
                assert_eq!(new_provider, "anthropic");
                assert_eq!(prev_provider, "openai");
            }
            other => panic!(
                "expected MissingModelAfterProviderSwitch for #942 + #863 chain, got {:?}",
                other.map(|r| r.llm.default_model().to_string()),
            ),
        }
    }

    /// Companion: per-agent provider override switches Anthropic → Gemini
    /// (or any non-OpenAiCompatible direction), the per-agent model field
    /// carries a Gemini-namespace name. Same shape as #860 but on the
    /// drop branch — the cross-namespace check should NOT fire because
    /// the model is in the new provider's namespace.
    #[test]
    fn test_per_agent_provider_switch_keeps_per_agent_model_when_namespace_matches() {
        let mut record = test_agent(Some("gemini-2.0-flash"), None);
        record.is_default = true;
        record.provider = Some("gemini".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

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
            "gemini".into(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::Gemini,
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_key_env: None,
                api_key: None,
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-goog-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("gemini", "AIza-test").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent model is in the new provider's namespace");

        assert_eq!(
            resolved.llm.default_model(),
            "gemini-2.0-flash",
            "per-agent gemini-namespace model must be preserved on a cross-provider \
             switch INTO gemini (the namespace check is asymmetric -- it only drops \
             models from the OLD namespace, not models that happen to match the new one)"
        );
        assert_eq!(resolved.llm.provider(), "gemini");
    }

    /// Sanity: `model_belongs_to_kind` is case-insensitive. Provider
    /// model lists are all lowercase today, but a user-typed config with
    /// mixed case (`Claude-3-haiku-20240307`) shouldn't silently get
    /// dropped by the cross-namespace guard. Tim flagged this as a nit
    /// on #944.
    #[test]
    fn test_model_belongs_to_kind_is_case_insensitive() {
        use crate::configuration::model_belongs_to_kind;
        use alms_core::config::ProviderKind;

        assert!(model_belongs_to_kind(
            "Claude-3-haiku-20240307",
            ProviderKind::Anthropic
        ));
        assert!(model_belongs_to_kind(
            "CLAUDE-3-OPUS",
            ProviderKind::Anthropic
        ));
        assert!(model_belongs_to_kind(
            "Gemini-1.5-pro",
            ProviderKind::Gemini
        ));
        assert!(model_belongs_to_kind(
            "Models/Gemini-1.5-pro",
            ProviderKind::Gemini
        ));
        // Negative: still rejects out-of-namespace regardless of case.
        assert!(!model_belongs_to_kind("GPT-4o", ProviderKind::Anthropic));
    }

    /// Symmetric snapshot test against `build_resolved_config`: pin that
    /// the persisted #837 triage snapshot reflects the post-fix wire
    /// model, not the leaked stale-namespace value. This is the surface
    /// operators read via `GET /runs/{id}.resolved_config.model` to
    /// confirm "what model was actually sent" — if the snapshot drifts
    /// from the wire, triage breaks.
    #[test]
    fn test_resolved_config_snapshot_reflects_dropped_per_agent_model() {
        let mut record = test_agent(Some("gpt-4o-mini"), None);
        record.is_default = true;
        record.provider = Some("anthropic".into());
        let mgr = manager_with_agent(&record);
        let base = base_config();

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
                // Anthropic entry pins a fallback model so the snapshot
                // shows the in-namespace fallback (not the empty-clear
                // path -- that one is exercised by the test above).
                model: Some("claude-haiku-4-5-20251001".into()),
                auth_scheme: alms_core::config::AuthScheme::Header {
                    name: "x-api-key".into(),
                },
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let server_cfg = LlmConfig {
            provider: "openai".into(),
            api_key: "openai-key".into(),
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
            providers,
            ..LlmConfig::default()
        };
        let server_llm = alms_runtime::LlmClient::new(server_cfg).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut secrets = alms_core::secrets::SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap_or_else(|_| alms_core::secrets::SecretsStore::empty());
        secrets.set_key("anthropic", "sk-ant-runtime").unwrap();
        std::mem::forget(dir);

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect(
                "success path: cross-namespace per-agent model is dropped, anthropic entry's \
                 fallback model fills in",
            );
        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);

        assert_ne!(
            snapshot.model, "gpt-4o-mini",
            "snapshot must NOT carry the dropped stale per-agent model -- \
             that would mislead triage to think the wire request used it"
        );
        assert_eq!(
            snapshot.model, "claude-haiku-4-5-20251001",
            "snapshot must reflect the post-drop fallback (the anthropic \
             provider entry's model) -- the value the wire request actually \
             carries (#837 triage invariant)"
        );
        assert_eq!(snapshot.provider, "anthropic");
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
            tool_invocation_id: None,
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
        let msg =
            format_dm_ended_notification("alice", ConversationEndReason::Ignored, None, false);
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
        let msg =
            format_dm_ended_notification("bob", ConversationEndReason::DepthExceeded, None, false);
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
        let msg = format_dm_ended_notification("x", ConversationEndReason::Ignored, None, false);
        assert!(
            msg.len() > 50,
            "notification should be a substantive message, not a stub"
        );
    }

    #[test]
    fn test_dm_ended_notification_with_history() {
        let history = "[10:00] alice: Hello Bob\n[10:01] bob: Hi Alice, what's up?";
        let msg = format_dm_ended_notification(
            "alice",
            ConversationEndReason::Ignored,
            Some(history),
            false,
        );
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
        let msg =
            format_dm_ended_notification("alice", ConversationEndReason::Ignored, Some(""), false);
        assert!(
            msg.contains("read_messages"),
            "empty history string should fall back to read_messages hint"
        );
    }

    /// #1215: the ender's self-notification must NOT attribute the ending to
    /// the PEER. For a self-notification `from_name` is the peer (the other
    /// party), but the RECIPIENT is the agent that ended the DM — so a
    /// peer-blaming phrasing is always wrong for it.
    #[test]
    fn test_dm_ended_self_notification_ignored_does_not_blame_peer() {
        let msg = format_dm_ended_notification("alice", ConversationEndReason::Ignored, None, true);
        assert!(
            !msg.contains("Agent \"alice\" ended the conversation"),
            "self-notification must NOT say the peer (alice) ended it; got: {msg}"
        );
        assert!(
            msg.contains("Your DM conversation with agent \"alice\""),
            "self-notification should use self-appropriate phrasing naming the \
             peer only as the other party"
        );
        assert!(
            msg.starts_with("[DM conversation ended]"),
            "still uses the DM-ended template"
        );
    }

    /// #1215: same non-misattribution for the depth-exceeded reason.
    #[test]
    fn test_dm_ended_self_notification_depth_does_not_blame_peer() {
        let msg =
            format_dm_ended_notification("bob", ConversationEndReason::DepthExceeded, None, true);
        assert!(
            !msg.contains("Agent \"bob\" ended"),
            "self-notification must NOT attribute the ending to the peer (bob); got: {msg}"
        );
        assert!(
            msg.contains("Your DM conversation with agent \"bob\""),
            "self-notification should use self-appropriate phrasing"
        );
    }

    /// #1215 guard: the PEER notification (self_notification=false) is
    /// unchanged — it correctly tells the OTHER party that the ender ended it.
    #[test]
    fn test_dm_ended_peer_notification_still_names_ender() {
        let ignored =
            format_dm_ended_notification("alice", ConversationEndReason::Ignored, None, false);
        assert!(
            ignored.contains("Agent \"alice\" ended the conversation (chose not to reply)"),
            "peer notification must still say the ender (alice) ended it; got: {ignored}"
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

    // -----------------------------------------------------------------------
    // build_resolved_config tests (#837)
    //
    // These run the result of `resolve_agent_config` through
    // `build_resolved_config` to confirm the snapshot reflects the
    // **effective** values the LLM adapter will actually use. Crucial for
    // triage of "I set model X but Y was used"-class reports — the
    // snapshot must agree with the adapter. Per-run overrides were removed
    // in the #941 pivot, so the layering this snapshot reflects is just
    // per-agent > server default.
    // -----------------------------------------------------------------------

    /// Per-agent model lands on the snapshot after layering. This is the
    /// scenario the issue's first acceptance test exercises: PATCH a per-
    /// agent model, start a run, confirm the snapshot's `model` matches
    /// the PATCHed value.
    #[test]
    fn test_resolved_config_snapshots_per_agent_model() {
        let mut record = test_agent(Some("per-agent-claude-sonnet-4"), None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("server-default-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent model, no provider switch");

        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert_eq!(
            snapshot.model, "per-agent-claude-sonnet-4",
            "snapshot.model must reflect the per-agent override (the value the adapter sends on the wire)"
        );
        // Provider falls through unchanged because the agent record has
        // no provider override; pinned so the snapshot's provider field
        // tracks the effective wire provider.
        assert_eq!(snapshot.provider, "openai");
    }

    /// No per-agent overrides surfaces the server default in the snapshot.
    /// Pinned so the `From<Run>` / `GET /runs/{id}` triage surface always
    /// agrees with the adapter's view of "what model was actually used".
    #[test]
    fn test_resolved_config_snapshots_server_default_when_no_overrides() {
        let mut record = test_agent(None, None);
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("server-default-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: no per-agent overrides at all");

        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert_eq!(snapshot.model, "server-default-model");
        assert_eq!(snapshot.provider, "openai");
    }

    /// The snapshot's posture / max_tokens / debug_mode reflect the
    /// merged `AgentConfig`. Per-agent posture wins over the server
    /// default; `max_tokens` and `debug_mode` track the server default
    /// because the agent record carries no overrides for those fields.
    #[test]
    fn test_resolved_config_snapshots_posture_and_max_tokens_layering() {
        let mut record = test_agent(None, Some("autonomous"));
        record.is_default = true;
        let mgr = manager_with_agent(&record);
        let base = AgentConfig {
            max_tokens: 7777,
            debug_mode: true,
            ..base_config()
        };
        let server_llm = server_default_llm("any-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent posture only");

        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert_eq!(snapshot.posture, "autonomous", "per-agent posture wins");
        assert_eq!(
            snapshot.max_tokens, 7777,
            "server-default max_tokens reaches the snapshot"
        );
        assert!(
            snapshot.debug_mode,
            "server-default debug_mode reaches the snapshot"
        );
    }

    /// Reasoning / thinking budgets layer through to the snapshot via the
    /// per-agent > server-default chain (#767 / #768 / #794). These three
    /// knobs are the long-tail of "config got dropped between layer X and
    /// the wire" bugs the snapshot is meant to triage.
    #[test]
    fn test_resolved_config_snapshots_reasoning_and_thinking_budgets() {
        use alms_core::config::ReasoningEffort;

        let mut record = test_agent(None, None);
        record.is_default = true;
        record.thinking_budget_tokens = Some(8192); // per-agent Anthropic
        record.reasoning_effort = Some(ReasoningEffort::High); // per-agent OpenAI
        record.gemini_thinking_budget = Some(4096); // per-agent Gemini
        let mgr = manager_with_agent(&record);
        let base = base_config();
        let server_llm = server_default_llm("any-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent reasoning/thinking budgets only");

        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert_eq!(snapshot.thinking_budget_tokens, 8192);
        assert_eq!(snapshot.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(snapshot.gemini_thinking_budget, Some(4096));
    }

    /// `Some(0)` per-agent thinking budget is an explicit per-agent
    /// disable (not "use default" — `Some(0)` is a meaningful value).
    /// Snapshot must record the literal `0` so triage can confirm the
    /// disable reached the wire. Mirrors the wire-format invariant pinned
    /// by #767/#794.
    #[test]
    fn test_resolved_config_snapshots_explicit_zero_thinking_budget() {
        let mut record = test_agent(None, None);
        record.is_default = true;
        record.thinking_budget_tokens = Some(0); // per-agent explicit disable
        let mgr = manager_with_agent(&record);
        // Server default enables thinking; per-agent must override to 0.
        let base = AgentConfig {
            anthropic_thinking_budget: 8192,
            ..base_config()
        };
        let server_llm = server_default_llm("any-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent explicit-zero thinking budget");

        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert_eq!(
            snapshot.thinking_budget_tokens, 0,
            "per-agent Some(0) must surface as a literal 0 in the snapshot, \
             confirming the disable reached the wire"
        );
    }

    // ===================================================================
    // #1003 — Per-agent debug_mode merge through resolve_agent_config
    //
    // The agent record's `debug_mode` flag must reach the resolved
    // `AgentConfig.debug_mode` so the runtime's `if self.config.debug_mode`
    // gate fires and the `ContextDebug` event is emitted on the next
    // turn. Two pinned cells: per-agent `true` overrides a server-default
    // `false`, and per-agent `false` doesn't accidentally clobber a
    // server-level `true` (there is no server-level debug knob today,
    // but a future refactor that introduced one would land here first —
    // pinning the asymmetry now means the merge stays "monotonic on":
    // we OR per-agent into the server default, never override it back
    // to false). This merge is the only debug_mode gate: the #546-era
    // notification debug-flip in `lifecycle::execute_run` was removed.
    // ===================================================================

    #[test]
    fn test_per_agent_debug_mode_true_lands_on_resolved_config() {
        let mut record = test_agent(None, None);
        record.is_default = true;
        record.debug_mode = true; // per-agent enable
        let mgr = manager_with_agent(&record);
        let base = AgentConfig {
            debug_mode: false, // server default
            ..base_config()
        };
        let server_llm = server_default_llm("any-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent debug_mode = true");

        assert!(
            resolved.agent_config.debug_mode,
            "per-agent debug_mode = true must reach resolved AgentConfig.debug_mode \
             so the runtime's emit gate fires"
        );

        // Belt-and-braces: the layered run-config snapshot (#837) also
        // surfaces the effective value, so triage tooling sees the
        // post-merge state for runs by this agent.
        let snapshot = build_resolved_config(&resolved.agent_config, &resolved.llm);
        assert!(
            snapshot.debug_mode,
            "per-agent debug_mode = true must surface in the persisted run snapshot"
        );
    }

    #[test]
    fn test_per_agent_debug_mode_false_does_not_clobber_server_default() {
        // Defensive: if a future refactor introduced a server-level
        // debug knob, the per-agent merge must not flip it OFF when
        // the per-agent value is `false` (the default). The merge is
        // "OR per-agent into server default" — `record.debug_mode = false`
        // is the cleared / default state and must be a no-op.
        let mut record = test_agent(None, None);
        record.is_default = true;
        record.debug_mode = false; // explicit per-agent disable
        let mgr = manager_with_agent(&record);
        let base = AgentConfig {
            debug_mode: true, // hypothetical server-level enable
            ..base_config()
        };
        let server_llm = server_default_llm("any-model");
        let secrets = empty_secrets();

        let resolved = resolve_agent_config(record.id, &mgr, &base, &server_llm, Some(&secrets))
            .expect("success path: per-agent debug_mode = false");

        assert!(
            resolved.agent_config.debug_mode,
            "server-default debug_mode = true must survive the per-agent merge \
             when the per-agent value is the default-false; merge is monotonic on"
        );
    }

    /// `Display` impl on the #863 error variant must mention the agent and
    /// both providers so the structured 400's `message` body is actionable
    /// for operators triaging the rejection.
    #[test]
    fn test_missing_model_after_provider_switch_display_format() {
        let agent_id = AgentId::new();
        let err = ResolveAgentConfigError::MissingModelAfterProviderSwitch {
            agent_id,
            new_provider: "anthropic".into(),
            prev_provider: "openrouter".into(),
        };
        let s = err.to_string();
        assert!(
            s.contains(&agent_id.to_string()),
            "display must mention agent id"
        );
        assert!(s.contains("anthropic"), "display must mention new provider");
        assert!(
            s.contains("openrouter"),
            "display must mention prev provider"
        );
    }
}
