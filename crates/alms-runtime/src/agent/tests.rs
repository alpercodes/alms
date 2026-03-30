use super::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use crate::tools::ToolRegistry;
use alms_core::{AgentId, AlmsError};
use alms_session::{SessionConfig, SessionManager};

#[tokio::test]
async fn test_agent_config_default() {
    let config = AgentConfig::default();
    assert_eq!(config.max_iterations, 10);
    assert!(!config.system_prompt.is_empty());
    assert_eq!(config.posture, Posture::Guarded);
}

#[tokio::test]
async fn test_stream_llm_call_emits_token_deltas() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let runtime = AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm: LlmClient::new(config).unwrap(),
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        agent_name: None,
    };

    let request =
        CompletionRequest::new("test").with_messages(vec![LlmMessage::user("hello world")]);

    let (content, tool_calls, _usage) = runtime.stream_llm_call(request).await.unwrap();

    // Content should be the reassembled mock response
    assert_eq!(content.as_deref(), Some("[mock] hello world"));
    // No tool calls from mock
    assert!(tool_calls.is_none());

    // Verify TokenDelta events were emitted (one per word chunk)
    let mut deltas = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let RuntimeEvent::TokenDelta { delta, .. } = event {
            deltas.push(delta);
        }
    }
    assert!(deltas.len() >= 2, "should emit multiple token deltas");
    let reassembled: String = deltas.concat();
    assert_eq!(reassembled, "[mock] hello world");
}

#[tokio::test]
async fn test_build_context() {
    let runtime = AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm: LlmClient::new(LlmConfig::default()).unwrap(),
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        agent_name: None,
    };

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let messages = runtime
        .build_context(&session_manager, &session.id, "test", "hello")
        .await
        .unwrap();
    // system prompt + current input = 2
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
}

#[tokio::test]
async fn test_build_context_dm_perspective_mapping() {
    let runtime = AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm: LlmClient::new(LlmConfig::default()).unwrap(),
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        agent_name: Some("bob".to_string()),
    };

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);

    // Create a shared DM session and populate it with messages from both agents
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let session = session_manager.get_or_create_shared(session_id, dm_context);

    // Alice's message (from_agent = "alice") — should stay User for Bob's perspective
    session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text("Hello Bob!".to_string()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "from_agent": "alice",
                    "message_type": "dm",
                })),
            },
        )
        .unwrap();

    // Bob's message (from_agent = "bob") — should become Assistant for Bob's perspective
    session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text("Hi Alice!".to_string()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "from_agent": "bob",
                    "message_type": "dm",
                })),
            },
        )
        .unwrap();

    let messages = runtime
        .build_context(&session_manager, &session.id, dm_context, "What's up?")
        .await
        .unwrap();

    // system + 2 history + current input = 4
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, "system");
    // Alice's message stays as "user" from Bob's perspective
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content_str(), "Hello Bob!");
    // Bob's own message becomes "assistant" from Bob's perspective
    assert_eq!(messages[2].role, "assistant");
    assert_eq!(messages[2].content_str(), "Hi Alice!");
    // Current input
    assert_eq!(messages[3].role, "user");
    assert_eq!(messages[3].content_str(), "What's up?");
}

#[tokio::test]
async fn test_build_context_non_dm_no_perspective() {
    // When context_id does NOT start with "dm:", no perspective mapping should occur
    let runtime = AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm: LlmClient::new(LlmConfig::default()).unwrap(),
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        agent_name: Some("bob".to_string()),
    };

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "regular-context");

    // Add a message with from_agent metadata (shouldn't matter for non-DM)
    session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text("Hello".to_string()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({"from_agent": "bob"})),
            },
        )
        .unwrap();

    let messages = runtime
        .build_context(&session_manager, &session.id, "regular-context", "hi")
        .await
        .unwrap();

    // system + 1 history + current = 3
    assert_eq!(messages.len(), 3);
    // No perspective mapping: message stays as "user" even though from_agent == "bob"
    assert_eq!(messages[1].role, "user");
}

#[test]
fn test_sanitize_error_runtime_auth() {
    let err = AlmsError::Runtime("HTTP 401 Unauthorized at https://api.example.com".into());
    assert_eq!(
        helpers::sanitize_error_for_session(&err),
        "LLM authentication error"
    );
}

#[test]
fn test_sanitize_error_runtime_rate_limit() {
    let err = AlmsError::Runtime("429 Too Many Requests".into());
    assert_eq!(
        helpers::sanitize_error_for_session(&err),
        "LLM rate limit exceeded"
    );
}

#[test]
fn test_sanitize_error_runtime_timeout() {
    let err = AlmsError::Runtime("request timed out after 60s".into());
    assert_eq!(
        helpers::sanitize_error_for_session(&err),
        "LLM request timed out"
    );
}

#[test]
fn test_sanitize_error_runtime_generic() {
    let err = AlmsError::Runtime("some secret-key=abc123 in raw error".into());
    assert_eq!(helpers::sanitize_error_for_session(&err), "Runtime error");
}

#[test]
fn test_sanitize_error_tool_strips_output() {
    let err = AlmsError::ToolExecution("shell_exec: secret output here".into());
    assert_eq!(
        helpers::sanitize_error_for_session(&err),
        "Tool execution failed: shell_exec"
    );
}

#[test]
fn test_sanitize_error_context_building() {
    let err = AlmsError::Runtime("failed to build context window".into());
    assert_eq!(
        helpers::sanitize_error_for_session(&err),
        "Context building failed"
    );
}

#[tokio::test]
async fn test_run_persists_user_message_on_failure() {
    // Use mock LLM that will produce a response, but we can verify
    // the user message is persisted to history.
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();
    let runtime = AgentRuntime::new(agent_id, AgentConfig::default(), llm).unwrap();

    // Run with mock LLM (succeeds)
    let result = runtime
        .run(&session_manager, "test-context", "hello agent")
        .await;
    assert!(result.is_ok());

    // Verify the user message was persisted in session history
    let session = session_manager.get_or_create(agent_id, "test-context");
    let history = session_manager.get_history(session.id).unwrap();
    assert!(
        history.iter().any(|m| m.role == alms_session::Role::User
            && matches!(&m.content, alms_session::Content::Text(t) if t == "hello agent")),
        "User message should be persisted in session history"
    );
}

#[tokio::test]
async fn test_guarded_posture_sequential_approvals() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools = crate::ToolRegistry::with_builtins_sandboxed(None, true, &["echo".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");

    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(config).unwrap(),
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        agent_name: None,
    };

    let tool_calls = vec![
        ToolCall {
            id: "tc1".to_string(),
            function: FunctionCall {
                name: "echo".to_string(),
                arguments: r#"{"text":"first"}"#.to_string(),
            },
        },
        ToolCall {
            id: "tc2".to_string(),
            function: FunctionCall {
                name: "echo".to_string(),
                arguments: r#"{"text":"second"}"#.to_string(),
            },
        },
    ];

    // Track the order: approval_count increments only after each approval resolves.
    let approval_count = Arc::new(AtomicUsize::new(0));
    let approval_count_clone = approval_count.clone();

    // Spawn the approval handler: approve each request, verify sequential ordering.
    let handler = tokio::spawn(async move {
        let mut approval_order = Vec::new();
        while let Some(event) = rx.recv().await {
            if let RuntimeEvent::ApprovalRequired {
                decision_tx, tool, ..
            } = event
            {
                let count = approval_count_clone.load(Ordering::SeqCst);
                approval_order.push((tool, count));
                decision_tx.send(true).unwrap();
                approval_count_clone.fetch_add(1, Ordering::SeqCst);
            }
        }
        approval_order
    });

    // Execute tool calls sequentially (guarded path)
    let mut results = Vec::new();
    for tc in &tool_calls {
        results.push(
            runtime
                .execute_tool_call(tc, &session_manager, session.id)
                .await,
        );
    }

    // Both should succeed
    assert!(results[0].is_ok());
    assert!(results[1].is_ok());

    // Drop the runtime's sender to close the channel so the handler finishes
    drop(runtime);

    let approval_order = handler.await.unwrap();
    // The second approval should have seen count=1 (first was resolved),
    // proving sequential execution.
    assert_eq!(approval_order.len(), 2);
    assert_eq!(approval_order[0].1, 0, "First approval should see count=0");
    assert_eq!(
        approval_order[1].1, 1,
        "Second approval should see count=1 (first resolved)"
    );
}

#[test]
fn test_invalid_sandbox_root_fails_closed() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_config = AgentConfig {
        sandbox_root: "/nonexistent/path/that/does/not/exist".into(),
        ..AgentConfig::default()
    };
    let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
    assert!(result.is_err(), "Should fail when sandbox_root is invalid");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox_root"),
        "Error should mention sandbox_root: {err}"
    );
}

#[test]
fn test_empty_sandbox_root_means_unrestricted() {
    let config = crate::llm_types::LlmConfig {
        mock: true,
        ..crate::llm_types::LlmConfig::default()
    };
    let llm = LlmClient::new(config).unwrap();
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let result = AgentRuntime::new(AgentId::new(), agent_config, llm);
    assert!(
        result.is_ok(),
        "Empty sandbox_root should mean unrestricted"
    );
}

/// Integration test for `run_on_session`: verifies that the agent uses
/// the shared DM session directly and does NOT persist its text response
/// (only `send_message`-written messages belong in the shared DM session).
#[tokio::test]
async fn test_run_on_session_skips_text_response_for_dm() {
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
        .with_agent_name("bob".to_string());

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

    // After run: the session should still have only 1 message.
    // The agent's text response is NOT persisted to the DM session —
    // only send_message-written messages belong there.
    let history_after = session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history_after.len(),
        1,
        "DM text response should NOT be stored. Expected 1 (alice's input only). Found: {}",
        history_after.len()
    );

    // The one message is alice's original input.
    assert_eq!(history_after[0].role, alms_session::Role::User);
    let alice_meta = history_after[0].metadata.as_ref().unwrap();
    assert_eq!(alice_meta["from_agent"], "alice");
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
/// when running on a DM session, telling the agent to use `send_message`.
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
        .with_agent_name("bob".to_string());

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
fn test_posture_from_str() {
    assert_eq!("guarded".parse::<Posture>().unwrap(), Posture::Guarded);
    assert_eq!(
        "full_control".parse::<Posture>().unwrap(),
        Posture::FullControl
    );
    assert_eq!(
        "autonomous".parse::<Posture>().unwrap(),
        Posture::Autonomous
    );
    assert!("unknown".parse::<Posture>().is_err());
    assert!("".parse::<Posture>().is_err());
}

#[test]
fn test_posture_display() {
    assert_eq!(Posture::Guarded.to_string(), "guarded");
    assert_eq!(Posture::FullControl.to_string(), "full_control");
    assert_eq!(Posture::Autonomous.to_string(), "autonomous");
}

#[test]
fn test_posture_roundtrip() {
    for posture in [Posture::Guarded, Posture::FullControl, Posture::Autonomous] {
        let s = posture.to_string();
        let parsed: Posture = s.parse().unwrap();
        assert_eq!(parsed, posture);
    }
}

#[test]
fn test_is_user_facing_context() {
    // User-facing: web UI, Telegram
    assert!(AgentRuntime::is_user_facing_context("web-chat-123"));
    assert!(AgentRuntime::is_user_facing_context("telegram_agent_456"));

    // Non-user-facing: DM, subagent, job, notification
    assert!(!AgentRuntime::is_user_facing_context("dm:alice:bob"));
    assert!(!AgentRuntime::is_user_facing_context("subagent_task123"));
    assert!(!AgentRuntime::is_user_facing_context(
        "subagent_task123_reviewer"
    ));
    assert!(!AgentRuntime::is_user_facing_context("job_abc"));
    assert!(!AgentRuntime::is_user_facing_context("notifications:alice"));
    assert!(!AgentRuntime::is_user_facing_context(
        "notifications:my-agent"
    ));

    // Edge cases: empty string and unknown prefix default to user-facing
    assert!(AgentRuntime::is_user_facing_context(""));
    assert!(AgentRuntime::is_user_facing_context("unknown_prefix"));

    // Near-miss prefixes must NOT match (prefix must be exact)
    assert!(AgentRuntime::is_user_facing_context("dmx:something"));
    assert!(AgentRuntime::is_user_facing_context("subagentx_something"));
    assert!(AgentRuntime::is_user_facing_context("jobs_something"));
    assert!(AgentRuntime::is_user_facing_context(
        "notification_something"
    ));
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
    let runtime = AgentRuntime::new(bob_id, agent_config, llm)
        .unwrap()
        .with_agent_name("bob".to_string());

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

// ---- send_message / ignore_message conflict detection tests (#364) ----
//
// These tests exercise `detect_dm_conflict` for request-level conflict
// detection, and `alms_core::ran_ignore_message_successfully` for
// result-level termination decisions (verifying the real code path).

/// When both `send_message` and `ignore_message` appear in the same
/// tool-call batch, both should be blocked with error results.
/// Neither tool should execute, and the run should NOT terminate
/// early (the agent gets another iteration to choose one).
#[test]
fn test_dm_conflict_blocks_both_tools() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![
        ToolCall {
            id: "tc_send".to_string(),
            function: FunctionCall {
                name: "send_message".to_string(),
                arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
            },
        },
        ToolCall {
            id: "tc_ignore".to_string(),
            function: FunctionCall {
                name: "ignore_message".to_string(),
                arguments: r#"{"reason":"nothing to add"}"#.to_string(),
            },
        },
    ];

    let check = dm::detect_dm_conflict(&tool_calls);

    assert!(
        check.conflict,
        "Conflict should be detected when both tools are present"
    );

    // Both DM tools should appear in the conflicting set.
    for tc in &tool_calls {
        assert!(
            check.conflicting_tools.contains(&tc.function.name.as_str()),
            "{} should be in the conflicting set",
            tc.function.name
        );
    }
}

/// When `ignore_message` is called alone (no `send_message`), there
/// should be no conflict.
#[test]
fn test_ignore_message_alone_no_conflict() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_ignore".to_string(),
        function: FunctionCall {
            name: "ignore_message".to_string(),
            arguments: r#"{"reason":"not relevant"}"#.to_string(),
        },
    }];

    let check = dm::detect_dm_conflict(&tool_calls);

    assert!(
        !check.conflict,
        "No conflict when only ignore_message is present"
    );
    assert!(
        check.conflicting_tools.is_empty(),
        "No tools should be flagged as conflicting"
    );
}

/// When both conflicting tools appear alongside a normal tool (e.g.
/// `echo`), only the conflicting tools should be blocked. The normal
/// tool should still be eligible for execution.
#[test]
fn test_dm_conflict_preserves_non_conflicting_tools() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![
        ToolCall {
            id: "tc_echo".to_string(),
            function: FunctionCall {
                name: "echo".to_string(),
                arguments: r#"{"message":"hello"}"#.to_string(),
            },
        },
        ToolCall {
            id: "tc_send".to_string(),
            function: FunctionCall {
                name: "send_message".to_string(),
                arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
            },
        },
        ToolCall {
            id: "tc_ignore".to_string(),
            function: FunctionCall {
                name: "ignore_message".to_string(),
                arguments: r#"{"reason":"nothing to add"}"#.to_string(),
            },
        },
    ];

    let check = dm::detect_dm_conflict(&tool_calls);
    assert!(check.conflict);

    // Partition: echo should be in the "execute" set, the other two
    // should be in the "conflict error" set.
    let exec_indices: Vec<usize> = tool_calls
        .iter()
        .enumerate()
        .filter(|(_, tc)| !check.conflicting_tools.contains(&tc.function.name.as_str()))
        .map(|(i, _)| i)
        .collect();

    assert_eq!(exec_indices, vec![0], "Only echo (index 0) should execute");
    assert_eq!(tool_calls[exec_indices[0]].function.name, "echo");

    // The two DM tools should be blocked.
    let blocked: Vec<&str> = tool_calls
        .iter()
        .filter(|tc| check.conflicting_tools.contains(&tc.function.name.as_str()))
        .map(|tc| tc.function.name.as_str())
        .collect();
    assert_eq!(blocked.len(), 2);
    assert!(blocked.contains(&"send_message"));
    assert!(blocked.contains(&"ignore_message"));
}

/// When `send_message` is called alone (no `ignore_message`), there
/// should be no conflict.
#[test]
fn test_send_message_alone_no_conflict() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_send".to_string(),
        function: FunctionCall {
            name: "send_message".to_string(),
            arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
        },
    }];

    let check = dm::detect_dm_conflict(&tool_calls);
    assert!(
        !check.conflict,
        "No conflict when only send_message is present"
    );
    assert!(check.conflicting_tools.is_empty());
}

/// When neither DM tool is present, there should be no conflict.
#[test]
fn test_no_dm_tools_no_conflict() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_echo".to_string(),
        function: FunctionCall {
            name: "echo".to_string(),
            arguments: r#"{"message":"hello"}"#.to_string(),
        },
    }];

    let check = dm::detect_dm_conflict(&tool_calls);
    assert!(!check.conflict);
    assert!(check.conflicting_tools.is_empty());
}

// ---- should_terminate_after_dm_send tests (#407 Bug 1) ----

/// In a DM run, `send_message` alone should terminate the loop.
#[test]
fn test_dm_send_terminates_in_dm_context() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_send".to_string(),
        function: FunctionCall {
            name: "send_message".to_string(),
            arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
        },
    }];

    assert!(
        dm::should_terminate_after_dm_send(&tool_calls, true, false),
        "send_message in a DM run (no conflict) should terminate the loop"
    );
}

/// Outside a DM run, `send_message` should NOT terminate the loop
/// (the agent may be using it from a web-chat context to message
/// another agent, and may have more work to do).
#[test]
fn test_dm_send_does_not_terminate_outside_dm() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_send".to_string(),
        function: FunctionCall {
            name: "send_message".to_string(),
            arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
        },
    }];

    assert!(
        !dm::should_terminate_after_dm_send(&tool_calls, false, false),
        "send_message outside a DM run should NOT terminate the loop"
    );
}

/// When there is a conflict (both send_message and ignore_message),
/// do NOT terminate — let the agent retry on the next iteration.
#[test]
fn test_dm_send_does_not_terminate_on_conflict() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![
        ToolCall {
            id: "tc_send".to_string(),
            function: FunctionCall {
                name: "send_message".to_string(),
                arguments: r#"{"to":"alice","message":"hi"}"#.to_string(),
            },
        },
        ToolCall {
            id: "tc_ignore".to_string(),
            function: FunctionCall {
                name: "ignore_message".to_string(),
                arguments: r#"{"reason":"done"}"#.to_string(),
            },
        },
    ];

    assert!(
        !dm::should_terminate_after_dm_send(&tool_calls, true, true),
        "send_message with conflict should NOT terminate the loop"
    );
}

/// When no DM tool is present, should_terminate_after_dm_send
/// returns false even in a DM context.
#[test]
fn test_dm_send_no_dm_tools() {
    use crate::llm_types::{FunctionCall, ToolCall};

    let tool_calls = vec![ToolCall {
        id: "tc_echo".to_string(),
        function: FunctionCall {
            name: "echo".to_string(),
            arguments: r#"{"message":"hello"}"#.to_string(),
        },
    }];

    assert!(
        !dm::should_terminate_after_dm_send(&tool_calls, true, false),
        "No send_message call means no DM-send termination"
    );
}

// ---- dm_tool_was_called tests (#361) ----

/// Returns false when no tool call records exist.
#[test]
fn test_dm_tool_was_called_empty_records() {
    assert!(
        !dm::dm_tool_was_called(&[]),
        "No records should mean no DM tool was called"
    );
}

/// Returns false when only non-DM tools were called.
#[test]
fn test_dm_tool_was_called_only_non_dm_tools() {
    let records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("echo".to_string()),
            tool_id: Some("tc_echo".to_string()),
            params: Some(r#"{"message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("echo".to_string()),
            tool_id: Some("tc_echo".to_string()),
            params: None,
            result: Some(r#"{"output":"hi"}"#.to_string()),
            timestamp: chrono::Utc::now(),
        },
    ];
    assert!(
        !dm::dm_tool_was_called(&records),
        "echo tool should not count as a DM tool"
    );
}

/// Returns true when send_message was called and succeeded.
#[test]
fn test_dm_tool_was_called_send_message() {
    let records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send".to_string()),
            params: Some(r#"{"to":"alice","message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send".to_string()),
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
        },
    ];
    assert!(
        dm::dm_tool_was_called(&records),
        "send_message should be detected"
    );
}

/// Returns true when ignore_message was called and succeeded.
#[test]
fn test_dm_tool_was_called_ignore_message() {
    let records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore".to_string()),
            params: Some(r#"{"reason":"not relevant"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore".to_string()),
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
        },
    ];
    assert!(
        dm::dm_tool_was_called(&records),
        "ignore_message should be detected"
    );
}

/// Only checks Assistant-role records paired with Tool-role results
/// (not Tool-role results alone).
#[test]
fn test_dm_tool_was_called_ignores_tool_role_only() {
    let records = vec![alms_core::ToolCallRecord {
        seq: 0,
        role: alms_core::ToolCallRole::Tool,
        tool_name: Some("send_message".to_string()),
        tool_id: Some("tc_send".to_string()),
        params: None,
        result: Some(r#"{"ok":true}"#.to_string()),
        timestamp: chrono::Utc::now(),
    }];
    assert!(
        !dm::dm_tool_was_called(&records),
        "Tool-role records alone should not count — need an Assistant-role call too"
    );
}

/// Returns false when send_message has an Assistant record but no
/// corresponding Tool result (tool never executed).
#[test]
fn test_dm_tool_was_called_no_tool_result() {
    let records = vec![alms_core::ToolCallRecord {
        seq: 0,
        role: alms_core::ToolCallRole::Assistant,
        tool_name: Some("send_message".to_string()),
        tool_id: Some("tc_send".to_string()),
        params: Some(r#"{"to":"alice","message":"hi"}"#.to_string()),
        result: None,
        timestamp: chrono::Utc::now(),
    }];
    assert!(
        !dm::dm_tool_was_called(&records),
        "Assistant record without Tool result should not count"
    );
}

/// Returns false when both send_message and ignore_message were
/// recorded as Assistant calls but both received DM conflict error
/// results (neither actually executed). This is the critical scenario
/// from PR #365 + #369 interaction.
#[test]
fn test_dm_tool_was_called_conflict_batch_false_positive() {
    let conflict_error = format!("Error: {}", dm::DM_CONFLICT_MSG);
    let records = vec![
        // Assistant records for both tools (recorded before conflict check)
        alms_core::ToolCallRecord {
            seq: 0,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send".to_string()),
            params: Some(r#"{"to":"alice","message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore".to_string()),
            params: Some(r#"{"reason":"spam"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        // Tool results for both — both contain the conflict error
        alms_core::ToolCallRecord {
            seq: 2,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send".to_string()),
            params: None,
            result: Some(conflict_error.clone()),
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 3,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore".to_string()),
            params: None,
            result: Some(conflict_error),
            timestamp: chrono::Utc::now(),
        },
    ];
    assert!(
        !dm::dm_tool_was_called(&records),
        "Conflict-blocked tool calls should not count as successfully called — \
         the DM text-only retry must still trigger"
    );
}

/// Returns true when send_message was conflict-blocked but then
/// succeeded on a subsequent attempt (second batch after conflict).
#[test]
fn test_dm_tool_was_called_conflict_then_success() {
    let conflict_error = format!("Error: {}", dm::DM_CONFLICT_MSG);
    let records = vec![
        // First batch: conflict — both tools blocked
        alms_core::ToolCallRecord {
            seq: 0,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send_1".to_string()),
            params: Some(r#"{"to":"alice","message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore_1".to_string()),
            params: Some(r#"{"reason":"spam"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 2,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send_1".to_string()),
            params: None,
            result: Some(conflict_error.clone()),
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 3,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("tc_ignore_1".to_string()),
            params: None,
            result: Some(conflict_error),
            timestamp: chrono::Utc::now(),
        },
        // Second batch: LLM picked just send_message — succeeds
        alms_core::ToolCallRecord {
            seq: 4,
            role: alms_core::ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send_2".to_string()),
            params: Some(r#"{"to":"alice","message":"hello"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        },
        alms_core::ToolCallRecord {
            seq: 5,
            role: alms_core::ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("tc_send_2".to_string()),
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
        },
    ];
    assert!(
        dm::dm_tool_was_called(&records),
        "After conflict resolution, a successful send_message should be detected"
    );
}
