// SPDX-License-Identifier: Apache-2.0

//! #1278 — episodic memory is injected on ordinary runs and withheld from subagent runs.

use super::base_runtime;
use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};

// ── #1278: episodic memory is not readable from a subagent run ──────────────

/// Build a store-backed manager holding one episodic summary for `agent_id`
/// on an ordinary chat session, plus the two sessions a `build_context` call
/// can run on.
fn manager_with_one_episodic_summary(
    agent_id: AgentId,
    subagent_context: &str,
) -> (SessionManager, alms_core::SessionId, alms_core::SessionId) {
    use alms_session::Session;
    use alms_session::sqlite::SqliteStore;

    let store = SqliteStore::open_in_memory().unwrap();

    // The agent's own operator chat, and the summary of it. This is the
    // private history the subagent run must not be able to surface.
    let chat = Session::new(agent_id, "web-chat-private");
    store.save_session(&chat).unwrap();
    store
        .upsert_session_summary(
            agent_id,
            chat.id,
            "OPERATOR-ONLY: discussed the Q3 layoff list with Alper.",
            None,
            Some("User chat"),
        )
        .unwrap();

    // Two more sessions to run on: another chat (control) and the subagent
    // session. Both are filed under the SAME agent id, which is exactly the
    // post-#1278 shape for a named subagent — so the gate cannot be passing
    // for the trivial reason that the agent ids differ.
    let other_chat = Session::new(agent_id, "web-chat-other");
    let subagent = Session::new(agent_id, subagent_context);
    store.save_session(&other_chat).unwrap();
    store.save_session(&subagent).unwrap();

    let manager = SessionManager::with_store(SessionConfig::default(), store).unwrap();
    (manager, other_chat.id, subagent.id)
}

fn episodic_probe_runtime(agent_id: AgentId) -> AgentRuntime {
    AgentRuntime {
        agent_id,
        agent_name: Some("reviewer".to_string()),
        ..base_runtime(LlmClient::new(LlmConfig::default()).unwrap())
    }
}

fn context_contains(messages: &[LlmMessage], needle: &str) -> bool {
    messages
        .iter()
        .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
}

fn context_contains_episodic(messages: &[LlmMessage]) -> bool {
    context_contains(messages, "<episodic_memory>")
}

#[tokio::test]
async fn build_context_injects_episodic_memory_on_an_ordinary_run() {
    // The control. Without this the subagent assertion below would pass on
    // a fixture that simply has no summaries to inject — the exact way a
    // negative test rots into a tautology.
    let agent_id = AgentId::new();
    let (manager, chat_id, _) =
        manager_with_one_episodic_summary(agent_id, "subagent_unused_context");

    let runtime = episodic_probe_runtime(agent_id);
    assert_ne!(
        runtime.config.context_config.run_summary_mode,
        alms_core::config::RunSummaryMode::Off,
        "the default mode must be non-Off or this pair of tests proves nothing"
    );

    let messages = runtime
        .build_context(&manager, &chat_id, "web-chat-other", "hello")
        .await
        .unwrap();

    assert!(
        context_contains_episodic(&messages),
        "an ordinary run must still get its episodic block: {messages:?}"
    );
    assert!(
        context_contains(&messages, "Q3 layoff list"),
        "and the block must carry the summary text"
    );
}

#[tokio::test]
async fn build_context_withholds_episodic_memory_on_a_named_subagent_run() {
    // #1278 files a named subagent session under the INVOKED agent's
    // registry id, so `load_session_summaries(self.agent_id)` — which
    // filters on `agent_id` alone — would hand the subagent run the invoked
    // agent's summaries of its own operator chats, Telegram threads, DMs and
    // jobs. The subagent's output is returned verbatim to the invoking
    // parent as the `invoke_agent` result, so this is a read primitive from
    // one agent's private history into another agent's context, and no tool
    // call is needed to trigger it.
    let agent_id = AgentId::new();
    let parent_id = AgentId::new();
    let context_id = alms_core::named_subagent_context_id(parent_id, "reviewer");
    let (manager, _, subagent_id) = manager_with_one_episodic_summary(agent_id, &context_id);

    let runtime = episodic_probe_runtime(agent_id);
    let messages = runtime
        .build_context(&manager, &subagent_id, &context_id, "review this diff")
        .await
        .unwrap();

    assert!(
        !context_contains_episodic(&messages),
        "a subagent run must get no episodic block at all: {messages:?}"
    );
    assert!(
        !context_contains(&messages, "Q3 layoff list"),
        "and specifically none of the invoked agent's private summary text: {messages:?}"
    );
}

#[tokio::test]
async fn build_context_withholds_episodic_memory_on_an_ephemeral_subagent_run() {
    // The gate keys on the run's own `context_id`, not on how the session
    // was filed, so an ephemeral subagent (fresh `AgentId::new()` filer, no
    // registry agent) is covered by the same branch. Pinned separately
    // because a fix written against #1278's named path alone would leave
    // this one open the moment ephemeral subagents are ever re-homed.
    let agent_id = AgentId::new();
    let parent_id = AgentId::new();
    let context_id = format!("subagent_{}_{}", parent_id.0, uuid::Uuid::new_v4());
    let (manager, _, subagent_id) = manager_with_one_episodic_summary(agent_id, &context_id);

    let runtime = episodic_probe_runtime(agent_id);
    let messages = runtime
        .build_context(&manager, &subagent_id, &context_id, "one-shot task")
        .await
        .unwrap();

    assert!(
        !context_contains_episodic(&messages),
        "an ephemeral subagent run must get no episodic block either: {messages:?}"
    );
}
