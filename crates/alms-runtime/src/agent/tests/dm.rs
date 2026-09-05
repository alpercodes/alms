// SPDX-License-Identifier: Apache-2.0

//! DM runs: `run_on_session` on a DM session, peer-name resolution, and the DM system-prompt addendum.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};

/// Integration test for `run_on_session`: verifies that the agent uses
/// the shared DM session directly and does NOT persist its text response
/// (only `send_message`-written messages belong in the shared DM session).
#[tokio::test]
async fn test_run_on_session_persists_reasoning_for_dm() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let bob_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let run_id = alms_core::RunId::new();
    let runtime = AgentRuntime::new(bob_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string())
        .with_run_id(run_id);

    // Simulate what MessageBus does: create a shared DM session and
    // write the sender's message into it.
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let sender_msg = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::Text("Hello Bob!".to_string()),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "from_agent": "alice",
            "from_agent_id": AgentId::new().0.to_string(),
            "message_type": "dm",
        })),
    };
    session_manager
        .append_message(session_id, sender_msg)
        .unwrap();

    // Before run: session has exactly 1 message (from alice via MessageBus).
    let history_before = session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history_before.len(),
        1,
        "Pre-condition: 1 message from MessageBus"
    );

    // Run on the shared session (as the recipient agent would).
    let result = runtime
        .run_on_session(&session_manager, session_id, dm_context, "Hello Bob!")
        .await;
    assert!(result.is_ok(), "run_on_session should succeed");
    let output = result.unwrap();
    assert!(
        !output.response.is_empty(),
        "Mock LLM should produce a non-empty response"
    );

    // After run: the session should have 2 messages -- alice's input plus
    // bob's reasoning text (the final text response persisted as reasoning).
    let history_after = session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history_after.len(),
        2,
        "DM text response should be stored as reasoning. Expected 2. Found: {}",
        history_after.len()
    );

    // The first message is alice's original input.
    assert_eq!(history_after[0].role, alms_session::Role::User);
    let alice_meta = history_after[0].metadata.as_ref().unwrap();
    assert_eq!(alice_meta["from_agent"], "alice");
    assert_eq!(alice_meta["message_type"], "dm");

    // The second message is bob's reasoning text (stored as Role::User
    // with message_type="reasoning" to preserve the DM invariant).
    assert_eq!(history_after[1].role, alms_session::Role::User);
    let bob_meta = history_after[1].metadata.as_ref().unwrap();
    assert_eq!(bob_meta["message_type"], "reasoning");
    assert_eq!(bob_meta["from_agent"], "bob");
    assert!(
        bob_meta.get("run_id").is_some(),
        "Reasoning metadata should include run_id"
    );
}

/// Verify that non-DM runs still persist the agent's text response normally.
#[tokio::test]
async fn test_non_dm_run_persists_text_response() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(agent_id, agent_config, llm).unwrap();

    // Run on a normal (non-DM) context.
    let result = runtime
        .run(&session_manager, "normal-context", "Hello agent")
        .await;
    assert!(result.is_ok(), "Normal run should succeed");

    // The session should have 2 messages: user input + assistant response.
    let session = session_manager.get_or_create(agent_id, "normal-context");
    let history = session_manager.get_history(session.id).unwrap();
    assert_eq!(
        history.len(),
        2,
        "Non-DM run should persist both user input and assistant response. Found: {}",
        history.len()
    );
    assert_eq!(history[0].role, alms_session::Role::User);
    assert_eq!(history[1].role, alms_session::Role::Assistant);
}

/// Verify that `dm_marker_metadata` returns `from_agent` metadata for DM
/// sessions and `None` for non-DM sessions.  This metadata is attached to
/// error/cancellation markers so `read_messages` can attribute them.
#[test]
fn test_dm_marker_metadata() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };

    // Without agent_name: always None regardless of is_dm.
    let rt_no_name = AgentRuntime::new(agent_id, agent_config.clone(), llm.clone()).unwrap();
    assert!(rt_no_name.dm_marker_metadata(true).is_none());
    assert!(rt_no_name.dm_marker_metadata(false).is_none());

    // With agent_name: returns metadata only when is_dm is true.
    let rt_named = AgentRuntime::new(agent_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string());

    let meta = rt_named.dm_marker_metadata(true);
    assert!(
        meta.is_some(),
        "DM session with agent_name should produce metadata"
    );
    let meta = meta.unwrap();
    assert_eq!(meta["from_agent"], "bob");
    assert_eq!(meta["from_agent_id"], agent_id.0.to_string());
    assert_eq!(meta["message_type"], "dm");

    // Non-DM: no metadata even with agent_name set.
    assert!(rt_named.dm_marker_metadata(false).is_none());
}

/// Verify that `run_on_session` fails with SessionNotFound if the session
/// does not exist (rather than silently creating a new empty session).
#[tokio::test]
async fn test_run_on_session_fails_if_session_missing() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(agent_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string());

    // Try to run on a non-existent session.
    let fake_session_id = alms_core::SessionId::new();
    let result = runtime
        .run_on_session(&session_manager, fake_session_id, "dm:alice:bob", "hello")
        .await;

    assert!(result.is_err(), "Should fail if session does not exist");
}

/// Verify `dm_peer_name` extracts the correct peer from a DM context_id.
#[test]
fn test_dm_peer_name() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };

    // Agent named "bob" in "dm:alice:bob" → peer is "alice".
    let rt = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
        .unwrap()
        .with_agent_name("bob".to_string());
    assert_eq!(rt.dm_peer_name("dm:alice:bob"), Some("alice".to_string()));

    // Agent named "alice" in "dm:alice:bob" → peer is "bob".
    let rt2 = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
        .unwrap()
        .with_agent_name("alice".to_string());
    assert_eq!(rt2.dm_peer_name("dm:alice:bob"), Some("bob".to_string()));

    // Non-DM context_id → None (not a valid DM context).
    assert_eq!(rt.dm_peer_name("some-context"), None);

    // Malformed context_id → None.
    assert_eq!(rt.dm_peer_name("dm:only-one"), None);

    // Agent name not in context_id → None.
    let rt3 = AgentRuntime::new(AgentId::new(), agent_config.clone(), llm.clone())
        .unwrap()
        .with_agent_name("charlie".to_string());
    assert_eq!(rt3.dm_peer_name("dm:alice:bob"), None);

    // No agent_name set → None.
    let rt4 = AgentRuntime::new(AgentId::new(), agent_config, llm).unwrap();
    assert_eq!(rt4.dm_peer_name("dm:alice:bob"), None);
}

/// Verify that the DM system prompt addendum is injected into the context
/// when running a peer-triggered (implicit-reply) run on a DM session.
#[tokio::test]
async fn test_dm_system_prompt_injection() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let bob_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(bob_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string())
        .with_dm_implicit_reply();

    // Create a shared DM session with one message from alice.
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let sender_msg = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::Text("Hello Bob!".to_string()),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "from_agent": "alice",
            "from_agent_id": AgentId::new().0.to_string(),
            "message_type": "dm",
        })),
    };
    session_manager
        .append_message(session_id, sender_msg)
        .unwrap();

    // Build context for the DM session.
    let context = runtime
        .build_context(&session_manager, &session_id, dm_context, "")
        .await
        .unwrap();

    // The system message should contain DM-specific instructions.
    let system_msg = &context[0];
    assert_eq!(system_msg.role, "system");
    let system_text = system_msg.content.as_deref().unwrap_or("");
    assert!(
        system_text.contains("direct message from agent \"alice\""),
        "System prompt should mention the peer agent. Got: {}",
        &system_text[system_text.len().saturating_sub(300)..]
    );
    assert!(
        system_text.contains("send_message"),
        "System prompt should instruct to use send_message"
    );
    assert!(
        system_text.contains("ignore_message"),
        "System prompt should mention ignore_message"
    );
}

/// #1156 defense-in-depth: a run on a `dm:` session that is NOT
/// peer-triggered (i.e. `dm_implicit_reply` was never armed by the
/// gateway) must NOT receive the implicit-reply addendum. The prompt
/// promises automatic delivery of the final text, and only the DM
/// completion gate (armed exclusively for peer-triggered runs) keeps
/// that promise — injecting it anywhere else would script a silent drop.
#[tokio::test]
async fn test_dm_no_addendum_without_implicit_reply_flag() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_manager = SessionManager::new(SessionConfig::default());
    let llm = LlmClient::new(config).unwrap();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    // No `.with_dm_implicit_reply()` — models a hypothetical non-peer
    // run on a DM session.
    let runtime = AgentRuntime::new(AgentId::new(), agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string());

    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let context = runtime
        .build_context(&session_manager, &session_id, dm_context, "hello")
        .await
        .unwrap();

    let system_text = context[0].content.as_deref().unwrap_or("");
    assert!(
        !system_text.contains("direct message from agent"),
        "non-peer dm: run must NOT get the implicit-reply addendum. Got tail: {}",
        &system_text[system_text.len().saturating_sub(300)..]
    );
}

/// Verify that non-DM sessions do NOT get the DM system prompt addendum.
#[tokio::test]
async fn test_non_dm_no_system_prompt_injection() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(agent_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string());

    // Create a normal session.
    let session = session_manager.get_or_create(agent_id, "normal-context");

    // Build context for a non-DM session.
    let context = runtime
        .build_context(&session_manager, &session.id, "normal-context", "Hi")
        .await
        .unwrap();

    let system_msg = &context[0];
    let system_text = system_msg.content.as_deref().unwrap_or("");
    assert!(
        !system_text.contains("direct message from agent"),
        "Non-DM session should NOT have DM prompt addendum"
    );
}

#[test]
fn test_dm_addendum_contains_peer_name() {
    let addendum = AgentRuntime::dm_addendum("alice");
    assert!(
        addendum.contains("\"alice\""),
        "DM addendum should contain the peer name in quotes. Got: {}",
        addendum
    );
    assert!(
        addendum.contains("send_message"),
        "DM addendum should instruct agent to use send_message. Got: {}",
        addendum
    );
    assert!(
        addendum.contains("direct message from agent"),
        "DM addendum should identify the message as a DM. Got: {}",
        addendum
    );
}

#[test]
fn test_dm_addendum_substitutes_different_peers() {
    let addendum_alice = AgentRuntime::dm_addendum("alice");
    let addendum_charlie = AgentRuntime::dm_addendum("charlie");

    assert!(addendum_alice.contains("\"alice\""));
    assert!(!addendum_alice.contains("\"charlie\""));

    assert!(addendum_charlie.contains("\"charlie\""));
    assert!(!addendum_charlie.contains("\"alice\""));
}

/// Verify the DM addendum survives the tool-loop system prompt rebuild.
///
/// This is a regression test for #346: the agent loop rebuilds the system
/// prompt after processing tool calls, and the DM addendum must be
/// re-injected so the agent still knows to use `send_message`.
///
/// We simulate the tool-loop rebuild logic (same as agent.rs lines
/// 1234-1244) and verify the rebuilt system prompt contains the addendum.
#[tokio::test]
async fn test_dm_addendum_survives_tool_loop_rebuild() {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let bob_id = AgentId::new();

    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    // `with_dm_implicit_reply` mirrors the gateway: the DM addendum is
    // injected only for peer-triggered DM runs (#1156 defense-in-depth).
    let runtime = AgentRuntime::new(bob_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string())
        .with_dm_implicit_reply();

    // Set up a DM session.
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    // Step 1: Build initial context and verify addendum is present.
    let context = runtime
        .build_context(&session_manager, &session_id, dm_context, "Hello")
        .await
        .unwrap();
    let initial_system = context[0].content.as_deref().unwrap_or("");
    assert!(
        initial_system.contains("send_message"),
        "Initial system prompt should contain DM addendum"
    );

    // Step 2: Simulate the tool-loop rebuild (mirrors agent_loop lines 1234-1244).
    // This is the exact code path that was broken before #346.
    let dm_peer: Option<&str> = runtime.dm_peer_name(dm_context).as_deref().map(|s| {
        // Leak the string so we get a &'static str -- acceptable in tests.
        Box::leak(s.to_string().into_boxed_str()) as &str
    });
    let include_user = AgentRuntime::is_user_facing_context(dm_context);

    // Use the extracted helper (rebuild_system_prompt_for_tool_loop) —
    // mirrors the exact code path in agent_loop.
    let mut messages = vec![LlmMessage::system(initial_system.to_string())];
    runtime.rebuild_system_prompt_for_tool_loop(&mut messages, include_user, dm_peer);
    let tool_loop_prompt = messages[0].content.as_deref().unwrap_or("");

    // The rebuilt system prompt must still contain the DM addendum.
    assert!(
        tool_loop_prompt.contains("send_message"),
        "Tool-loop rebuilt system prompt should contain send_message instruction. Got tail: {}",
        &tool_loop_prompt[tool_loop_prompt.len().saturating_sub(300)..]
    );
    assert!(
        tool_loop_prompt.contains("direct message from agent \"alice\""),
        "Tool-loop rebuilt system prompt should reference peer agent 'alice'. Got tail: {}",
        &tool_loop_prompt[tool_loop_prompt.len().saturating_sub(300)..]
    );

    // Also verify the tool_loop prompt content is present (not just DM addendum).
    assert!(
        tool_loop_prompt.contains(&runtime.config.prompts.tool_loop),
        "Tool-loop rebuilt system prompt should contain tool_loop continuation guidance"
    );
}
