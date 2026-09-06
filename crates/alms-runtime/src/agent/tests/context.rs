// SPDX-License-Identifier: Apache-2.0

//! Context-window assembly: `build_context`, DM perspective mapping, and system-prompt layer order.

use super::base_runtime;
use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AgentId;
use alms_session::{SessionConfig, SessionManager};

#[tokio::test]
async fn test_stream_llm_call_emits_token_deltas() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let runtime = AgentRuntime {
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(config).unwrap())
    };

    let request =
        CompletionRequest::new("test").with_messages(vec![LlmMessage::user("hello world")]);

    let emitted = std::sync::atomic::AtomicBool::new(false);
    let activity = crate::agent::loop_impl::ActivityClock::new();
    let result = runtime
        .stream_llm_call(request, &emitted, &activity)
        .await
        .unwrap();

    // Content should be the reassembled mock response
    assert_eq!(result.content.as_deref(), Some("[mock] hello world"));
    // No tool calls from mock
    assert!(result.tool_calls.is_none());
    // Mock stream doesn't emit reasoning_content
    assert!(result.reasoning.is_none());
    // The stream emitted visible token deltas, so the emitted-flag is set
    // (the buffered-fallback reset/re-emit reconciliation reads this).
    assert!(
        emitted.load(std::sync::atomic::Ordering::Relaxed),
        "stream_llm_call must flag that it emitted token deltas"
    );

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
    let runtime = base_runtime(LlmClient::new(LlmConfig::default()).unwrap());

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
        agent_name: Some("bob".to_string()),
        ..base_runtime(LlmClient::new(LlmConfig::default()).unwrap())
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
        agent_name: Some("bob".to_string()),
        ..base_runtime(LlmClient::new(LlmConfig::default()).unwrap())
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

    // Two consecutive user turns (history "Hello" + current input "hi")
    // merge into a single user message under the canonical invariant.
    // Expected shape: [system, user(merged)].
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    assert!(messages[1].content_str().contains("Hello"));
    assert!(messages[1].content_str().contains("hi"));
}

// ---- System prompt assembly order regression tests ----
//
// These tests pin the layer order `base -> workspace -> tool_loop ->
// dm_addendum` documented in `docs/system-prompts.md` § "Prompt Assembly
// Order". The base prompt comes first (foundational role/identity), the
// workspace prefix (agent-specific personalization) follows, and any
// stage-specific addenda (tool_loop continuation, DM recipient hint) come
// after that. This order matches common LLM prompting practice and keeps
// the stable base prompt at the head of the system block — which improves
// Anthropic prompt-cache hit rates when workspace content drifts.

/// Non-DM, non-tool-loop turn: the base prompt comes first, then the
/// workspace prefix follows. This is the canonical assembly produced by
/// `assemble_system_prompt`.
#[tokio::test]
async fn test_system_prompt_order_base_before_workspace() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().to_path_buf();
    let agent_meta = agents_dir.join("alice");
    std::fs::create_dir_all(&agent_meta).unwrap();
    std::fs::write(
        agent_meta.join("personality.md"),
        "I am Alice, a concise coding assistant.",
    )
    .unwrap();
    std::fs::write(agent_meta.join("goals.md"), "Help with Rust.").unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        // A distinctive base prompt we can search for unambiguously.
        system_prompt: "BASE_PROMPT_MARKER: foundational identity.".to_string(),
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .unwrap()
    .with_workspace(AgentWorkspace::new(&agents_dir, "alice"));

    let assembled = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);

    let base_pos = assembled
        .find("BASE_PROMPT_MARKER")
        .expect("assembled prompt must contain the base prompt marker");
    let personality_pos = assembled
        .find("Alice, a concise coding assistant")
        .expect("assembled prompt must contain the workspace personality");
    let goals_pos = assembled
        .find("## Current Goals")
        .expect("assembled prompt must contain the workspace goals heading");

    assert!(
        base_pos < personality_pos,
        "Base prompt must come before workspace personality. Got:\n{assembled}"
    );
    assert!(
        personality_pos < goals_pos,
        "Workspace internal order (personality -> goals) must be preserved. Got:\n{assembled}"
    );
}

/// DM session, tool-loop iteration: the order must be
/// `base -> workspace -> tool_loop -> dm_addendum`. This pins the full
/// four-layer assembly and guards `rebuild_system_prompt_for_tool_loop`
/// against accidental reordering.
#[tokio::test]
async fn test_system_prompt_order_dm_tool_loop_layers() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().to_path_buf();
    let agent_meta = agents_dir.join("bob");
    std::fs::create_dir_all(&agent_meta).unwrap();
    std::fs::write(
        agent_meta.join("personality.md"),
        "I am Bob, a methodical reviewer.",
    )
    .unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        system_prompt: "BASE_PROMPT_MARKER: bob's identity.".to_string(),
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_workspace(AgentWorkspace::new(&agents_dir, "bob"));

    // Simulate the tool-loop rebuild path with a DM peer.
    let mut messages = vec![LlmMessage::system("placeholder".to_string())];
    // Non-user-facing context: `user.md` is omitted but personality is included.
    runtime.rebuild_system_prompt_for_tool_loop(&mut messages, false, Some("alice"));
    let assembled = messages[0].content.as_deref().unwrap_or("");

    let base_pos = assembled
        .find("BASE_PROMPT_MARKER")
        .expect("rebuilt prompt must contain the base prompt marker");
    let personality_pos = assembled
        .find("Bob, a methodical reviewer")
        .expect("rebuilt prompt must contain the workspace personality");
    // The tool_loop prompt content is loaded from `prompts/tool_loop.md`;
    // search for its actual configured value to avoid coupling to file
    // contents.
    let tool_loop_pos = assembled
        .find(&runtime.config.prompts.tool_loop)
        .expect("rebuilt prompt must contain the tool_loop continuation guidance");
    let dm_pos = assembled
        .find("direct message from agent \"alice\"")
        .expect("rebuilt prompt must contain the DM addendum for peer 'alice'");

    assert!(
        base_pos < personality_pos,
        "Order layer 1->2 violated (base before workspace). Got:\n{assembled}"
    );
    assert!(
        personality_pos < tool_loop_pos,
        "Order layer 2->3 violated (workspace before tool_loop). Got:\n{assembled}"
    );
    assert!(
        tool_loop_pos < dm_pos,
        "Order layer 3->4 violated (tool_loop before dm_addendum). Got:\n{assembled}"
    );
}
