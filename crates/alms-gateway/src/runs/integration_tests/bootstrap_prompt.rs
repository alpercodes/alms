// SPDX-License-Identifier: Apache-2.0

//! #174: the bootstrap interview prompt is for runs a human started.
//!
//! `execute_run` swaps an agent's system prompt for `bootstrap.md` ("You are
//! a new ALMS agent being set up for the first time ... Ask the user ...")
//! while the agent has no `personality.md`. Before #174 it did that for every
//! run type, so a peer-triggered DM turn was told to interview a user by the
//! same prompt whose DM addendum says the counterparty is not a human.
//!
//! Each test drives a real `execute_run` against a scripted LLM for an agent
//! whose workspace is empty, and reads the system prompt off the request the
//! provider received. That is the prompt the agent was actually given,
//! whatever happens between the override and the wire.
//!
//! The guard reads `is_system_triggered`, so the value each triggered path
//! passes is part of the fix. Two tests start the run from the loop that
//! production feeds instead of building `RunParams`, so they pin that value
//! wherever below the loop it is set:
//! [`peer_dm_turn_gets_the_normal_prompt_not_bootstrap`] from
//! `run_trigger_loop` (peer DMs, notification runs, episode continuations),
//! and [`scheduled_job_run_gets_the_normal_prompt_not_bootstrap`] from
//! `scheduler_fire_loop` (scheduled jobs). Both paths pass `true`, which also
//! drives the Guarded -> Autonomous posture override. The other tests build
//! `RunParams` by hand, to test the guard itself: with `false` for a human,
//! as `create_run` (HTTP `POST /runs`) sends it, and with `true` across
//! session types.

use super::seed_alice_bob;
use crate::server::AppState;
use alms_coordinator::message_bus::RunTrigger;
use alms_core::{AgentId, Run, SessionId};
use alms_test_support::{Canned, ScriptedLlm};
use alms_tools::MessageSender;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// bob's own system prompt, planted in the live config. Asserting on it
/// tells "got its normal prompt" apart from "got some prompt that is not
/// bootstrap".
const NORMAL_PROMPT: &str = "NORMAL-PROMPT-SENTINEL: you are bob, a registered ALMS agent.";

/// One OpenAI-style streamed text turn (the default `openrouter` provider
/// parses this wire shape).
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

/// A gateway whose agents (alice and bob, registered) have no
/// `personality.md`, whose LLM is scripted, and whose system prompt is
/// [`NORMAL_PROMPT`].
struct Harness {
    state: AppState,
    llm: ScriptedLlm,
    alice: AgentId,
    bob: AgentId,
    shutdown: CancellationToken,
    // The MessageBus's run triggers. Held so the DM completion gate's reply
    // to alice has somewhere to go; the peer-DM test takes it and feeds it
    // to `run_trigger_loop`.
    triggers: Option<mpsc::Receiver<RunTrigger>>,
    // An empty directory: every agent under it `needs_bootstrap()`.
    _workspace: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let llm = ScriptedLlm::always(Canned::sse(text_turn("hello"))).await;
        let workspace = tempfile::tempdir().unwrap();
        let (state, shutdown, _completions, triggers, _dm_events) =
            crate::test_support::TestAppState::new()
                .in_memory_sqlite()
                .workspace_dir(workspace.path())
                .llm_config(alms_runtime::LlmConfig {
                    base_url: llm.base_url(),
                    api_key: "test-key".to_string(),
                    default_model: "test-model".to_string(),
                    timeout_secs: 30,
                    stream_chunk_timeout_secs: 30,
                    ..alms_runtime::LlmConfig::default()
                })
                .build_with_channels();
        state.agent_config.write().system_prompt = NORMAL_PROMPT.to_string();
        let (alice, bob) = seed_alice_bob(&state);
        Self {
            state,
            llm,
            alice,
            bob,
            shutdown,
            triggers: Some(triggers),
            _workspace: workspace,
        }
    }

    /// The first request the scripted LLM receives. Triggered runs go through
    /// the agent queue, so the test cannot await the run itself.
    async fn first_request(&self) -> String {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(body) = self.llm.request_bodies().await.into_iter().next() {
                return body;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no run reached the LLM within 10s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Run one bob turn through `execute_run` and return the system prompt
    /// the provider received for it.
    async fn bob_system_prompt(
        &self,
        session_id: SessionId,
        context_id: &str,
        is_peer_message: bool,
        is_system_triggered: bool,
    ) -> String {
        let run = Run::new(session_id, self.bob, "ping".to_string());
        let run_id = run.run_id;
        self.state.run_manager.insert_run(run.clone()).unwrap();
        let cancel_token = CancellationToken::new();
        self.state
            .run_manager
            .register_cancel_token(run_id, cancel_token.clone());

        crate::runs::lifecycle::execute_run(
            self.state.clone(),
            crate::runs::RunParams {
                run_id,
                session_id,
                agent_id: self.bob,
                input: run.input,
                context_id: context_id.to_string(),
                cancel_token,
                is_peer_message,
                is_system_triggered,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;

        let bodies = self.llm.request_bodies().await;
        system_prompt_of(
            bodies
                .last()
                .unwrap_or_else(|| panic!("[{context_id}] the run never reached the LLM")),
            context_id,
        )
    }
}

/// The system prompt in one request the provider received. `label` names
/// the case in panic messages.
fn system_prompt_of(body: &str, label: &str) -> String {
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let system = body["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|m| m["role"] == "system"))
        .unwrap_or_else(|| panic!("[{label}] no system message in {body}"));
    match &system["content"] {
        serde_json::Value::String(text) => text.clone(),
        // Content-part arrays (cache-control shapes): join the text parts.
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        other => panic!("[{label}] unexpected system content {other}"),
    }
}

fn bootstrap() -> &'static str {
    alms_runtime::AgentWorkspace::bootstrap_prompt()
}

/// Control, and what keeps the rows below from passing vacuously: in this
/// harness a turn a human sent does get the bootstrap prompt. Onboarding by
/// web chat is unchanged.
#[tokio::test]
async fn user_started_web_chat_run_still_gets_the_bootstrap_prompt() {
    let h = Harness::new().await;
    let session = h.state.session_manager.get_or_create(h.bob, "web-chat-1");

    let prompt = h
        .bob_system_prompt(session.id, "web-chat-1", false, false)
        .await;

    assert!(
        prompt.starts_with(bootstrap()),
        "a human-started run on an agent with no personality.md must get the \
         bootstrap interview prompt; got:\n{prompt}"
    );
    assert!(!prompt.contains(NORMAL_PROMPT), "got:\n{prompt}");
    h.shutdown.cancel();
}

/// The reported case: alice DMs bob, and bob's peer-triggered turn gets his
/// normal prompt with the DM addendum, not an instruction to interview a
/// user in a session the addendum says has none.
///
/// End to end: alice's message goes through the MessageBus, and bob's turn
/// is started by `run_trigger_loop` -> `enqueue_triggered_run`, so this pins
/// the `is_system_triggered` value that caller passes, not only the guard.
#[tokio::test]
async fn peer_dm_turn_gets_the_normal_prompt_not_bootstrap() {
    let mut h = Harness::new().await;
    let triggers = h.triggers.take().unwrap();
    let trigger_loop = tokio::spawn(crate::runs::notifications::run_trigger_loop(
        triggers,
        h.state.clone(),
    ));
    h.state
        .message_bus
        .send("alice", h.alice, "bob", h.bob, "ping", None)
        .await
        .unwrap();

    // bob's turn is the only run until it replies, so the first request is
    // his. The addendum assertion below confirms it.
    let prompt = system_prompt_of(&h.first_request().await, "dm:alice:bob");
    trigger_loop.abort();

    assert!(
        !prompt.contains(bootstrap()),
        "a peer DM turn must not be told to interview a user; got:\n{prompt}"
    );
    assert!(
        prompt.starts_with(NORMAL_PROMPT),
        "bob's own prompt must lead; got:\n{prompt}"
    );
    assert!(
        prompt.contains(
            r#"This is a direct message from agent "alice". It is NOT from a human user"#
        ),
        "the DM addendum still arrives; got:\n{prompt}"
    );
    h.shutdown.cancel();
}

/// A scheduled job, fired through `scheduler_fire_loop` the way the
/// scheduler fires it, so this pins the `is_system_triggered` value the job
/// path passes, whichever function under the loop builds the run. The `job_`
/// row below covers the same session type with hand-built `RunParams`.
#[tokio::test]
async fn scheduled_job_run_gets_the_normal_prompt_not_bootstrap() {
    let h = Harness::new().await;
    let job_id = super::create_recurring_job(&h.state, h.bob, "daily digest");
    let (fire_tx, fire_rx) = mpsc::unbounded_channel();
    let fire_loop = tokio::spawn(crate::runs::notifications::scheduler_fire_loop(
        fire_rx,
        h.state.clone(),
    ));
    fire_tx.send(job_id).unwrap();

    let prompt = system_prompt_of(&h.first_request().await, "job");
    fire_loop.abort();

    assert!(
        !prompt.contains(bootstrap()),
        "a scheduled job run must not get the bootstrap prompt; got:\n{prompt}"
    );
    assert!(
        prompt.starts_with(NORMAL_PROMPT),
        "bob's own prompt must lead; got:\n{prompt}"
    );
    h.shutdown.cancel();
}

/// Every other system-triggered shape: a notification run on bob's own
/// notifications session (the DM-ended route), a notification run on the
/// user's web-chat session (the subagent-completion route), and a scheduled
/// job. The web-chat row is the one a context-type test such as
/// `AgentRuntime::is_user_facing_context` would let through: the session is
/// the user's, but nobody typed this turn.
#[tokio::test]
async fn system_triggered_runs_get_the_normal_prompt_not_bootstrap() {
    let job_context = format!("job_{}", uuid::Uuid::new_v4());
    for context_id in ["notifications:bob", "web-chat-1", job_context.as_str()] {
        let h = Harness::new().await;
        let session = h.state.session_manager.get_or_create(h.bob, context_id);

        let prompt = h
            .bob_system_prompt(session.id, context_id, false, true)
            .await;

        assert!(
            !prompt.contains(bootstrap()),
            "[{context_id}] a system-triggered run must not get the bootstrap \
             prompt; got:\n{prompt}"
        );
        assert!(
            prompt.starts_with(NORMAL_PROMPT),
            "[{context_id}] bob's own prompt must lead; got:\n{prompt}"
        );
        h.shutdown.cancel();
    }
}
