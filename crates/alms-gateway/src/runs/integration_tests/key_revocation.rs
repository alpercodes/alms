// SPDX-License-Identifier: Apache-2.0

//! Removing an API key takes effect on the next run, on every run path.
//!
//! `Gateway::new` resolves the server-default provider's key and builds its
//! `LlmClient` with it. `AppState::new` clones that client into `state.llm`,
//! which `execute_run` and the coordinator clone for every run and subagent,
//! and the `Gateway` keeps its own copy for Telegram. `DELETE
//! /auth/keys/{provider}` edits only the `SecretsStore`, so the per-run
//! re-resolution (`with_secrets`, or `with_provider_and_secrets` for an
//! override) is the only thing that can stop a removed key from being sent.
//! Before the fix it kept the base client's key whenever the store had none,
//! so a removed key went on being sent until a restart.
//!
//! Each test writes the key into the secrets file *before* boot, so it is
//! in the clients `Gateway::new` and `AppState::new` build (the premise of
//! the bug), removes it with `DELETE /auth/keys/openrouter` through the
//! router, and checks the `Authorization` header the scripted provider
//! received. The Telegram loop itself needs a bot, so its test stops at the
//! resolved client the loop would hand to `AgentRuntime::new`.

use super::seed_agent;
use crate::server::AppState;
use alms_core::secrets::SecretsStore;
use alms_core::{AgentId, RunId, SessionId};
use alms_test_support::{Canned, ScriptedLlm};
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// The key `alms auth set openrouter` stored before the gateway booted.
const STORE_KEY: &str = "sk-or-store-key-to-remove";
/// A key `alms.toml` declares on `[llm.providers.openrouter]`.
const ENTRY_KEY: &str = "sk-or-key-from-alms-toml";

/// One OpenAI-style streamed text turn, the wire shape the `openrouter`
/// provider parses.
fn text_turn(text: &str) -> String {
    format!(
        concat!(
            "data: {{\"id\":\"txt\",\"object\":\"chat.completion.chunk\",\"created\":1,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        text
    )
}

struct Harness {
    state: AppState,
    llm: ScriptedLlm,
    _dir: tempfile::TempDir,
    _shutdown: CancellationToken,
}

impl Harness {
    /// A gateway booted with [`STORE_KEY`] in its secrets file, on an
    /// `openrouter` provider pointed at a scripted LLM. `entry_key` is the
    /// `api_key` on the provider entry, if `alms.toml` declares one.
    async fn boot(entry_key: Option<&str>) -> Self {
        let h = Self::boot_with_summaries(entry_key).await;
        // One provider request per run: the post-run episodic summary is
        // fire-and-forget on a client resolved at run start, so its request
        // could land after the DELETE and carry the key legitimately.
        h.state.agent_config.write().context_config.run_summary_mode =
            alms_core::config::RunSummaryMode::Off;
        h
    }

    /// [`boot`](Self::boot) with the post-run episodic summary left on, as
    /// the compiled defaults have it, so each run makes a second request.
    async fn boot_with_summaries(entry_key: Option<&str>) -> Self {
        let llm = ScriptedLlm::always(Canned::sse(text_turn("ok"))).await;
        let dir = tempfile::tempdir().unwrap();
        // `secrets_path_from_db` puts the store next to the database.
        SecretsStore::load(dir.path().join("secrets.json"))
            .unwrap()
            .set_key("openrouter", STORE_KEY)
            .unwrap();

        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openrouter".to_string(),
            alms_core::config::ProviderEntry {
                kind: alms_core::config::ProviderKind::OpenAiCompatible,
                base_url: llm.base_url(),
                api_key_env: None,
                api_key: entry_key.map(str::to_owned),
                model: None,
                auth_scheme: alms_core::config::AuthScheme::Bearer,
                quirks: alms_core::config::ProviderQuirks::default(),
            },
        );
        let db_path = dir.path().join("alms.db");
        let (state, shutdown, _completions, _triggers, _dm_events) =
            crate::test_support::TestAppState::new()
                .db_path(db_path.to_str().unwrap())
                .data_dir(dir.path())
                .llm_config(alms_runtime::LlmConfig {
                    provider: "openrouter".to_string(),
                    base_url: llm.base_url(),
                    default_model: "test-model".to_string(),
                    timeout_secs: 30,
                    stream_chunk_timeout_secs: 30,
                    providers,
                    ..alms_runtime::LlmConfig::default()
                })
                .build_with_channels();

        assert!(
            state.llm.read().has_api_key(),
            "precondition: the shared client the run paths clone was built \
             with the store's key at boot"
        );
        Self {
            state,
            llm,
            _dir: dir,
            _shutdown: shutdown,
        }
    }

    /// `DELETE /auth/keys/{provider}` through the production router.
    async fn delete_key(&self, provider: &str) {
        let response = crate::server::routes::protected_router()
            .with_state(self.state.clone())
            .oneshot(
                HttpRequest::delete(format!("/auth/keys/{provider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["removed"], true,
            "the key must have been stored: {body}"
        );
    }

    /// `POST /runs` through the production router, then wait for the run
    /// to finish.
    async fn run_to_completion(&self, session_id: SessionId) {
        let response = crate::server::routes::protected_router()
            .with_state(self.state.clone())
            .oneshot(
                HttpRequest::post("/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "session_id": session_id,
                            "input": { "type": "text", "text": "ping" },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(
            status.is_success(),
            "POST /runs returned {status}: {}",
            String::from_utf8_lossy(&body)
        );
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let run_id: RunId = serde_json::from_value(body["run_id"].clone()).unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(run) = self.state.run_manager.get_run(run_id)
                && run.status().is_terminal()
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "run {run_id:?} did not finish within 20s"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The `Authorization` header on every provider request so far.
    async fn authorization(&self) -> Vec<Option<String>> {
        self.llm.request_headers("authorization").await
    }

    /// Wait until the scripted provider has answered `n` requests. A run's
    /// summary is spawned when the run completes, so a terminal run does
    /// not mean its summary request has been sent.
    async fn wait_for_requests(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while self.llm.calls() < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the provider saw {} of {n} requests within 20s",
                self.llm.calls()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The `model` of every provider request so far.
    async fn models(&self) -> Vec<String> {
        self.llm
            .request_bodies()
            .await
            .iter()
            .map(|body| {
                let body: serde_json::Value = serde_json::from_str(body).unwrap();
                body["model"].as_str().unwrap_or_default().to_owned()
            })
            .collect()
    }
}

fn bearer(key: &str) -> Option<String> {
    Some(format!("Bearer {key}"))
}

/// `POST /runs` for an agent on the server default: the `with_secrets`
/// path, which `execute_run` also takes for DM, notification and job runs.
#[tokio::test]
async fn a_removed_key_is_not_sent_by_the_next_post_runs_run() {
    let h = Harness::boot(None).await;
    let agent = seed_agent(&h.state, "keyholder");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");

    h.run_to_completion(session.id).await;
    assert_eq!(h.authorization().await, vec![bearer(STORE_KEY)]);

    h.delete_key("openrouter").await;
    h.run_to_completion(session.id).await;
    assert_eq!(
        h.authorization().await,
        vec![bearer(STORE_KEY), None],
        "the run after `DELETE /auth/keys/openrouter` must send no key: \
         neither the store nor the provider entry has one"
    );
}

/// The post-run episodic summary on the compiled defaults
/// (`run_summary_mode = "llm"`, `summary_provider = "openrouter"`, here the
/// agent's own provider) goes through `build_summary_client`'s
/// `with_provider_and_secrets` on the run's client. The summary of a run
/// that starts after the `DELETE` must send no key either (Tim S4 on
/// #194). `ScriptedLlm::always` answers the non-streaming summary call with
/// SSE, so the summary fails to parse and falls back to the heuristic; its
/// request and header are recorded all the same.
#[tokio::test]
async fn a_removed_key_is_not_sent_by_the_next_runs_summary() {
    let h = Harness::boot_with_summaries(None).await;
    let summary_model = {
        let config = h.state.agent_config.read();
        let context = &config.context_config;
        assert_eq!(
            context.run_summary_mode,
            alms_core::config::RunSummaryMode::Llm,
            "precondition: the compiled default summarises every run"
        );
        assert_eq!(
            context.summary_provider.as_deref(),
            Some("openrouter"),
            "precondition: the compiled default summary provider"
        );
        context.summary_model.clone().expect("compiled default")
    };
    let agent = seed_agent(&h.state, "keyholder");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");

    // The run's turn, then its summary: both before the DELETE.
    h.run_to_completion(session.id).await;
    h.wait_for_requests(2).await;
    assert_eq!(
        h.authorization().await,
        vec![bearer(STORE_KEY), bearer(STORE_KEY)]
    );

    h.delete_key("openrouter").await;
    h.run_to_completion(session.id).await;
    h.wait_for_requests(4).await;
    assert_eq!(
        h.models().await,
        vec![
            "test-model".to_string(),
            summary_model.clone(),
            "test-model".to_string(),
            summary_model,
        ],
        "each run makes one turn request, then one summary request"
    );
    assert_eq!(
        h.authorization().await,
        vec![bearer(STORE_KEY), bearer(STORE_KEY), None, None],
        "the run after `DELETE /auth/keys/openrouter`, and its summary, must \
         send no key"
    );
}

/// With a key on the provider entry, removing the store key falls back to
/// it (the store wins while it has one).
#[tokio::test]
async fn a_removed_store_key_falls_back_to_the_provider_entry_key() {
    let h = Harness::boot(Some(ENTRY_KEY)).await;
    let agent = seed_agent(&h.state, "keyholder");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");

    h.run_to_completion(session.id).await;
    h.delete_key("openrouter").await;
    h.run_to_completion(session.id).await;
    assert_eq!(
        h.authorization().await,
        vec![bearer(STORE_KEY), bearer(ENTRY_KEY)]
    );
}

/// A per-agent `provider` naming the server-default provider takes
/// `with_provider_and_secrets`, whose same-provider miss used to keep the
/// key too.
#[tokio::test]
async fn a_removed_key_is_not_sent_for_an_agent_overriding_to_the_same_provider() {
    let h = Harness::boot(None).await;
    let record = alms_core::registry::AgentRecord {
        provider: Some("openrouter".to_string()),
        model: Some("test-model".to_string()),
        ..alms_core::registry::AgentRecord::for_test("pinned")
    };
    h.state
        .session_manager
        .store()
        .unwrap()
        .create_agent(&record)
        .unwrap();
    let session = h.state.session_manager.get_or_create(record.id, "web-chat");

    h.run_to_completion(session.id).await;
    h.delete_key("openrouter").await;
    h.run_to_completion(session.id).await;
    assert_eq!(h.authorization().await, vec![bearer(STORE_KEY), None]);
}

/// Subagents clone the coordinator's handle on the same shared client.
#[tokio::test]
async fn a_removed_key_is_not_sent_by_the_next_subagent() {
    use alms_tools::SubagentDispatcher;

    let h = Harness::boot(None).await;
    let dispatch = || {
        h.state.coordinator.dispatch(
            "say ok".to_string(),
            SessionId::new(),
            AgentId::new(),
            None,
            None,
            Some("researcher".to_string()),
            None,
            None,
        )
    };

    dispatch().await.expect("first subagent runs");
    assert_eq!(h.authorization().await, vec![bearer(STORE_KEY)]);

    h.delete_key("openrouter").await;
    dispatch().await.expect("second subagent runs");
    assert_eq!(h.authorization().await, vec![bearer(STORE_KEY), None]);
}

/// Telegram resolves from the `Gateway`'s own client, built at boot and
/// never updated, against the secrets store it shares with `AppState`.
#[tokio::test]
async fn a_removed_key_reaches_telegram_resolution() {
    let h = Harness::boot(None).await;
    let agent = seed_agent(&h.state, "keyholder");
    let gateway = h.state.gateway.lock().await;

    let before = gateway
        .resolve_telegram_run_config(agent)
        .unwrap_or_else(|e| panic!("resolve before removal: {e}"));
    assert!(before.llm.has_api_key());

    h.delete_key("openrouter").await;
    assert!(
        gateway.llm().has_api_key(),
        "precondition: the Gateway's boot-time client still carries the key"
    );
    let after = gateway
        .resolve_telegram_run_config(agent)
        .unwrap_or_else(|e| panic!("resolve after removal: {e}"));
    assert!(
        !after.llm.has_api_key(),
        "a Telegram run after `DELETE /auth/keys/openrouter` must get a \
         client with no key"
    );
}
