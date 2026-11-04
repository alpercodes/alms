// SPDX-License-Identifier: Apache-2.0

//! What a run cancelled by the gateway's shutdown leaves behind, against what
//! an operator's cancel leaves behind. Each test holds a real run in flight on
//! a scripted LLM, because the record is written only once the loop unwinds.

use super::{marker_of_type, seed_alice_bob};
use crate::server::{AppState, begin_shutdown};
use crate::test_support::AppStateWithChannels;
use alms_coordinator::message_bus::{MessageSource, RunTrigger};
use alms_core::{AgentId, CreateRunRequest, Run, RunId, RunInput, RunStatus, SessionId};
use alms_session::{Content, Message, Role};
use alms_test_support::{Canned, ScriptedLlm};
use alms_tools::MessageSender;
use alms_tools::message_sender::ConversationEndReason;
use axum::Json;
use axum::extract::{Path, State};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const USER_CANCEL_RECORD: &str = "[Run cancelled by user]";
const SHUTDOWN_RECORD: &str = "[Run interrupted by gateway shutdown]";

/// A gateway whose LLM accepts a request and then never answers it, so a run
/// stays in flight until something cancels it.
struct Gateway {
    channels: AppStateWithChannels,
    llm: ScriptedLlm,
    _workspace: tempfile::TempDir,
}

impl Gateway {
    async fn start() -> Self {
        let llm =
            ScriptedLlm::always(Canned::json(200, "{}").after(Duration::from_secs(3600))).await;
        let workspace = tempfile::tempdir().unwrap();
        let channels = crate::test_support::TestAppState::new()
            .in_memory_sqlite()
            .workspace_dir(workspace.path())
            .llm_config(alms_runtime::LlmConfig {
                base_url: llm.base_url(),
                api_key: "test-key".to_string(),
                default_model: "test-model".to_string(),
                timeout_secs: 3600,
                stream_chunk_timeout_secs: 3600,
                ..alms_runtime::LlmConfig::default()
            })
            .build_with_channels();
        Self {
            channels,
            llm,
            _workspace: workspace,
        }
    }

    fn state(&self) -> AppState {
        self.channels.0.clone()
    }

    fn shutdown(&self) {
        let state = self.state();
        begin_shutdown(&self.channels.1, &state.run_manager);
    }

    /// Wait until `run_id` is inside its LLM call.
    async fn wait_in_flight(&self, run_id: RunId) {
        wait_until("the run to reach its LLM call", || {
            self.llm.calls() > 0
                && self.state().run_manager.get_run(run_id).unwrap().status() == RunStatus::Running
        })
        .await;
    }
}

async fn wait_until(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// `execute_run` removes the cancel token on its way out, after the terminal
/// arm has done its DM bookkeeping.
async fn wait_finished(state: &AppState, run_id: RunId) {
    wait_until("the run to finish unwinding", || {
        !state.run_manager.has_cancel_token(run_id)
    })
    .await;
}

fn text_of(message: &Message) -> &str {
    match &message.content {
        Content::Text(text) => text,
        _ => "",
    }
}

/// Every text row in the session, so a test can see the whole record.
fn texts(state: &AppState, session_id: SessionId) -> Vec<String> {
    state
        .session_manager
        .get_history(session_id)
        .unwrap()
        .iter()
        .map(|message| text_of(message).to_string())
        .collect()
}

/// The `reason` on the session's `dm_ended` row, if it has one.
fn dm_ended_reason(state: &AppState, session_id: SessionId) -> Option<String> {
    state
        .session_manager
        .get_history(session_id)
        .unwrap()
        .iter()
        .filter_map(|message| message.metadata.as_ref())
        .find(|metadata| metadata.get("message_type").and_then(|v| v.as_str()) == Some("dm_ended"))
        .and_then(|metadata| metadata.get("reason")?.as_str().map(String::from))
}

async fn start_web_run(state: &AppState, agent_id: AgentId) -> (RunId, SessionId) {
    let session_id = state.session_manager.get_or_create(agent_id, "web").id;
    let (_, Json(created)) = crate::runs::lifecycle::create_run(
        State(state.clone()),
        Json(CreateRunRequest {
            session_id,
            agent_id: None,
            input: RunInput::Text {
                text: "do something slow".into(),
            },
        }),
    )
    .await
    .map_err(|error| error.0)
    .unwrap();
    (created.run_id, session_id)
}

/// Alice opens a DM from her web chat, and bob's run on it goes in flight.
/// Returns the run, the DM session and alice's chat.
async fn start_dm_run(
    gateway: &mut Gateway,
    alice_id: AgentId,
    bob_id: AgentId,
) -> (RunId, SessionId, SessionId) {
    let state = gateway.state();
    let alice_web = state.session_manager.get_or_create(alice_id, "web").id;
    state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", Some(alice_web))
        .await
        .unwrap();
    let trigger = gateway.channels.3.try_recv().unwrap();
    let (tx, rx) = mpsc::channel(8);
    tx.send(trigger).await.unwrap();
    drop(tx);
    crate::runs::notifications::run_trigger_loop(rx, state.clone()).await;

    let dm_session = SessionId::deterministic_dm("alice", "bob");
    let run_id = state.run_manager.list_by_session(dm_session, 10)[0].run_id;
    gateway.wait_in_flight(run_id).await;
    (run_id, dm_session, alice_web)
}

fn received_triggers(gateway: &mut Gateway) -> Vec<RunTrigger> {
    let mut triggers = Vec::new();
    while let Ok(trigger) = gateway.channels.3.try_recv() {
        triggers.push(trigger);
    }
    triggers
}

fn end_reason(trigger: &RunTrigger) -> &ConversationEndReason {
    match &trigger.source {
        MessageSource::ConversationEnded { reason, .. } => reason,
        other => panic!("expected ConversationEnded, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_of_a_web_run_does_not_claim_a_user_cancelled_it() {
    let gateway = Gateway::start().await;
    let state = gateway.state();
    let (run_id, session_id) = start_web_run(&state, AgentId::new()).await;
    gateway.wait_in_flight(run_id).await;

    gateway.shutdown();
    wait_finished(&state, run_id).await;

    let history = state.session_manager.get_history(session_id).unwrap();
    let record = history.last().unwrap();
    assert_eq!(text_of(record), SHUTDOWN_RECORD, "history: {history:?}");
    assert_eq!(record.role, Role::Assistant);
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_cancel_of_a_web_run_keeps_its_record() {
    let gateway = Gateway::start().await;
    let state = gateway.state();
    let (run_id, session_id) = start_web_run(&state, AgentId::new()).await;
    gateway.wait_in_flight(run_id).await;

    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .map_err(|error| error.0)
        .unwrap();
    wait_finished(&state, run_id).await;

    let history = state.session_manager.get_history(session_id).unwrap();
    let record = history.last().unwrap();
    assert_eq!(text_of(record), USER_CANCEL_RECORD, "history: {history:?}");
    assert_eq!(record.role, Role::Assistant);
    gateway.channels.1.cancel();
}

/// The drain held open by a slow request: `run_trigger_loop` outlives the
/// run, so the DM's end is fully written. Nothing on it, on either agent's
/// side, may say a user cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_of_a_dm_run_with_the_trigger_loop_alive_blames_no_user() {
    let mut gateway = Gateway::start().await;
    let state = gateway.state();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let (run_id, dm_session, alice_web) = start_dm_run(&mut gateway, alice_id, bob_id).await;

    gateway.shutdown();
    wait_finished(&state, run_id).await;

    let history = state.session_manager.get_history(dm_session).unwrap();
    let record = history
        .iter()
        .find(|message| text_of(message) == SHUTDOWN_RECORD)
        .unwrap_or_else(|| panic!("no shutdown record in {history:?}"));
    assert_eq!(record.role, Role::User, "DM rows stay Role::User");
    assert_eq!(
        record.metadata.as_ref().unwrap()["from_agent"],
        "bob",
        "the record keeps its DM shape"
    );
    assert_eq!(
        dm_ended_reason(&state, dm_session).as_deref(),
        Some("errored")
    );

    let triggers = received_triggers(&mut gateway);
    assert!(!triggers.is_empty(), "the end must reach the trigger loop");
    for trigger in &triggers {
        assert_eq!(
            end_reason(trigger),
            &ConversationEndReason::Errored {
                message: "the gateway shut down".to_string(),
                interrupted: true,
            }
        );
    }

    // The operator's banner on alice's chat, as the live loop writes it.
    let (tx, rx) = mpsc::channel(8);
    for trigger in triggers {
        tx.send(trigger).await.unwrap();
    }
    drop(tx);
    crate::runs::notifications::run_trigger_loop(rx, state.clone()).await;
    let banner = marker_of_type(&state, alice_web, "dm_ended_notification")
        .expect("alice's chat gets the banner marker");
    assert_eq!(
        text_of(&banner),
        "[DM conversation ended] Conversation with bob ended \
         (run failed: the gateway shut down)."
    );

    for session_id in [dm_session, alice_web] {
        let rows = texts(&state, session_id);
        assert!(
            !rows.iter().any(|row| row.contains("by user")),
            "no row may blame a user: {rows:?}"
        );
    }
}

/// The usual shutdown order: `run_trigger_loop` is aborted before the run
/// unwinds, so `end_conversation` fails and only the runtime's record is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_of_a_dm_run_with_the_trigger_loop_gone_blames_no_user() {
    let mut gateway = Gateway::start().await;
    let state = gateway.state();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let (run_id, dm_session, _) = start_dm_run(&mut gateway, alice_id, bob_id).await;
    gateway.channels.3.close();

    gateway.shutdown();
    wait_finished(&state, run_id).await;

    let rows = texts(&state, dm_session);
    assert!(rows.iter().any(|row| row == SHUTDOWN_RECORD), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("by user")), "{rows:?}");
    assert_eq!(dm_ended_reason(&state, dm_session), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_cancel_of_a_dm_run_keeps_its_record_and_reason() {
    let mut gateway = Gateway::start().await;
    let state = gateway.state();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let (run_id, dm_session, _) = start_dm_run(&mut gateway, alice_id, bob_id).await;

    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .map_err(|error| error.0)
        .unwrap();
    wait_finished(&state, run_id).await;

    let rows = texts(&state, dm_session);
    assert!(rows.iter().any(|row| row == USER_CANCEL_RECORD), "{rows:?}");
    assert_eq!(
        dm_ended_reason(&state, dm_session).as_deref(),
        Some("user_cancelled")
    );
    for trigger in received_triggers(&mut gateway) {
        assert_eq!(end_reason(&trigger), &ConversationEndReason::UserCancelled);
    }
    gateway.channels.1.cancel();
}

/// A DM run shutdown catches while it is still queued never starts its loop,
/// so it goes through the early exit and not the terminal arms.
#[tokio::test]
async fn shutdown_of_a_queued_dm_run_blames_no_user() {
    let mut gateway = Gateway::start().await;
    let state = gateway.state();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = received_triggers(&mut gateway);

    let dm_session = SessionId::deterministic_dm("alice", "bob");
    let run = Run::new(dm_session, bob_id, "ping".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    gateway.shutdown();
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id: dm_session,
            agent_id: bob_id,
            input: run.input,
            context_id: "dm:alice:bob".to_string(),
            cancel_token,
            is_peer_message: true,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    assert_eq!(
        dm_ended_reason(&state, dm_session).as_deref(),
        Some("errored")
    );
    assert_eq!(gateway.llm.calls(), 0, "the run must never reach the LLM");
}
