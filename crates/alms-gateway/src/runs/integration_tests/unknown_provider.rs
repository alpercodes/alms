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
    let run_error = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .find(|event| event.event_type == "run_error")
        .expect("the failure is published as run_error");
    assert_eq!(
        run_error.data["error"]["code"], "AGENT_PROVIDER_NOT_CONFIGURED",
        "the code must not be guessed from the message: {:?}",
        run_error.data
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

/// Telegram has no HTTP response to carry the refusal, so its loop logs and
/// drops the message when this resolve fails; the prompt must go nowhere.
#[tokio::test]
async fn a_telegram_message_for_an_agent_on_an_unknown_provider_is_refused() {
    let (state, llm, agent_id, _session_id) = boot_with_agent_on("removed-provider").await;
    let gateway = state.gateway.lock().await;
    let err = match gateway.resolve_telegram_run_config(agent_id) {
        Ok(_) => panic!("Telegram must refuse an unconfigured provider"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            crate::configuration::ResolveAgentConfigError::UnknownProvider { .. }
        ),
        "got: {err}"
    );
    assert_eq!(llm.calls(), 0);
}

/// An operator who removes or renames a `[llm.providers]` entry learns at
/// boot which agents it stranded, not at each one's first run.
#[tokio::test]
async fn boot_warns_about_agents_on_unconfigured_providers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("alms.db");
    let db = db.to_str().expect("utf-8 path");
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "mylab".to_string(),
        alms_core::config::ProviderEntry::default(),
    );
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        providers,
        ..alms_runtime::LlmConfig::default()
    };

    let seeded = TestAppState::new()
        .db_path(db)
        .data_dir(tmp.path())
        .llm_config(llm_config.clone())
        .build();
    let store = seeded.session_manager.store().expect("sqlite store");
    for record in [
        AgentRecord {
            provider: Some("mylab".to_string()),
            ..AgentRecord::for_test("on-mylab")
        },
        AgentRecord {
            provider: Some("gemini".to_string()),
            ..AgentRecord::for_test("on-builtin")
        },
        AgentRecord {
            provider: Some("removed".to_string()),
            ..AgentRecord::for_test("on-removed")
        },
        AgentRecord {
            summary_provider: Some("gone".to_string()),
            summary_model: Some("m".to_string()),
            ..AgentRecord::for_test("summary-on-gone")
        },
    ] {
        store.create_agent(&record).expect("seed the agent");
    }
    drop(seeded);

    let captured = alms_test_support::capture_events(tracing::Level::WARN, || {
        let _rebooted = TestAppState::new()
            .db_path(db)
            .data_dir(tmp.path())
            .llm_config(llm_config)
            .build();
    });

    let warned: Vec<(String, Option<String>, Option<String>)> = captured
        .at_target("alms.config")
        .filter(|e| e.has_field("agent"))
        .map(|e| {
            (
                e.field("agent").unwrap_or_default().to_string(),
                e.field("provider").map(str::to_string),
                e.field("summary_provider").map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        warned,
        vec![
            ("on-removed".to_string(), Some("removed".to_string()), None),
            (
                "summary-on-gone".to_string(),
                None,
                Some("gone".to_string())
            ),
        ],
        "only the stranded agents are named:\n{captured}"
    );
}

/// #126: an earlier version could store a blank `model` override, which runs
/// send as the model name. Boot names each such agent, and only those.
#[tokio::test]
async fn boot_warns_about_agents_with_a_blank_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("alms.db");
    let db = db.to_str().expect("utf-8 path");
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };

    let seeded = TestAppState::new()
        .db_path(db)
        .data_dir(tmp.path())
        .llm_config(llm_config.clone())
        .build();
    let store = seeded.session_manager.store().expect("sqlite store");
    for (name, model) in [
        ("blank-model", Some("   ")),
        ("empty-model", Some("")),
        ("real-model", Some("gpt-4o")),
        ("no-model", None),
    ] {
        let record = AgentRecord {
            model: model.map(str::to_string),
            ..AgentRecord::for_test(name)
        };
        store.create_agent(&record).expect("seed the agent");
    }
    drop(seeded);

    let captured = alms_test_support::capture_events(tracing::Level::WARN, || {
        let _rebooted = TestAppState::new()
            .db_path(db)
            .data_dir(tmp.path())
            .llm_config(llm_config)
            .build();
    });

    let mut warned: Vec<String> = captured
        .at_target("alms.config")
        .filter(|e| e.has_field("model") && e.message.contains("blank model override"))
        .map(|e| e.field("agent").unwrap_or_default().to_string())
        .collect();
    warned.sort();
    assert_eq!(
        warned,
        vec!["blank-model".to_string(), "empty-model".to_string()],
        "only the blank overrides are named:\n{captured}"
    );
}

/// The WARNs about a server-level `[context].summary_provider`, from booting
/// with `alms.toml`'s pair set to `toml_pair` and `settings_json` written.
fn server_summary_warnings(
    toml_pair: Option<(&str, &str)>,
    settings_json: Option<&str>,
) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    if let Some(json) = settings_json {
        std::fs::write(crate::settings::settings_path(tmp.path()), json).expect("settings.json");
    }
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "mylab".to_string(),
        alms_core::config::ProviderEntry::default(),
    );
    let captured = alms_test_support::capture_events(tracing::Level::WARN, || {
        let mut builder =
            TestAppState::new()
                .data_dir(tmp.path())
                .llm_config(alms_runtime::LlmConfig {
                    mock: true,
                    providers,
                    ..alms_runtime::LlmConfig::default()
                });
        if let Some((provider, model)) = toml_pair {
            builder = builder.summary_pair(provider, model);
        }
        let _state = builder.build();
    });
    captured
        .at_target("alms.config")
        .filter(|e| e.has_field("summary_provider") && !e.has_field("agent"))
        .map(|e| e.field("summary_provider").unwrap_or_default().to_string())
        .collect()
}

/// #172: a removed entry the server-level summary pair names is reported at
/// boot too, whether the pair came from `alms.toml` or `settings.json`.
#[tokio::test]
async fn boot_warns_about_a_server_summary_provider_with_no_entry() {
    assert_eq!(
        server_summary_warnings(Some(("gone", "m")), None),
        vec!["gone".to_string()],
        "from alms.toml"
    );
    assert_eq!(
        server_summary_warnings(
            Some(("mylab", "m")),
            Some(r#"{"context":{"summary_provider":"removed","summary_model":"m"}}"#),
        ),
        vec!["removed".to_string()],
        "settings.json overrides alms.toml, and is what the check sees"
    );
    for (pair, case) in [
        (Some(("mylab", "m")), "a declared entry"),
        (Some(("gemini", "m")), "a built-in"),
        (None, "no summary pair"),
    ] {
        assert!(
            server_summary_warnings(pair, None).is_empty(),
            "no WARN for {case}"
        );
    }
}
