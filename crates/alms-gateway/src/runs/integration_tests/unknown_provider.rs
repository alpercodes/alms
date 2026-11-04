// SPDX-License-Identifier: Apache-2.0

//! An agent stored with a provider that is not configured must not run (#172).
//!
//! The agent API refuses such a name, but a record can still hold one:
//! `alms agent create` writes the registry directly, and a provider entry can
//! be removed from `alms.toml` after the agent was saved. Before the fix,
//! `apply_provider` kept the server default's `base_url`, cleared the key and
//! recorded the bad name, so the prompt went to the default provider with no
//! key while `resolved_config` named a provider nothing had talked to.

use super::*;
use crate::runs::{RunParams, lifecycle};
use alms_core::registry::AgentRecord;
use alms_core::{CreateRunRequest, Run, RunInput, RunStatus};
use axum::Json;
use axum::extract::State;
use tokio_util::sync::CancellationToken;

/// A gateway whose default provider is a scripted LLM, with one agent pinned
/// to `provider`. The agent has a model, so the #863 missing-model rejection
/// cannot be what stops the run.
async fn boot_with_agent_on(provider: &str) -> (AppState, ScriptedLlm, AgentId, SessionId) {
    let llm = ScriptedLlm::always(Canned::sse("data: [DONE]\n\n")).await;
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "openrouter".to_string(),
        alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::OpenAiCompatible,
            base_url: llm.base_url(),
            api_key: Some("dummy-default-key".to_string()),
            ..Default::default()
        },
    );
    let (state, _shutdown, _completions, _triggers, _dm_events) = TestAppState::new()
        .in_memory_sqlite()
        .llm_config(alms_runtime::LlmConfig {
            provider: "openrouter".to_string(),
            base_url: llm.base_url(),
            default_model: "test-model".to_string(),
            timeout_secs: 5,
            stream_chunk_timeout_secs: 5,
            providers,
            ..alms_runtime::LlmConfig::default()
        })
        .build_with_channels();
    state.agent_config.write().context_config.run_summary_mode =
        alms_core::config::RunSummaryMode::Off;

    let agent_id = AgentId::new();
    let record = AgentRecord {
        id: agent_id,
        provider: Some(provider.to_string()),
        model: Some("agent-model".to_string()),
        ..AgentRecord::for_test("stale-provider")
    };
    state
        .session_manager
        .store()
        .expect("the test state has a store")
        .create_agent(&record)
        .expect("seed the agent");
    let session = state.session_manager.get_or_create(agent_id, "web");
    (state, llm, agent_id, session.id)
}

#[tokio::test]
async fn post_runs_rejects_an_agent_stored_with_an_unknown_provider() {
    let (state, llm, agent_id, session_id) = boot_with_agent_on("removed-provider").await;

    let result = lifecycle::create_run(
        State(state.clone()),
        Json(CreateRunRequest {
            session_id,
            agent_id: Some(agent_id),
            input: RunInput::Text {
                text: "hello".into(),
            },
        }),
    )
    .await;
    let Err((status, body)) = result else {
        panic!("an unconfigured provider must be rejected before the run is created");
    };

    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body.0["error_code"], "AGENT_PROVIDER_NOT_CONFIGURED");
    assert_eq!(body.0["provider"], "removed-provider");
    assert_eq!(body.0["agent_id"], agent_id.0.to_string());
    assert_eq!(llm.calls(), 0, "nothing may reach any provider");
}

/// Scheduled, DM, notification and subagent-completion runs enter here without
/// `create_run`'s pre-flight, so this is where the prompt used to leave.
#[tokio::test]
async fn a_non_http_run_on_an_unknown_provider_fails_and_sends_nothing() {
    let (state, llm, agent_id, session_id) = boot_with_agent_on("removed-provider").await;

    let run = Run::new(session_id, agent_id, "secret prompt".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    lifecycle::execute_run(
        state.clone(),
        RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "web".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    assert_eq!(
        llm.calls(),
        0,
        "the prompt must not go to the default provider's URL: {:?}",
        llm.request_headers("authorization").await
    );
    let finished = state.run_manager.get_run(run_id).expect("the run exists");
    assert_eq!(finished.status(), RunStatus::Failed);
    let error = finished.error.clone().unwrap_or_default();
    assert!(
        error.contains("removed-provider") && error.contains("not configured"),
        "the run must say why it failed: {error}"
    );
}

/// The rejection is about names nothing can route, not about pinning: a run
/// for an agent on a configured entry is still accepted.
#[tokio::test]
async fn an_agent_on_a_configured_provider_still_runs() {
    let (state, _llm, agent_id, session_id) = boot_with_agent_on("openrouter").await;

    let result = lifecycle::create_run(
        State(state.clone()),
        Json(CreateRunRequest {
            session_id,
            agent_id: Some(agent_id),
            input: RunInput::Text {
                text: "hello".into(),
            },
        }),
    )
    .await;

    assert!(result.is_ok(), "a configured provider must not be rejected");
}
