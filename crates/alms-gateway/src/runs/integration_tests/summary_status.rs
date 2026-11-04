// SPDX-License-Identifier: Apache-2.0

//! A failing conversation summary reaches the session the user is looking at,
//! and nothing else changes.
//!
//! The agent runs on its own provider, so its turns succeed, with summaries on
//! the compiled default `openrouter` pair: the setup that has no OpenRouter
//! key and could not summarise, with no sign of it outside the log.

use super::seed_agent;
use crate::server::{AppState, ManagedSubscription};
use crate::sse::SseEventData;
use alms_core::{RunId, SessionId};
use alms_test_support::{Canned, ScriptedLlm};
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const AGENT_KEY: &str = "dummy-agent-key";
const SUMMARY_TEXT: &str = "The user said ping and the agent answered with a short reply.";
/// In the run's input, so it would show if the failure text or the input ended
/// up somewhere it should not.
const INPUT: &str = "ping";

fn text_turn() -> String {
    concat!(
        "data: {\"id\":\"txt\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"m\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string()
}

fn summary_reply() -> Canned {
    Canned::json(
        200,
        serde_json::json!({
            "id": "s", "object": "chat.completion", "created": 1, "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": SUMMARY_TEXT},
                "finish_reason": "stop",
            }],
        })
        .to_string(),
    )
}

/// How the scripted summary provider answers.
#[derive(Clone, Copy)]
enum SummaryProvider {
    /// 401 without a key, and a good summary with one, like OpenRouter.
    NeedsKey,
    UnknownModel,
    Healthy,
}

struct Harness {
    state: AppState,
    summary_provider: Arc<Mutex<SummaryProvider>>,
    llm: ScriptedLlm,
    _dir: tempfile::TempDir,
    _shutdown: CancellationToken,
}

impl Harness {
    async fn boot() -> Self {
        let summary_provider = Arc::new(Mutex::new(SummaryProvider::NeedsKey));
        let route_state = Arc::clone(&summary_provider);
        let llm = ScriptedLlm::routed(move |request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            if body["stream"] == serde_json::Value::Bool(true) {
                return Canned::sse(text_turn());
            }
            match *route_state.lock() {
                SummaryProvider::NeedsKey if !request.headers.contains_key("authorization") => {
                    Canned::json(401, r#"{"error":{"message":"No auth credentials found"}}"#)
                }
                SummaryProvider::UnknownModel => {
                    Canned::json(404, r#"{"error":{"message":"No endpoints found"}}"#)
                }
                _ => summary_reply(),
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();

        let entry = |key: Option<&str>| alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::OpenAiCompatible,
            base_url: llm.base_url(),
            api_key_env: None,
            api_key: key.map(str::to_owned),
            model: None,
            auth_scheme: alms_core::config::AuthScheme::Bearer,
            quirks: alms_core::config::ProviderQuirks::default(),
        };
        let mut providers = std::collections::BTreeMap::new();
        providers.insert("local".to_string(), entry(Some(AGENT_KEY)));
        providers.insert("openrouter".to_string(), entry(None));

        let db_path = dir.path().join("alms.db");
        let (state, shutdown, _completions, _triggers, _dm_events) =
            crate::test_support::TestAppState::new()
                .db_path(db_path.to_str().unwrap())
                .data_dir(dir.path())
                .llm_config(alms_runtime::LlmConfig {
                    provider: "local".to_string(),
                    api_key: AGENT_KEY.to_string(),
                    base_url: llm.base_url(),
                    default_model: "local-model".to_string(),
                    timeout_secs: 30,
                    stream_chunk_timeout_secs: 30,
                    providers,
                    ..alms_runtime::LlmConfig::default()
                })
                .build_with_channels();

        {
            let config = state.agent_config.read();
            let context = &config.context_config;
            assert_eq!(
                context.run_summary_mode,
                alms_core::config::RunSummaryMode::Llm,
                "precondition: the compiled default summarises every run"
            );
            assert_eq!(context.summary_provider.as_deref(), Some("openrouter"));
        }

        Self {
            state,
            summary_provider,
            llm,
            _dir: dir,
            _shutdown: shutdown,
        }
    }

    fn set_summary_provider(&self, behaviour: SummaryProvider) {
        *self.summary_provider.lock() = behaviour;
    }

    fn router(&self) -> axum::Router {
        crate::server::routes::protected_router().with_state(self.state.clone())
    }

    /// `POST /runs`, then wait for the run and its post-run summary to finish.
    /// Returns the run's terminal status.
    async fn run(&self, session_id: SessionId) -> String {
        let calls_before = self.llm.calls();
        let response = self
            .router()
            .oneshot(
                HttpRequest::post("/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "session_id": session_id,
                            "input": { "type": "text", "text": INPUT },
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let run_id: RunId = serde_json::from_value(body["run_id"].clone()).unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(run) = self.state.run_manager.get_run(run_id)
                && run.status().is_terminal()
            {
                break format!("{:?}", run.status());
            }
            assert!(tokio::time::Instant::now() < deadline, "run did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        // One turn request and one summary request.
        while self.llm.calls() < calls_before + 2 {
            assert!(tokio::time::Instant::now() < deadline, "no summary request");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            self.state
                .run_manager
                .wait_drain(Duration::from_secs(20))
                .await,
            "the summary task did not finish"
        );
        status
    }

    async fn put_key(&self, provider: &str, key: &str) {
        let response = self
            .router()
            .oneshot(
                HttpRequest::put("/auth/keys")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "provider": provider, "key": key }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    async fn patch_settings(&self, body: serde_json::Value) -> StatusCode {
        self.router()
            .oneshot(
                HttpRequest::patch("/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
}

/// The `failure` of every `summary_status` a subscriber has received so far.
async fn summary_statuses(sub: &mut ManagedSubscription<SessionId>) -> Vec<serde_json::Value> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await {
        let event: SseEventData = event;
        if event.event_type == "summary_status" {
            seen.push(event.data["failure"].clone());
        }
    }
    seen
}

/// The whole life of the problem, on the setup the maintainer described: an
/// agent that works, and no OpenRouter key.
#[tokio::test]
async fn a_missing_key_shows_once_and_clears_when_a_key_is_added() {
    let h = Harness::boot().await;
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    let mut sub = h.state.run_manager.subscribe_session(session.id);

    // The run itself succeeds, and the session hears about the summary.
    assert_eq!(h.run(session.id).await, "Completed");
    let seen = summary_statuses(&mut sub).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    let failure = &seen[0];
    assert_eq!(failure["code"], "MISSING_KEY");
    assert_eq!(failure["provider"], "openrouter");
    assert_eq!(failure["model"], "google/gemma-4-31b-it");
    let message = failure["message"].as_str().unwrap();
    assert!(message.contains("Settings (Summary section"), "{message}");
    assert!(message.contains("add an OpenRouter key"), "{message}");

    // Still failing: another run, another failed call, nothing new on screen.
    assert_eq!(h.run(session.id).await, "Completed");
    assert!(summary_statuses(&mut sub).await.is_empty());

    // Neither the failure nor its fix text is in the session, so neither can
    // reach a model's context: the next run's request carries none of it.
    let history = h
        .state
        .session_manager
        .get_context_history(session.id)
        .unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.to_display_string().contains("summaries failed")),
        "the failure must not be written to the session"
    );
    let bodies = h.llm.request_bodies().await;
    assert!(!bodies.iter().any(|b| b.contains("summaries failed")));

    // Adding the key clears it straight away, without waiting for a run.
    h.put_key("openrouter", "dummy-summary-key").await;
    assert_eq!(
        summary_statuses(&mut sub).await,
        vec![serde_json::Value::Null]
    );

    // And the next run bears it out, without saying so a second time.
    assert_eq!(h.run(session.id).await, "Completed");
    assert!(summary_statuses(&mut sub).await.is_empty());
    assert!(
        h.state
            .run_manager
            .summary_status_snapshot(session.id)
            .is_none()
    );
}

/// A key for a provider the failure is not about leaves the problem showing.
#[tokio::test]
async fn a_key_for_another_provider_does_not_clear_it() {
    let h = Harness::boot().await;
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    h.run(session.id).await;

    h.put_key("anthropic", "dummy-other-key").await;
    assert!(
        h.state
            .run_manager
            .summary_status_snapshot(session.id)
            .is_some()
    );
}

/// A provider recovering on its own clears the banner with the next success,
/// and an unknown model is not presented as a key problem.
#[tokio::test]
async fn an_unknown_model_does_not_blame_a_key_and_clears_on_success() {
    let h = Harness::boot().await;
    h.put_key("openrouter", "dummy-summary-key").await;
    h.set_summary_provider(SummaryProvider::UnknownModel);
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    let mut sub = h.state.run_manager.subscribe_session(session.id);

    assert_eq!(h.run(session.id).await, "Completed");
    let seen = summary_statuses(&mut sub).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["code"], "REJECTED_REQUEST");
    let message = seen[0]["message"].as_str().unwrap().to_lowercase();
    assert!(message.contains("change the summary model"), "{message}");
    assert!(!message.contains(" key"), "{message}");

    h.set_summary_provider(SummaryProvider::Healthy);
    assert_eq!(h.run(session.id).await, "Completed");
    assert_eq!(
        summary_statuses(&mut sub).await,
        vec![serde_json::Value::Null]
    );
}

/// Unsetting the pair in Settings clears the problem, and the next run's
/// summary goes to the agent's own provider and works.
#[tokio::test]
async fn unsetting_the_summary_pair_clears_it() {
    let h = Harness::boot().await;
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    let mut sub = h.state.run_manager.subscribe_session(session.id);
    h.run(session.id).await;
    assert_eq!(summary_statuses(&mut sub).await.len(), 1);

    // A PATCH that does not touch the pair changes nothing.
    assert_eq!(
        h.patch_settings(serde_json::json!({ "context": { "strategy": "truncate" } }))
            .await,
        StatusCode::OK
    );
    assert!(summary_statuses(&mut sub).await.is_empty());

    assert_eq!(
        h.patch_settings(serde_json::json!({
            "context": { "summary_model": "", "summary_provider": "" }
        }))
        .await,
        StatusCode::OK
    );
    assert_eq!(
        summary_statuses(&mut sub).await,
        vec![serde_json::Value::Null]
    );

    assert_eq!(h.run(session.id).await, "Completed");
    assert!(summary_statuses(&mut sub).await.is_empty());
}

/// Opening the session after the failure, or reloading the page, must still
/// show it: the event is transient and is not in any replay log. Read through
/// the production SSE route.
#[tokio::test]
async fn a_stream_opened_later_starts_with_the_problem() {
    let h = Harness::boot().await;
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    h.run(session.id).await;

    let open_stream = || async {
        let response = h
            .router()
            .oneshot(
                HttpRequest::get(format!("/sessions/{}/events", session.id.0))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let mut text = String::new();
        // Everything replayed is ready at once, so a short gap ends the read.
        while let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(200), stream.next()).await
        {
            text.push_str(&String::from_utf8_lossy(&chunk));
        }
        text
    };

    let text = open_stream().await;
    let state_at = text.find("event: stream_state").expect("stream_state");
    let status_at = text.find("event: summary_status").expect("summary_status");
    let replay_at = text
        .find("event: run_created")
        .expect("replayed run events");
    assert!(state_at < status_at && status_at < replay_at, "{text}");
    assert!(text.contains("MISSING_KEY"), "{text}");

    // Fixed: a reload shows nothing.
    h.put_key("openrouter", "dummy-summary-key").await;
    let text = open_stream().await;
    assert!(text.contains("event: stream_state"), "{text}");
    assert!(!text.contains("event: summary_status"), "{text}");
}

/// Deleting the session forgets its problem.
#[tokio::test]
async fn deleting_the_session_forgets_the_problem() {
    let h = Harness::boot().await;
    let agent = seed_agent(&h.state, "local-agent");
    let session = h.state.session_manager.get_or_create(agent, "web-chat");
    h.run(session.id).await;
    assert!(
        h.state
            .run_manager
            .summary_status_snapshot(session.id)
            .is_some()
    );

    let response = h
        .router()
        .oneshot(
            HttpRequest::delete(format!("/sessions/{}", session.id.0))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        h.state
            .run_manager
            .summary_status_snapshot(session.id)
            .is_none()
    );
}
