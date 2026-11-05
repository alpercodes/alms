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
const UNROUTABLE_APPROVAL_RECORD: &str = "[Run stopped: tool approval could not be requested]";

/// A gateway whose LLM accepts a request and then never answers it, so a run
/// stays in flight until something cancels it.
struct Gateway {
    channels: AppStateWithChannels,
    llm: ScriptedLlm,
    _workspace: tempfile::TempDir,
}

impl Gateway {
    async fn start() -> Self {
        Self::with_llm(
            ScriptedLlm::always(Canned::json(200, "{}").after(Duration::from_secs(3600))).await,
        )
    }

    fn with_llm(llm: ScriptedLlm) -> Self {
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

/// A user's Stop on a queued DM run is already accepted when the queue reaches
/// the item, however long that takes. A shutdown starting in between must not
/// take the reason away from them.
#[tokio::test]
async fn user_stop_of_a_queued_dm_run_keeps_its_reason_during_shutdown() {
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

    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .map_err(|error| error.0)
        .unwrap();
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
        Some("user_cancelled")
    );
}

/// One streamed turn asking for a shell call, which a guarded agent has to get
/// approved.
fn shell_call_turn() -> String {
    let chunk = serde_json::json!({
        "id": "x",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "shell",
                        "arguments": "{\"command\":\"echo hi\"}",
                    },
                }],
            },
            "finish_reason": "tool_calls",
        }],
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Runs a guarded named subagent up to its first tool call, which nobody can
/// approve, and returns its session.
///
/// The denial fires the subagent's own token inside the loop's poll, so the
/// biased token arm in `run_subagent` can't drop the loop first, and
/// `finish_run` always writes its record.
async fn run_guarded_subagent(
    state: &AppState,
    parent_events: Option<std::sync::Arc<dyn alms_tools::EventForwarder>>,
) -> SessionId {
    let mut guard = alms_core::registry::AgentRecord::for_test("guard");
    guard.posture = Some("guarded".to_string());
    let name = guard.name.clone();
    state
        .session_manager
        .store()
        .unwrap()
        .create_agent(&guard)
        .unwrap();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "web")
        .id;
    let (_, session_id) = state
        .coordinator
        .spawn_subagent(
            alms_coordinator::SubagentRequest {
                task: "needs approval".to_string(),
                parent_session,
                parent_agent_id,
                parent_run_id: None,
                subagent_name: Some(name),
                parent_tool_invocation_id: None,
            },
            parent_events,
            false,
            None,
        )
        .await
        .unwrap();
    wait_until("the subagent's record", || {
        texts(state, session_id)
            .iter()
            .any(|row| row.starts_with("[Run "))
    })
    .await;
    session_id
}

/// The denied call's tool row, as the model reads it on a later turn.
fn denied_tool_result(state: &AppState, session_id: SessionId) -> serde_json::Value {
    state
        .session_manager
        .get_history(session_id)
        .unwrap()
        .into_iter()
        .find_map(|message| match message.content {
            Content::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .expect("the denied call leaves a tool row")
}

/// The reason on the session's one `Deny` audit row.
fn denial_audit_reason(state: &AppState, session_id: SessionId) -> String {
    let audit = state.session_manager.get_audit(session_id).unwrap();
    let denials: Vec<_> = audit
        .iter()
        .filter(|event| matches!(event.decision, alms_core::AuditDecision::Deny))
        .collect();
    assert_eq!(denials.len(), 1, "{audit:?}");
    denials[0].error.clone().unwrap_or_default()
}

/// The real coordinator hands the shutdown token all the way to a subagent's
/// runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_token_reaches_a_subagent_runtime_through_the_coordinator() {
    let gateway = Gateway::with_llm(ScriptedLlm::always(Canned::sse(shell_call_turn())).await);
    let state = gateway.state();
    gateway.shutdown();

    let session_id = run_guarded_subagent(&state, None).await;

    let rows = texts(&state, session_id);
    assert!(rows.iter().any(|row| row == SHUTDOWN_RECORD), "{rows:?}");
}

/// An approval no one can be asked for is denied automatically, in the shape
/// `invoke_agent` spawns (with a parent forwarder) and without one. Neither
/// involves a user, so no row the run leaves may say one denied or cancelled
/// anything: not the record, the tool row the model reads, or the audit row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unroutable_approval_denial_blames_no_user() {
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let forwarder: std::sync::Arc<dyn alms_tools::EventForwarder> =
        std::sync::Arc::new(crate::runs::tools::RuntimeEventForwarder::new(events_tx));

    for parent_events in [Some(forwarder), None] {
        let gateway = Gateway::with_llm(ScriptedLlm::always(Canned::sse(shell_call_turn())).await);
        let state = gateway.state();

        let session_id = run_guarded_subagent(&state, parent_events).await;

        let rows = texts(&state, session_id);
        assert!(
            rows.iter().any(|row| row == UNROUTABLE_APPROVAL_RECORD),
            "{rows:?}"
        );
        assert!(!rows.iter().any(|row| row.contains("by user")), "{rows:?}");
        let result = denied_tool_result(&state, session_id);
        assert_eq!(result["approval_unavailable"], true, "{result}");
        assert!(result.get("user_denied").is_none(), "{result}");
        assert!(!result.to_string().contains("user"), "{result}");
        let reason = denial_audit_reason(&state, session_id);
        assert!(!reason.contains("user"), "{reason}");
        gateway.channels.1.cancel();
    }
}

/// A user's Stop on a DM run mid-turn, with shutdown starting before its
/// terminal arm runs. The user did ask, so the end keeps `user_cancelled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_stop_of_a_running_dm_run_keeps_its_reason_during_shutdown() {
    let mut gateway = Gateway::start().await;
    let state = gateway.state();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let (run_id, dm_session, _) = start_dm_run(&mut gateway, alice_id, bob_id).await;
    let barrier = crate::runs::lifecycle::install_terminal_transition_barrier(run_id);

    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .map_err(|error| error.0)
        .unwrap();
    barrier.wait().await;
    gateway.shutdown();
    barrier.wait().await;
    wait_finished(&state, run_id).await;

    assert_eq!(
        dm_ended_reason(&state, dm_session).as_deref(),
        Some("user_cancelled")
    );
}

/// A person's denial of a top-level guarded run keeps every row a person's
/// denial has always left, so the automatic-denial wording can't leak onto it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_denial_keeps_the_user_record() {
    let gateway = Gateway::with_llm(ScriptedLlm::always(Canned::sse(shell_call_turn())).await);
    let state = gateway.state();
    let mut guard = alms_core::registry::AgentRecord::for_test("guard");
    guard.posture = Some("guarded".to_string());
    state
        .session_manager
        .store()
        .unwrap()
        .create_agent(&guard)
        .unwrap();
    let (run_id, session_id) = start_web_run(&state, guard.id).await;
    wait_until("the approval", || {
        !state.approval_store.list_pending().is_empty()
    })
    .await;
    let approval = state.approval_store.list_pending()[0].approval_id;
    assert!(state.approval_store.resolve(approval, false).is_some());
    wait_finished(&state, run_id).await;

    let rows = texts(&state, session_id);
    assert!(rows.iter().any(|row| row == USER_CANCEL_RECORD), "{rows:?}");
    let result = denied_tool_result(&state, session_id);
    assert_eq!(
        result,
        serde_json::json!({
            "user_denied": true,
            "message": "The user denied this tool call. Do not retry it or work around the denial.",
        })
    );
    assert_eq!(
        denial_audit_reason(&state, session_id),
        "Tool 'shell' denied by user"
    );
    gateway.channels.1.cancel();
}
