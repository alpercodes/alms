// SPDX-License-Identifier: Apache-2.0

//! Per-run config resolution: the live server default pair (#1148), the missing-model guard (#863), and token-budget validation (#919).

use super::{
    drain_activity_events, drain_events, subscribe_activity, subscribe_agent, subscribe_session,
    test_app_state, test_app_state_with_sqlite,
};
use crate::gateway::GatewayConfig;
use crate::server::AppState;
use crate::test_env_locks::BudgetValidationEnvGuard;
use alms_coordinator::SubagentCompletion;
use alms_coordinator::message_bus::{DmEvent, RunTrigger};
use alms_core::{AgentId, Run, RunStatus, SessionId};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// =====================================================================
// #1148 — the server-default `(model, provider)` pair is live for the
// NEXT run, with no daemon restart.
//
// These are the end-to-end pins. Everything below drives the real
// `create_run` -> queue -> `execute_run` chain and asserts against
// `run.resolved_config()` — the snapshot `mark_run_as_running_with_config`
// takes from the `LlmClient` the agent loop is about to send on. A test
// that only asserted `state.server_llm_default` changed would pass with
// the run path still reading a boot-time clone, i.e. it would pass
// against the bug.
// =====================================================================

/// Everything a #1148 test needs kept alive for its duration.
///
/// The `data_dir` tempdir is load-bearing, not tidiness. `AppState::new`
/// reads `{data_dir}/settings.json` at boot and `persist_settings` writes
/// it on every accepted PATCH, so a harness that leaves `data_dir` at the
/// `GatewayConfig::default()` cwd-relative `./.alms` would have one test's
/// PATCH silently become the next test's boot-time server default —
/// order-dependent failures that only reproduce when the suite is run in
/// a particular sequence. Each harness gets its own directory instead.
struct LlmDefaultHarness {
    state: AppState,
    _data_dir: tempfile::TempDir,
    _shutdown_token: CancellationToken,
    _completion_rx: mpsc::UnboundedReceiver<SubagentCompletion>,
    _trigger_rx: mpsc::Receiver<RunTrigger>,
    _dm_event_rx: mpsc::Receiver<DmEvent>,
}

/// Build an `AppState` with a mock LLM, a SQLite store, an isolated
/// `data_dir`, and a populated `[llm.providers]` map, so `PATCH /settings`
/// can validate provider switches (the map is config-file-only and
/// `GatewayConfig::default()` leaves it empty).
fn llm_default_harness(provider: &str, model: &str) -> LlmDefaultHarness {
    let mut providers = std::collections::BTreeMap::new();
    providers.insert(
        "openrouter".to_string(),
        alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_env: None,
            api_key: Some("sk-or-test".into()),
            model: None,
            auth_scheme: alms_core::config::AuthScheme::Bearer,
            quirks: alms_core::config::ProviderQuirks::default(),
        },
    );
    providers.insert(
        "anthropic".to_string(),
        alms_core::config::ProviderEntry {
            kind: alms_core::config::ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com/v1".into(),
            api_key_env: None,
            api_key: Some("sk-ant-test".into()),
            // No entry-level model on purpose: it is what makes a
            // provider-only switch fall through to the #863 decision.
            model: None,
            auth_scheme: alms_core::config::AuthScheme::Header {
                name: "x-api-key".into(),
            },
            quirks: alms_core::config::ProviderQuirks::default(),
        },
    );
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        provider: provider.to_string(),
        default_model: model.to_string(),
        providers,
        ..alms_runtime::LlmConfig::default()
    };
    let data_dir = tempfile::tempdir().expect("tempdir for isolated settings.json");
    let gateway_config = GatewayConfig {
        db_path: Some(":memory:".to_string()),
        data_dir: Some(data_dir.path().to_path_buf()),
        llm_config,
        ..GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();
    let (trigger_tx, trigger_rx) = mpsc::channel(64);
    let (dm_event_tx, dm_event_rx) = mpsc::channel(64);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();
    LlmDefaultHarness {
        state,
        _data_dir: data_dir,
        _shutdown_token: shutdown_token,
        _completion_rx: completion_rx,
        _trigger_rx: trigger_rx,
        _dm_event_rx: dm_event_rx,
    }
}

/// Seed an agent record with the given per-agent overrides.
fn seed_llm_default_test_agent(
    state: &AppState,
    name: &str,
    model: Option<&str>,
    provider: Option<&str>,
) -> AgentId {
    use alms_core::registry::AgentRecord;

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        model: model.map(str::to_string),
        provider: provider.map(str::to_string),
        ..AgentRecord::for_test(name)
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");
    agent_id
}

/// Apply a `PATCH /settings` body and assert it was accepted.
///
/// Goes through the real handler (not a direct `server_llm_default`
/// write) so every test below exercises the validation + commit + client
/// rebuild chain an operator's UI click actually takes.
async fn patch_server_default(state: &AppState, patch: serde_json::Value) {
    use axum::Json;
    use axum::extract::State;
    use axum::response::IntoResponse;

    let resp = crate::settings::patch_settings(State(state.clone()), Json(patch.clone()))
        .await
        .into_response();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "PATCH /settings {patch} must be accepted"
    );
}

/// Drive one `POST /runs` to `run_started` and return the
/// `ResolvedRunConfig` the run path actually committed.
async fn resolved_config_for_one_run(
    state: &AppState,
    agent_id: AgentId,
    context_id: &str,
) -> alms_core::ResolvedRunConfig {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let session = state.session_manager.get_or_create(agent_id, context_id);
    let session_id = session.id;
    // Subscribe BEFORE `create_run`: the producer persists the resolved
    // snapshot via `mark_run_as_running_with_config` immediately before
    // broadcasting `run_started` (#895 ordering), so observing the event
    // is sufficient to know the snapshot is queryable.
    let mut session_rx = subscribe_session(state, session_id);

    let req = CreateRunRequest {
        session_id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session_id.0.to_string(),
            "input": { "type": "text", "text": "which model am I?" },
        }))
        .expect("CreateRunRequest must deserialize")
    };

    let (status, resp) =
        match crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await {
            Ok(ok) => ok,
            Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
        };
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let run_id = resp.0.run_id;

    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), session_rx.recv())
            .await
            .expect("test must observe run_started within 10s")
            .expect("session sender must not close before run_started");
        if event.event_type == "run_started" {
            break;
        }
    }

    state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after create_run enqueued it")
        .resolved_config()
        .expect("resolved_config must be populated once the run reaches Running")
        .clone()
}

/// **The core acceptance test for #1148.** A `PATCH /settings` that moves
/// the server-default model must reach the *next* run — no restart.
///
/// Pre-fix, `state.llm` was a by-value clone taken in `AppState::new`, so
/// this run resolved against the boot model and the operator was told
/// (correctly, at the time) that a restart was required. Post-fix the
/// PATCH rebuilds the shared client the run path reads.
///
/// The assertion is on `run.resolved_config().model` — the snapshot taken
/// from the client the agent loop is about to send on — not on
/// `server_llm_default`, which would still be green with the bug present.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_model_reaches_the_next_run_without_restart() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    // No per-agent model or provider — this agent inherits the server
    // default, which is the population the issue is about.
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    // Baseline: the boot pair is what a run resolves to today.
    let before = resolved_config_for_one_run(state, agent_id, "web-before").await;
    assert_eq!(before.model, "z-ai/glm-5.2");
    assert_eq!(before.provider, "openrouter");

    // Operator changes the server-default model in the UI.
    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    // ...and the very next run uses it. No restart in between.
    let after = resolved_config_for_one_run(state, agent_id, "web-after").await;
    assert_eq!(
        after.model, "moonshotai/kimi-k2.5",
        "#1148: the next run must resolve against the patched server default. \
         A boot-time `state.llm` clone would still report z-ai/glm-5.2 here."
    );
    assert_eq!(
        after.provider, "openrouter",
        "a model-only PATCH must not disturb the provider"
    );
}

/// A live server-default switch must not step on agents that carry their
/// own model. Per-agent overrides are the higher-precedence layer and
/// stay that way — `PATCH /settings` only moves the value agents fall
/// back to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_does_not_override_a_per_agent_model() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let pinned =
        seed_llm_default_test_agent(state, "pinned-agent", Some("openai/gpt-4o-mini"), None);
    let inheriting = seed_llm_default_test_agent(state, "inheriting-agent", None, None);

    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    let pinned_cfg = resolved_config_for_one_run(state, pinned, "web").await;
    assert_eq!(
        pinned_cfg.model, "openai/gpt-4o-mini",
        "a per-agent model must still win over a freshly patched server default"
    );

    let inheriting_cfg = resolved_config_for_one_run(state, inheriting, "web").await;
    assert_eq!(
        inheriting_cfg.model, "moonshotai/kimi-k2.5",
        "an agent without an override must pick the new default up — the same \
         PATCH has to move one agent and not the other"
    );
}

/// A live server-default **provider** switch retargets the wire for the
/// next run: provider name and model both move, and the mock adapter the
/// run resolves is rebuilt from `[llm.providers.anthropic]`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patched_server_default_provider_reaches_the_next_run_without_restart() {
    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    patch_server_default(
        state,
        serde_json::json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }),
    )
    .await;

    let after = resolved_config_for_one_run(state, agent_id, "web").await;
    assert_eq!(after.provider, "anthropic");
    assert_eq!(after.model, "claude-sonnet-4-6");
}

/// **The coherence constraint.** The `#863`
/// `MISSING_MODEL_AFTER_PROVIDER_SWITCH` decision must be computed
/// against the *live* server-default pair, not the boot-time one.
///
/// The agent here pins `provider: anthropic` and carries no model. While
/// the server default is also `anthropic` that is not a switch at all, so
/// the run inherits the server-default model and starts fine. The moment
/// the operator moves the server default to `openrouter`, the same agent
/// record becomes a genuine provider switch with no model available at
/// any layer — and `POST /runs` must reject it with the structured 400
/// before any LLM call.
///
/// If the run path still read a boot-time client, the second `create_run`
/// would happily succeed and the agent would send an OpenRouter slug to
/// Anthropic's wire. Getting this wrong is what turns a config change
/// into a fleet of opaque downstream 4xx errors, so it is pinned
/// end-to-end rather than at the helper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_model_after_provider_switch_is_judged_against_the_live_default() {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let harness = llm_default_harness("anthropic", "claude-sonnet-4-6");
    let state = &harness.state;
    // Per-agent provider matching the server default => not a switch.
    let agent_id = seed_llm_default_test_agent(state, "anthropic-pinned", None, Some("anthropic"));

    let before = resolved_config_for_one_run(state, agent_id, "web-before").await;
    assert_eq!(before.provider, "anthropic");
    assert_eq!(
        before.model, "claude-sonnet-4-6",
        "baseline: no provider switch, so the server-default model applies"
    );

    // Move the server default off anthropic. The agent record is
    // untouched — but it is now a provider switch with no model anywhere.
    patch_server_default(
        state,
        serde_json::json!({
            "provider": "openrouter",
            "model": "z-ai/glm-5.2",
        }),
    )
    .await;

    let session = state.session_manager.get_or_create(agent_id, "web-after");
    let req = CreateRunRequest {
        session_id: session.id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session.id.0.to_string(),
            "input": { "type": "text", "text": "should be rejected" },
        }))
        .expect("CreateRunRequest must deserialize")
    };
    let err = crate::runs::lifecycle::create_run(State(state.clone()), Json(req))
        .await
        .expect_err(
            "the live provider switch must be rejected — a boot-time client \
             would have let this run through with a cross-namespace model",
        );
    assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        err.1.0["error_code"], "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
        "the structured 400 must still fire, now against the live pair: {:?}",
        err.1.0
    );
    assert_eq!(err.1.0["new_provider"], "anthropic");
    assert_eq!(
        err.1.0["prev_provider"], "openrouter",
        "`prev_provider` must name the LIVE server default, not the boot one"
    );
}

/// In-flight runs are unaffected: a PATCH that lands while a run is
/// already executing must not retarget that run's wire. The run resolves
/// its client once at start and holds it by value for the duration.
///
/// **Coverage boundary — this is the weakest of the five #1148 pins, and
/// deliberately so.** `resolved_config()` is written once by
/// `try_mark_run_as_running_with_config` and never rewritten, and nothing
/// here holds the run open past `run_started` — with the mock adapter it
/// has most likely already finished. So the assertion would stay green
/// even if the runtime *did* re-read the shared handle mid-run.
///
/// The property itself is structurally guaranteed rather than test-
/// enforced: `llm` is moved into `AgentRuntime::new` and the loop owns it
/// by value, so there is no handle left to re-read. Holding a run open to
/// give the assertion something to fail against would need a tool-gated
/// mock adapter that does not exist in-repo. What this test does earn is
/// the other half of the claim — that the PATCH landed and moved the live
/// client — which is asserted explicitly below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patching_the_default_mid_run_does_not_disturb_the_in_flight_run() {
    use alms_core::CreateRunRequest;
    use axum::Json;
    use axum::extract::State;

    let harness = llm_default_harness("openrouter", "z-ai/glm-5.2");
    let state = &harness.state;
    let agent_id = seed_llm_default_test_agent(state, "inherits-default", None, None);

    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;
    let mut session_rx = subscribe_session(state, session_id);

    let req = CreateRunRequest {
        session_id,
        ..serde_json::from_value(serde_json::json!({
            "session_id": session_id.0.to_string(),
            "input": { "type": "text", "text": "in flight" },
        }))
        .expect("CreateRunRequest must deserialize")
    };
    let (_status, resp) = crate::runs::lifecycle::create_run(State(state.clone()), Json(req))
        .await
        .expect("create_run should succeed");
    let run_id = resp.0.run_id;

    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), session_rx.recv())
            .await
            .expect("test must observe run_started within 10s")
            .expect("session sender must not close before run_started");
        if event.event_type == "run_started" {
            break;
        }
    }

    // The run has already resolved and committed its config. PATCH now.
    patch_server_default(
        state,
        serde_json::json!({ "model": "moonshotai/kimi-k2.5" }),
    )
    .await;

    let snapshot = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist")
        .resolved_config()
        .expect("resolved_config must be populated")
        .clone();
    assert_eq!(
        snapshot.model, "z-ai/glm-5.2",
        "the already-running run must keep the pair it resolved at start"
    );
    assert_eq!(
        state.llm.read().default_model(),
        "moonshotai/kimi-k2.5",
        "…while the live client HAS moved — otherwise the assertion above \
         would pass simply because the PATCH never landed"
    );
}

#[tokio::test]
async fn create_run_rejects_agent_session_mismatch() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state();
    let owner_id = AgentId::new();
    let other_id = AgentId::new();
    let session = state.session_manager.get_or_create(owner_id, "web");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(other_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) = crate::runs::lifecycle::create_run(State(state), Json(req)).await
    else {
        panic!("create_run should reject mismatched agent_id");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body.0["error"]["code"], "AGENT_SESSION_MISMATCH");
}

#[tokio::test]
async fn create_run_requires_agent_id_for_shared_session() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state();
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let session = state
        .session_manager
        .get_or_create_shared(session_id, "dm:alice:bob");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: None,
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) = crate::runs::lifecycle::create_run(State(state), Json(req)).await
    else {
        panic!("create_run should require agent_id for shared sessions");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body.0["error"]["code"], "AGENT_ID_REQUIRED");
}

#[tokio::test]
async fn create_run_resolves_per_agent_config_for_shared_session_via_requested_agent_id() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        model: Some("claude-sonnet-4-6".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("chamunchuk")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    // A non-DM shared session (agent_id = nil). This test originally used
    // a `dm:` session as its shared-session vehicle, but POST /runs on DM
    // sessions is rejected since #1156 (Option C — DM sessions are
    // agent-to-agent only). The behaviour under test — per-agent config
    // resolution via the request's `agent_id` on a shared session — is
    // independent of the context flavour.
    let session_id = SessionId::new();
    let session = state
        .session_manager
        .get_or_create_shared(session_id, "shared:config-resolution-test");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, resp) =
        match crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await {
            Ok(ok) => ok,
            Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
        };
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();

    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, resp.0.run_id);
    assert_eq!(runs[0].agent_id, agent_id);
    assert_eq!(runs[0].session_id, session.id);

    let base_agent_config = state.agent_config.read().clone();
    let secrets = state.secrets.read();
    let resolved = crate::configuration::resolve_agent_config(
        runs[0].agent_id,
        &state.session_manager,
        &base_agent_config,
        &state.llm.read().clone(),
        Some(&secrets),
    )
    .expect("success path: per-agent provider+model both supplied");
    assert_eq!(resolved.agent_name.as_deref(), Some("chamunchuk"));
    assert_eq!(resolved.llm.provider(), "anthropic");
    assert_eq!(resolved.llm.default_model(), "claude-sonnet-4-6");
}

// ---------------------------------------------------------------------------
// #863: MISSING_MODEL_AFTER_PROVIDER_SWITCH gateway-side 400
//
// `POST /runs` must reject requests where a per-agent provider override is
// set but no model was supplied at any layer. Pre-#863 the agent loop would
// send `model: ""` on the new provider's wire and surface as an opaque
// downstream 4xx (e.g. Anthropic 404 on `model: ""`). Post-#863 the gateway
// catches the deterministic config-shape failure mode at request time and
// returns a structured 400 BEFORE any LLM call.
// ---------------------------------------------------------------------------

/// Per-agent provider switch with NO model on any layer -> structured 400.
///
/// Server default is the test-default `LlmConfig::default()` (provider:
/// openrouter, default_model: z-ai/glm-5.2, providers: empty).
/// Agent record carries `provider: Some("anthropic")` and `model: None`,
/// and there is no `[llm.providers.anthropic]` entry to supply a model.
/// This is the canonical #863 leak shape — pre-fix the agent loop would
/// send Anthropic the OpenRouter server default; pre-#863 it would
/// then fall through the empty-clear and Anthropic would 404 on `model: ""`;
/// post-#863 the gateway returns 400 MISSING_MODEL_AFTER_PROVIDER_SWITCH
/// before any LLM call.
#[tokio::test]
async fn create_run_rejects_provider_switch_with_no_model_anywhere() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // The #863 trigger: provider override with NO model at any layer.
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("leaky-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject when no model is supplied at any layer (#863)");
    };

    // Acceptance criteria from issue #863:
    // 1. 400 status code BEFORE any LLM call
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 2. error_code == "MISSING_MODEL_AFTER_PROVIDER_SWITCH"
    assert_eq!(
        body.0["error_code"], "MISSING_MODEL_AFTER_PROVIDER_SWITCH",
        "body must carry the structured error_code so clients can branch on it"
    );
    // 3. Body carries agent_id + new_provider + prev_provider so the operator
    //    knows which agent to PATCH and which providers were involved.
    assert_eq!(
        body.0["agent_id"],
        agent_id.0.to_string(),
        "body must identify the agent so operators know which record to PATCH"
    );
    assert_eq!(
        body.0["new_provider"], "anthropic",
        "body must name the new provider the run was about to be sent to"
    );
    assert_eq!(
        body.0["prev_provider"], "openrouter",
        "body must name the previous (server-default) provider whose model leaked"
    );
    // 4. Human-readable message describes the failure mode.
    let message = body.0["message"]
        .as_str()
        .expect("message must be a string");
    assert!(
        message.contains("anthropic") && message.contains("openrouter"),
        "message must explain which provider override caused the failure: {message}"
    );

    // 5. No run was enqueued — the rejection happens BEFORE `insert_run`.
    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert!(
        runs.is_empty(),
        "no run should have been created when the gateway rejects pre-flight"
    );
}

/// Same provider on both sides -> no spurious 400.
///
/// Pin the no-spurious-400 invariant: when the agent record's provider
/// matches the server default (no actual switch), the leak guard must NOT
/// fire even if the agent has no per-agent `model`. The server-default
/// model reaches the wire as intended.
#[tokio::test]
async fn create_run_does_not_reject_when_provider_unchanged() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // Same provider as the server default (`openrouter` per
        // `LlmConfig::default()`). No switch -> no leak guard.
        provider: Some("openrouter".into()),
        ..AgentRecord::for_test("happy-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = crate::runs::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("same-provider config must NOT be rejected (#863)");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Per-agent provider switch WITH a per-agent model -> 200 (success path).
///
/// Pin the no-spurious-400 invariant: when the agent record carries an
/// in-namespace per-agent model, the run is accepted even though the
/// provider was switched.
#[tokio::test]
async fn create_run_accepts_provider_switch_with_per_agent_model() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // Per-agent model in the new provider's namespace -> success.
        model: Some("claude-sonnet-4-6".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("well-configured")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = crate::runs::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("provider switch with valid per-agent model must NOT be rejected");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// `execute_run`'s `match resolve_outcome` failure arm must mark the run
/// `Failed` with the structured `MissingModelAfterProviderSwitch` message
/// when invoked on a non-HTTP path (Telegram / scheduler / peer-DM /
/// subagent-completion triggers).
///
/// `create_run` runs `resolve_agent_config` as a pre-flight check and
/// rejects with `400 MISSING_MODEL_AFTER_PROVIDER_SWITCH` before
/// `insert_run`, so the HTTP path never reaches the in-loop resolve. The
/// non-HTTP triggers all enqueue runs that flow straight into
/// `execute_run`, where the resolve runs again under live locks. If a
/// future refactor "simplifies" the in-loop resolve back to `unwrap()`
/// (the symmetry argument: "create_run already pre-flighted, the second
/// resolve can't fail") the regression would be silent because the only
/// existing tests covering the missing-model path go through
/// `create_run`'s pre-flight rather than driving `execute_run` directly.
///
/// This test closes that coverage gap: it bypasses `create_run` (mirroring
/// what the Telegram / scheduler paths do — `insert_run` + `execute_run`
/// directly) and pins the three post-conditions of the failure arm:
///
/// 1. Terminal status is `Failed` — not `Running` (would mean the resolve
///    Err leaked through), not `Cancelled` (would mean the cancel-token
///    early-exit fired instead).
/// 2. The persisted `error` field carries the `Display`-formatted
///    structured message — `mark_run_as_failed(run_id, e.to_string())` —
///    so operators reading `GET /runs/{id}` can identify the failure mode
///    by `error_code`-substring grep, same as the HTTP 400 body's
///    `message` field.
/// 3. The `in_flight` counter returns to zero — the RAII
///    `_in_flight_guard` decrements correctly when the failure arm
///    early-returns.
#[tokio::test]
async fn execute_run_failure_arm_marks_run_failed_with_structured_error_on_provider_switch_without_model()
 {
    use alms_core::registry::AgentRecord;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Seed an agent with the canonical #863 trigger shape: provider
    // override to `anthropic` with NO model on any layer. Server default
    // is `openrouter` per `LlmConfig::default()`, so this is a real
    // cross-namespace switch and the in-loop `resolve_agent_config` will
    // fail with `MissingModelAfterProviderSwitch`.
    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("non-http-trigger-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    // Bypass `create_run` (which would reject pre-flight) and enqueue the
    // run directly — this is the shape the Telegram / scheduler / peer-DM
    // / subagent paths use.
    let run = Run::new(session_id, agent_id, "trigger #863 in execute_run".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Snapshot the in-flight counter before the call so we can pin its
    // post-call delta even if the test fixture changes the baseline.
    let in_flight_before = state.run_manager.in_flight_count();

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "non-http-context".to_string(),
            cancel_token,
            // is_peer_message=false / is_system_triggered=false matches
            // what a Telegram-driven run carries — the failure arm is
            // independent of these flags. Other non-HTTP callers
            // (notifications, subagent completions) use
            // is_system_triggered=true; pinning the false case here is
            // sufficient since the resolve happens before any flag-driven
            // branch.
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    // 1. Terminal status is Failed — the failure arm fired.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status(),
        RunStatus::Failed,
        "run must reach Failed via the resolve_outcome failure arm; got {:?}",
        final_run.status(),
    );

    // 2. The persisted error carries the `Display`-formatted structured
    //    message. We grep for the agent_id and both provider names rather
    //    than pinning the full string — the `Display` format itself is
    //    pinned by `test_missing_model_after_provider_switch_display_format`
    //    in `mod.rs`, and decoupling the assertions there from this one
    //    means a benign rephrasing of `Display` only updates one test.
    let error_msg = final_run
        .error
        .as_ref()
        .expect("Failed run must carry a structured error message");
    assert!(
        error_msg.contains(&agent_id.0.to_string()),
        "error must identify the agent_id (got: {error_msg})"
    );
    assert!(
        error_msg.contains("anthropic"),
        "error must name the new provider (got: {error_msg})"
    );
    assert!(
        error_msg.contains("openrouter"),
        "error must name the previous provider (got: {error_msg})"
    );

    // 3. The RAII `_in_flight_guard` must have decremented the counter
    //    back to its pre-call value when the failure arm returned. A
    //    future refactor that hoists the resolve out of the
    //    `track_in_flight` window would silently regress the drain
    //    semantics; pinning the delta catches that.
    assert_eq!(
        state.run_manager.in_flight_count(),
        in_flight_before,
        "in_flight counter must return to baseline ({}) after the failure arm; got {}",
        in_flight_before,
        state.run_manager.in_flight_count(),
    );

    shutdown_token.cancel();
}

/// When the user sends a message to an agent that is already *running* another
/// task (but nothing is queued behind it), `queued_behind` in the run_created
/// SSE event must be >= 1 so the UI shows "Queued -- waiting for agent..."
/// rather than a misleading "Thinking...".
///
/// Reproduces the bug where `SessionQueue::pending_count` returns 0 because
/// the currently-running item has already been dequeued, leaving no visible
/// signal that the new run is actually queued.
#[tokio::test]
async fn create_run_reports_queued_behind_when_agent_is_running() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;

    // Simulate an already-running run on this agent.
    let running_run = Run::new(session_id, agent_id, "prior task".into());
    let running_run_id = running_run.run_id;
    let _ = state.run_manager.insert_run(running_run);
    state.run_manager.mark_run_as_running(running_run_id);

    // Subscribe to session events so we can inspect the run_created payload.
    let mut rx = subscribe_session(&state, session_id);

    let req = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "second message".into(),
        },
    };

    match crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await {
        Ok(_) => {}
        Err((code, body)) => panic!("create_run failed: status={code:?} body={:?}", body.0),
    }

    // Cancel shutdown so the enqueued execute_run task (spawned by create_run)
    // early-exits without trying to call a real LLM.  Run-level events emitted
    // after cancellation are irrelevant -- we only inspect the run_created
    // event emitted synchronously during create_run.
    shutdown_token.cancel();

    // Give the SSE fan-out a moment to land.
    tokio::task::yield_now().await;

    let events = drain_events(&mut rx);
    let run_created = events
        .iter()
        .find(|e| e.event_type == "run_created")
        .expect("run_created event should be emitted");

    let queued_behind = run_created
        .data
        .get("queued_behind")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        queued_behind >= 1,
        "queued_behind should be >= 1 when the agent is already running another run; got {queued_behind}",
    );
}

#[tokio::test]
async fn run_panic_is_reconciled_to_failed_and_cleans_activity_state() {
    let (state, _shutdown, _completion_rx, _trigger_rx, _dm_rx) = test_app_state();
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let run = Run::new(session_id, agent_id, "panic".to_string());
    let run_id = run.run_id;
    let cancel_token = CancellationToken::new();
    let _ = state.run_manager.insert_run(run);
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut activity = subscribe_agent(&state, agent_id);

    crate::runs::lifecycle::execute_run_guarded_future(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: "panic".to_string(),
            context_id: "web".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
        async { panic!("synthetic queued work panic") },
    )
    .await;

    let failed = state.run_manager.get_run(run_id).expect("run retained");
    assert_eq!(failed.status(), RunStatus::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("Run panicked during execution")
    );
    assert!(
        !state.run_manager.cancel_run(run_id),
        "panic reconciliation must remove the cancellation token"
    );
    assert!(!state.run_manager.has_active_runs(session_id));

    let ended = activity.try_recv().expect("activity-ended event");
    assert_eq!(ended.event_type, "session_activity_ended");
    assert_eq!(
        ended.data["run_id"].as_str(),
        Some(run_id.0.to_string().as_str())
    );
}

#[tokio::test]
async fn late_cleanup_panic_does_not_reclassify_a_completed_run_as_failed() {
    let (state, _shutdown, _completion_rx, _trigger_rx, _dm_rx) = test_app_state();
    let agent_id = AgentId::new();
    let session_id = SessionId::new();
    let run = Run::new(session_id, agent_id, "complete".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);
    assert!(state.run_manager.mark_run_as_completed(
        run_id,
        "done".to_string(),
        Default::default()
    ));

    crate::runs::lifecycle::execute_run_guarded_future(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: "complete".to_string(),
            context_id: "web".to_string(),
            cancel_token: CancellationToken::new(),
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
        async { panic!("synthetic late cleanup panic") },
    )
    .await;

    let completed = state.run_manager.get_run(run_id).expect("run retained");
    assert_eq!(completed.status(), RunStatus::Completed);
    assert_eq!(completed.output.as_deref(), Some("done"));
    assert!(completed.error.is_none());
}

#[tokio::test]
async fn full_agent_queue_rejects_before_run_side_effects() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "web");
    let session_id = session.id;
    let mut events = subscribe_session(&state, session_id);

    let held: Vec<_> = (0..crate::session_queue::MAX_PENDING_PER_KEY)
        .map(|_| {
            state
                .agent_queue
                .try_reserve(agent_id)
                .expect("fill per-agent capacity")
        })
        .collect();
    let messages_before = state
        .session_manager
        .get_history(session_id)
        .expect("session history")
        .len();

    let request = CreateRunRequest {
        session_id,
        agent_id: None,
        input: RunInput::Text {
            text: "must be rejected cleanly".into(),
        },
    };
    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(request)).await
    else {
        panic!("saturated queue must reject the request");
    };

    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body.0["error_code"], "AGENT_QUEUE_FULL");
    assert!(state.run_manager.list_by_session(session_id, 10).is_empty());
    assert_eq!(
        state
            .session_manager
            .get_history(session_id)
            .expect("session history")
            .len(),
        messages_before
    );
    assert!(
        drain_events(&mut events)
            .iter()
            .all(|event| event.event_type != "run_created")
    );

    drop(held);
    shutdown_token.cancel();
}

#[test]
fn queue_admission_429_includes_retry_after_header() {
    let response =
        axum::response::IntoResponse::into_response(crate::runs::lifecycle::queue_admission_error(
            crate::session_queue::AdmissionError::PerKeyFull,
        ));
    assert_eq!(
        response.headers().get(axum::http::header::RETRY_AFTER),
        Some(&axum::http::HeaderValue::from_static("1"))
    );
}

// ---------------------------------------------------------------------------
// #919: per-run token-budget validation against provider context window
//
// `POST /runs` must reject requests where the resolved per-agent
// `(provider, model, max_input_tokens, max_tokens)` quadruple overshoots
// the provider's published context window. The validator runs inside
// `pre_flight_token_budget` after `resolve_agent_config` succeeds, so a
// per-agent provider/model override that lands on a too-small cap is
// caught BEFORE the run is enqueued.
// ---------------------------------------------------------------------------

// The `ALMS_LLM_BUDGET_VALIDATION` env-var mutex and RAII guard live in
// `crate::test_env_locks` so they are shared with `settings.rs::tests`
// (which also exercises the validator on the PATCH path). Both files are
// compiled into the same `cargo test -p alms-gateway` process — without a
// single shared mutex, a strict-mode PATCH test could race a concurrent
// warn-mode `POST /runs` test on the same env var. The lock guards a
// single var-set (`ALMS_LLM_BUDGET_VALIDATION` only) and is disjoint by
// construction from any other env-var mutex in the workspace.
/// Per-agent override pinning provider+model whose published context
/// window is smaller than `max_input_tokens + max_tokens` -> structured
/// 400 INVALID_TOKEN_BUDGET_FOR_PROVIDER.
///
/// Setup:
/// - Server-default `[context].max_input_tokens` is 128_000 (default).
/// - `agent.max_tokens` defaults to 32_000 (DEFAULT_AGENT_MAX_TOKENS).
/// - Per-agent override pins provider=`anthropic` and model=`claude-haiku-4-5`,
///   whose 200K cap fits the default 128K + 32K = 160K budget.
/// - Bumping `[context].max_input_tokens` to 250_000 pushes the effective
///   total to 282_000, which overshoots the 200K cap → validator fires.
///
/// Note: post-2026-05-09 verification round Opus 4.7 / Sonnet 4.6 moved to
/// 1M caps. Haiku 4.5 stays at 200K and is the natural overshoot fixture.
#[tokio::test]
async fn create_run_rejects_per_agent_override_that_blows_provider_cap() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    // Pin strict mode for this test so a concurrent warn-mode test
    // can't make us silently accept the overbudget config.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, _shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Bump the server-level input budget so 250K input + 32K output
    // overshoots Haiku 4.5's 200K cap.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // Pin a model whose 200K cap is smaller than the 282K effective
        // total once we bump max_input_tokens above.
        model: Some("claude-haiku-4-5".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("overbudget-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!(
            "create_run must reject when the resolved budget overshoots the provider cap (#919)"
        );
    };

    // 1. 400 status code BEFORE any LLM call.
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 2. Structured error code so clients can branch on it.
    assert_eq!(
        body.0["error_code"], "INVALID_TOKEN_BUDGET_FOR_PROVIDER",
        "body must carry the structured error_code so clients can branch on it"
    );
    // 3. Body carries every datum the operator needs to fix the config.
    assert_eq!(body.0["agent_id"], agent_id.0.to_string());
    assert_eq!(body.0["provider"], "anthropic");
    assert_eq!(body.0["model"], "claude-haiku-4-5");
    assert_eq!(body.0["max_input_tokens"], 250_000);
    assert_eq!(body.0["max_tokens"], 32_000);
    assert_eq!(body.0["effective_total"], 282_000);
    assert_eq!(body.0["provider_cap"], 200_000);
    // 4. Human-readable message points at both knobs and the cap.
    let message = body.0["message"]
        .as_str()
        .expect("message must be a string");
    assert!(
        message.contains("max_input_tokens") && message.contains("max_tokens"),
        "message must name both budget knobs: {message}"
    );
    assert!(
        message.contains("anthropic") && message.contains("claude-haiku-4-5"),
        "message must identify the provider and resolved model: {message}"
    );
    // 5. No run was enqueued.
    let runs = state.run_manager.list_by_agent(agent_id, 10);
    assert!(
        runs.is_empty(),
        "no run should have been created when the gateway rejects pre-flight"
    );
}

/// Same overbudget config + `ALMS_LLM_BUDGET_VALIDATION=warn` -> run is
/// accepted (the env var downgrades the strict reject to a structured
/// WARN log).
///
/// Pins the warn opt-out behaviour for the per-run path. Uses a process-
/// global env-var mutex via `parking_lot` to avoid races with parallel
/// tests that read the same env var.
#[tokio::test]
async fn create_run_warn_mode_accepts_overbudget_config() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    // Pin warn mode for this test, holding the global env-var lock so
    // concurrent strict-mode tests can't see the warn value.
    let _env = BudgetValidationEnvGuard::set("warn");

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        model: Some("claude-haiku-4-5".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("overbudget-warn-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = crate::runs::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("warn mode must accept overshooting configs (#919)");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Per-agent override that resolves to a model whose `(provider, model)`
/// pair the budget table doesn't know about -> run is accepted regardless
/// of size. Mirrors the unknown-pair-skips contract pinned in the
/// alms-core unit tests, exercised end-to-end through `pre_flight_token_budget`.
#[tokio::test]
async fn create_run_accepts_unknown_model_regardless_of_budget() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    // Pin strict mode — the unknown-pair branch must skip the check
    // regardless of mode, but we hold the lock so a concurrent warn-mode
    // test doesn't make this assertion vacuous.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    // 10M input + 32K output overshoots every published cap, but with an
    // unknown model the validator returns Ok(()) and the run proceeds.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 10_000_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // Per-agent provider override to anthropic with a model NOT in
        // the table — falls through to None at lookup time, validator
        // skips silently.
        model: Some("claude-2.1".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("unknown-model-agent")
    };
    // Bump session storage to match so the cross-section validator is
    // satisfied.
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = crate::runs::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("unknown (provider, model) pair must skip the budget check");
    assert_eq!(status, axum::http::StatusCode::CREATED);
    shutdown_token.cancel();
}

/// Mock mode bypasses the per-run pre-flight budget guard, mirroring the
/// boot-time skip in `AlmsConfig::validate` (Codex P2 #1 follow-up on PR
/// #1020). A mock-mode run with an intentionally-overshooting budget for
/// a known `(provider, model)` pair must land cleanly — the mock client
/// will not call the real provider, so refusing it is a false positive
/// that blocks otherwise-valid local/dev test setups.
#[tokio::test]
async fn create_run_mock_mode_bypasses_budget_validation() {
    use alms_core::registry::AgentRecord;
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    // Pin strict mode — the mock-mode bypass must take effect regardless
    // of `ALMS_LLM_BUDGET_VALIDATION`. Hold the global env-var lock so a
    // concurrent warn-mode test can't make this assertion vacuous.
    let _env = BudgetValidationEnvGuard::unset();

    // Build state with a mock-mode LLM client. We can't mutate
    // `state.llm.config.mock` after construction (no public setter — the
    // flag travels through `LlmClient::new`), so we route through a
    // `GatewayConfig` whose `llm_config.mock = true`.
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = crate::gateway::GatewayConfig {
        db_path: Some(":memory:".to_string()),
        llm_config,
        ..crate::gateway::GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, _cr) = mpsc::unbounded_channel();
    let (trigger_tx, _tr) = mpsc::channel(8);
    let (dm_event_tx, _dr) = mpsc::channel(8);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();

    // 250K input + 32K output = 282K — overshoots Haiku 4.5's 200K cap.
    // Without the mock-mode bypass the per-run validator would reject
    // this with `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER`.
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // A known table-row whose 200K cap is smaller than the 282K
        // effective total — without the mock bypass the validator would
        // fire here.
        model: Some("claude-haiku-4-5".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("mock-mode-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state.session_manager.get_or_create(agent_id, "web");
    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "hello".into(),
        },
    };

    let (status, _resp) = crate::runs::lifecycle::create_run(State(state), Json(req))
        .await
        .expect("mock-mode run with overshooting budget must be accepted (#1020 P2 #1)");
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "mock mode must bypass token-budget pre-flight, mirroring `AlmsConfig::validate`"
    );
    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #919: per-run token-budget validation INSIDE `execute_run` (non-HTTP path)
//
// `pre_flight_token_budget` originally only fired on the HTTP `POST /runs`
// path. Runs created via `enqueue_triggered_run` (peer DMs, scheduler
// triggers, notification runs, subagent completion runs) skip `create_run`
// entirely and land directly in `execute_run`, so the create-time guard
// did not protect them — the exact opaque-downstream-4xx symptom the
// validator is meant to prevent. Codex P2 follow-up on PR #1020 moved the
// guard into `execute_run` so every run-creation path inherits it.
// ---------------------------------------------------------------------------

/// Non-HTTP path: bypass `create_run` and call `execute_run` directly with
/// an over-budget agent config. `execute_run` must reject the run before
/// any LLM call by marking it `Failed` with the structured
/// `INVALID_TOKEN_BUDGET_FOR_PROVIDER` message.
///
/// Setup mirrors `create_run_rejects_per_agent_override_that_blows_provider_cap`
/// — same overbudget shape, same expected message structure. The
/// distinction is the call site: this test enqueues the run shape used by
/// the scheduler / Telegram / peer-DM / subagent completion paths and
/// confirms the `execute_run`-side guard fires identically. Pins the
/// "queued runs whose agent config changed after `POST /runs`" leak too,
/// because the second resolve inside `execute_run` is what catches both
/// the never-validated and the re-validated case.
#[tokio::test]
async fn execute_run_rejects_overbudget_resolved_config_on_non_http_path() {
    use alms_core::registry::AgentRecord;

    // Pin strict mode so a concurrent warn-mode test can't make us silently
    // accept the overbudget config.
    let _env = BudgetValidationEnvGuard::unset();

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();
    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        // 250K + 32K = 282K overshoots Haiku 4.5's 200K cap — same fixture
        // as the create_run-side test, exercised from the non-HTTP path.
        model: Some("claude-haiku-4-5".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("overbudget-non-http-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    // Bypass `create_run` (which would reject pre-flight on the HTTP path)
    // and enqueue the run directly — this is the shape the Telegram /
    // scheduler / peer-DM / subagent paths use.
    let run = Run::new(session_id, agent_id, "over-budget non-http trigger".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut activity_feed = subscribe_activity(&state);

    let in_flight_before = state.run_manager.in_flight_count();

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "non-http-context".to_string(),
            cancel_token,
            // System-triggered shape (scheduler / notification / subagent
            // completion). The budget check is independent of these flags
            // — it runs immediately after `resolve_agent_config` succeeds,
            // before the posture / bootstrap / debug-mode transforms.
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;
    // PR #1220 review regression: the budget-rejection arm must emit its
    // terminal activity after flipping Queued -> Failed. Otherwise the
    // authoritative snapshot still sees this run as active and leaves the
    // sidebar dot stuck.
    let activity_events = drain_activity_events(&mut activity_feed);
    let run_id_text = run_id.0.to_string();
    let ended = activity_events
        .iter()
        .find(|event| {
            event.event_type == "session_activity_ended"
                && event.data.get("run_id").and_then(|value| value.as_str())
                    == Some(run_id_text.as_str())
        })
        .expect("budget-rejected run must publish terminal session activity");
    assert_eq!(
        ended
            .data
            .get("has_active_run")
            .and_then(|value| value.as_bool()),
        Some(false),
        "budget-rejected run terminal activity must carry the settled false predicate",
    );

    // 1. Terminal status is Failed — the budget arm fired before any LLM
    //    call. NOT Cancelled (would mean the cancel-token early-exit fired
    //    instead) and NOT Completed (would mean the guard didn't trip).
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    assert_eq!(
        final_run.status(),
        RunStatus::Failed,
        "run must reach Failed via the budget arm; got {:?}",
        final_run.status(),
    );

    // 2. The persisted error carries the structured message — same shape
    //    operators see on `GET /runs/{id}` for the HTTP path's 400 body.
    let error_msg = final_run
        .error
        .as_ref()
        .expect("Failed run must carry a structured error message");
    assert!(
        error_msg.contains("anthropic") && error_msg.contains("claude-haiku-4-5"),
        "error must name the provider and resolved model (got: {error_msg})"
    );
    assert!(
        error_msg.contains("max_input_tokens") && error_msg.contains("max_tokens"),
        "error must name both budget knobs (got: {error_msg})"
    );
    assert!(
        error_msg.contains("200000") || error_msg.contains("200_000"),
        "error must name the provider cap (got: {error_msg})"
    );

    // 3. The RAII `_in_flight_guard` decrements back to baseline on the
    //    new failure arm too, mirroring the contract pinned in
    //    `execute_run_failure_arm_marks_run_failed_with_structured_error_on_provider_switch_without_model`.
    assert_eq!(
        state.run_manager.in_flight_count(),
        in_flight_before,
        "in_flight counter must return to baseline ({}) after the budget failure arm; got {}",
        in_flight_before,
        state.run_manager.in_flight_count(),
    );

    // 4. The run never transitioned to `Running` — the guard fires
    //    BEFORE `mark_run_as_running_with_config`, so the resolved-config
    //    snapshot is never persisted and the run isn't visible in the
    //    running set.
    assert!(
        final_run.resolved_config().is_none(),
        "Failed-before-running runs must not have a resolved_config snapshot; got {:?}",
        final_run.resolved_config(),
    );

    shutdown_token.cancel();
}

/// Mock-mode skip on the non-HTTP path: mirrors the create_run-side mock
/// bypass test. When the LLM client is in mock mode, `execute_run`'s
/// budget guard must skip regardless of strict-mode env var.
///
/// (We don't test the warn-mode opt-out on the non-HTTP path explicitly —
/// `evaluate_pre_flight_token_budget` is the shared helper exercised by
/// both surfaces, so the strict/warn dispatch is pinned by the HTTP-side
/// `create_run_warn_mode_accepts_overbudget_config` test. The mock-mode
/// branch lives at the top of the helper and short-circuits before the
/// strict/warn split, so we pin it on both surfaces.)
#[tokio::test]
async fn execute_run_mock_mode_skips_budget_validation_on_non_http_path() {
    use alms_core::registry::AgentRecord;

    let _env = BudgetValidationEnvGuard::unset();

    // Build state with a mock-mode LLM client.
    let llm_config = alms_runtime::LlmConfig {
        mock: true,
        ..alms_runtime::LlmConfig::default()
    };
    let gateway_config = crate::gateway::GatewayConfig {
        db_path: Some(":memory:".to_string()),
        llm_config,
        ..crate::gateway::GatewayConfig::default()
    };
    let gateway = crate::gateway::Gateway::new(gateway_config).unwrap();
    let scheduler = std::sync::Arc::new(alms_runtime::Scheduler::new());
    let shutdown_token = CancellationToken::new();
    let (completion_tx, _cr) = mpsc::unbounded_channel();
    let (trigger_tx, _tr) = mpsc::channel(8);
    let (dm_event_tx, _dr) = mpsc::channel(8);
    let state = AppState::new(
        gateway,
        scheduler,
        shutdown_token.clone(),
        completion_tx,
        trigger_tx,
        dm_event_tx,
    )
    .unwrap();

    {
        let mut cfg = state.agent_config.write();
        cfg.context_config.max_input_tokens = 250_000;
        cfg.max_tokens = 32_000;
    }

    let agent_id = AgentId::new();
    let agent = AgentRecord {
        id: agent_id,
        model: Some("claude-haiku-4-5".into()),
        provider: Some("anthropic".into()),
        ..AgentRecord::for_test("mock-mode-non-http-agent")
    };
    state
        .session_manager
        .store()
        .expect("SQLite-backed state should have a store")
        .create_agent(&agent)
        .expect("agent seed should succeed");

    let session = state
        .session_manager
        .get_or_create(agent_id, "non-http-context");
    let session_id = session.id;

    let run = Run::new(
        session_id,
        agent_id,
        "mock-mode overbudget non-http trigger".into(),
    );
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "non-http-context".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    // The run must NOT carry the budget-failure signature — mock mode
    // bypassed the guard, mirroring `AlmsConfig::validate`'s boot-time
    // skip and the HTTP-path test.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after execute_run returns");
    if let Some(error_msg) = final_run.error.as_ref() {
        assert!(
            !error_msg.contains("context.max_input_tokens"),
            "mock mode must NOT produce the budget-failure signature; got: {error_msg}"
        );
    }

    shutdown_token.cancel();
}
