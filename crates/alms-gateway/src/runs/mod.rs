//! Run management for ALMS Gateway
//!
//! Implements POST /runs and GET /runs/{id}/events per docs/api.md
//!
//! This module is split into focused submodules:
//! - [`lifecycle`] — run creation, execution, completion
//! - [`streaming`] — SSE event streaming (per-run and per-session)
//! - [`notifications`] — DM notification routing, scheduler integration, trigger loops
//! - [`tools`] — runtime event forwarding bridge

pub(crate) mod lifecycle;
pub(crate) mod notifications;
pub(crate) mod streaming;
mod tools;

// ---------------------------------------------------------------------------
// Re-exports — preserve the public API surface of the former single-file module
// ---------------------------------------------------------------------------

pub use lifecycle::{
    ListRunsQuery, cancel_run, create_run, get_run_status, get_run_tool_calls, list_runs,
};
pub(crate) use notifications::{
    completion_notification_loop, run_trigger_loop, scheduler_fire_loop,
};
pub use streaming::{SessionEventsQuery, stream_run_events, stream_session_events};

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
        if let Some(model) = merged.model_override {
            llm = llm.with_model(model);
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
    if let Some(record) = agent_record
        && let Some(ref p) = record.posture
        && let Ok(posture) = p.parse::<alms_runtime::Posture>()
    {
        cfg.posture = posture;
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

    MergedConfig {
        agent_config: cfg,
        model_override,
    }
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
        }
    }

    /// Helper: evaluates the three-way condition for ignore_message detection.
    ///
    /// Mirrors the production logic in `execute_run()`: the condition is
    /// `is_peer_message && ran_ignore_message && context_id.starts_with("dm:")`.
    ///
    /// Uses `alms_core::ran_ignore_message_successfully` which requires a
    /// matching non-conflict `Tool`-role result for each `Assistant`-role
    /// `ignore_message` record.
    fn should_signal_ignore(
        is_peer_message: bool,
        tool_calls: &[alms_core::ToolCallRecord],
        context_id: &str,
    ) -> bool {
        let ran_ignore_message = alms_core::ran_ignore_message_successfully(tool_calls);
        is_peer_message && ran_ignore_message && context_id.starts_with("dm:")
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
