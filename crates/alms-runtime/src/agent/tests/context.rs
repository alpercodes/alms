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
async fn oversized_workspace_files_do_not_evict_session_history() {
    use crate::workspace::{AgentWorkspace, WorkspaceFile};
    use alms_core::config::ContextConfig;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    for (file, start, fill, end) in [
        (
            WorkspaceFile::Personality,
            "PERSONALITY_START",
            'p',
            "PERSONALITY_END",
        ),
        (WorkspaceFile::Goals, "GOALS_START", 'g', "GOALS_END"),
        (WorkspaceFile::User, "USER_START", 'u', "USER_END"),
        (
            WorkspaceFile::Memories,
            "MEMORIES_START",
            'm',
            "MEMORIES_END",
        ),
    ] {
        workspace
            .write_file_as_operator(
                file,
                &format!(
                    "{start}\n{}{end}",
                    format!("{fill}-workspace-entry\n").repeat(10_000)
                ),
            )
            .unwrap();
    }

    let runtime = AgentRuntime::new(
        AgentId::new(),
        AgentConfig {
            system_prompt: "Test agent.".into(),
            context_config: ContextConfig {
                strategy: "truncate".into(),
                max_input_tokens: 8_000,
                ..ContextConfig::default()
            },
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap(),
    )
    .unwrap()
    .with_workspace(workspace);

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "web-chat");
    for index in 0..8 {
        let role = if index % 2 == 0 {
            alms_session::Role::User
        } else {
            alms_session::Role::Assistant
        };
        session_manager
            .append_message(
                session.id,
                alms_session::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    role,
                    content: alms_session::Content::Text(format!(
                        "HISTORY_{index}_{}",
                        "h".repeat(900)
                    )),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: None,
                },
            )
            .unwrap();
    }

    let messages = runtime
        .build_context(&session_manager, &session.id, "web-chat", "continue")
        .await
        .unwrap();
    let system_prompt = messages[0].content_str();
    for file in ["personality.md", "goals.md", "user.md"] {
        assert!(
            system_prompt.contains(&format!("{file} truncated:")),
            "the system prompt should identify its partial {file} view"
        );
    }
    assert!(
        system_prompt.contains("Older memories truncated:"),
        "memories should keep their existing tail-window marker"
    );

    assert!(
        messages
            .iter()
            .any(|message| message.content_str().contains("HISTORY_0_")),
        "the oldest history should remain when one workspace file is oversized"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content_str().contains("HISTORY_7_")),
        "the newest history should remain when one workspace file is oversized"
    );
}

#[tokio::test]
async fn tool_loop_rebuild_reuses_the_initial_workspace_budget() {
    use crate::workspace::{AgentWorkspace, WorkspaceFile};
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(
            WorkspaceFile::Personality,
            &format!("PERSONALITY_START\n{}\nPERSONALITY_END", "p".repeat(1600)),
        )
        .unwrap();
    workspace
        .write_file_as_operator(
            WorkspaceFile::Goals,
            "GOALS_START\nship the feature\nGOALS_END",
        )
        .unwrap();
    workspace
        .write_file_as_operator(
            WorkspaceFile::Memories,
            &format!("MEMORIES_START\n{}\nMEMORIES_END", "m".repeat(900)),
        )
        .unwrap();

    let runtime = AgentRuntime::new(
        AgentId::new(),
        AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap(),
    )
    .unwrap()
    .with_workspace(workspace);

    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "web-chat");
    for index in 0..300 {
        session_manager
            .append_message(
                session.id,
                alms_session::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: if index % 2 == 0 {
                        alms_session::Role::User
                    } else {
                        alms_session::Role::Assistant
                    },
                    content: alms_session::Content::Text(format!(
                        "HISTORY_{index}_{}",
                        "h".repeat(1500)
                    )),
                    timestamp: alms_core::Timestamp::now(),
                    metadata: None,
                },
            )
            .unwrap();
    }

    let built = runtime
        .build_context_with_budget(&session_manager, &session.id, "web-chat", "continue")
        .await
        .unwrap();
    let workspace_budget_bytes = built.workspace_budget_bytes;
    let mut messages = built.messages;
    let mut previous = None;

    for batch in 1..=3 {
        messages.push(LlmMessage::assistant(format!("tool batch {batch}")));
        messages.push(LlmMessage::tool_result(
            format!("call_{batch}"),
            "r".repeat(3000),
        ));
        runtime.rebuild_system_prompt_for_tool_loop_with_budget(
            &mut messages,
            true,
            None,
            workspace_budget_bytes,
        );

        let system_prompt = messages[0].content_str().to_string();
        for marker in ["PERSONALITY_END", "GOALS_END", "MEMORIES_END"] {
            assert!(
                system_prompt.contains(marker),
                "the stable workspace view must contain {marker}"
            );
        }
        if let Some(previous) = previous.replace(system_prompt.clone()) {
            assert_eq!(
                previous, system_prompt,
                "the system block must remain stable across tool batches"
            );
        }
    }
}

#[tokio::test]
async fn normal_workspace_files_are_byte_stable_across_build_and_rebuild() {
    use crate::workspace::{AgentWorkspace, WorkspaceFile};
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    let personality = "p".repeat(1684);
    let goals = "g".repeat(39);
    let memories = "m".repeat(963);
    workspace
        .write_file_as_operator(WorkspaceFile::Personality, &personality)
        .unwrap();
    workspace
        .write_file_as_operator(WorkspaceFile::Goals, &goals)
        .unwrap();
    workspace
        .write_file_as_operator(WorkspaceFile::Memories, &memories)
        .unwrap();

    let runtime = AgentRuntime::new(
        AgentId::new(),
        AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap(),
    )
    .unwrap()
    .with_workspace(workspace);
    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "web-chat");
    let built = runtime
        .build_context_with_budget(&session_manager, &session.id, "web-chat", "continue")
        .await
        .unwrap();
    let expected_initial = format!(
        "{}\n\n{}\n\n## Current Goals\n{}\n\n## Memories\n{}",
        runtime.config.system_prompt, personality, goals, memories
    );
    assert_eq!(built.messages[0].content_str(), expected_initial);

    let mut messages = built.messages;
    for batch in 1..=2 {
        messages.push(LlmMessage::assistant(format!("tool batch {batch}")));
        messages.push(LlmMessage::tool_result(format!("call_{batch}"), "ok"));
        runtime.rebuild_system_prompt_for_tool_loop_with_budget(
            &mut messages,
            true,
            None,
            built.workspace_budget_bytes,
        );
        assert_eq!(
            messages[0].content_str(),
            format!("{expected_initial}\n\n{}", runtime.config.prompts.tool_loop),
            "unchanged normal-size files should retain the same bytes on every rebuild"
        );
    }
}

/// One streamed OpenAI-style completion that calls `echo` once.
fn echo_tool_call_sse(call_id: &str) -> String {
    let chunk = serde_json::json!({
        "id": call_id, "object": "chat.completion.chunk", "created": 1, "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0, "id": call_id, "type": "function",
                    "function": { "name": "echo", "arguments": "{\"message\":\"ok\"}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// One streamed OpenAI-style completion that ends the run with `text`.
fn final_text_sse(text: &str) -> String {
    let chunk = serde_json::json!({
        "id": "final", "object": "chat.completion.chunk", "created": 2, "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// An agent under budget pressure: an 8,000-token window and a 20 KB
/// one-line personality that cannot fit whole. The base and `tool_loop`
/// prompts are fixed here so the budget arithmetic does not move when the
/// shipped prompt files are edited. Keep the returned directory alive for as
/// long as the runtime is used.
fn budget_pressure_runtime(agent_id: AgentId, llm: LlmConfig) -> (tempfile::TempDir, AgentRuntime) {
    use crate::workspace::{AgentWorkspace, WorkspaceFile};
    use alms_core::config::ContextConfig;

    let dir = tempfile::tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(WorkspaceFile::Personality, &"p".repeat(20_000))
        .unwrap();

    let runtime = AgentRuntime::new(
        agent_id,
        AgentConfig {
            system_prompt: "Budget-pressure base prompt.".into(),
            prompts: SystemPrompts {
                tool_loop: "Continue.".into(),
            },
            context_config: ContextConfig {
                max_input_tokens: 8_000,
                ..ContextConfig::default()
            },
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(llm).unwrap(),
    )
    .unwrap()
    .with_workspace(workspace);
    (dir, runtime)
}

/// S8 (#192 review): `run()` must carry the budget its context was built
/// with into every tool-loop rebuild. The rebuild tests above call
/// `rebuild_system_prompt_for_tool_loop_with_budget` directly, so none of
/// them sees `finish_run_with_workspace_budget` drop the budget; with
/// `usize::MAX` passed there instead, every rebuild injects the whole 20 KB
/// personality and overflows the window the first request was budgeted for.
#[tokio::test]
async fn run_carries_the_initial_workspace_budget_into_tool_loop_rebuilds() {
    use alms_test_support::{Canned, ScriptedLlm};

    let llm = ScriptedLlm::in_order(vec![
        Canned::sse(echo_tool_call_sse("c1")),
        Canned::sse(echo_tool_call_sse("c2")),
        Canned::sse(final_text_sse("done")),
    ])
    .await;
    let (_dir, runtime) = budget_pressure_runtime(
        AgentId::new(),
        LlmConfig {
            base_url: llm.base_url(),
            api_key: "test-key".to_string(),
            default_model: "test-model".to_string(),
            timeout_secs: 5,
            stream_chunk_timeout_secs: 5,
            ..LlmConfig::default()
        },
    );
    let session_manager = SessionManager::new(SessionConfig::default());

    let output = runtime
        .run(&session_manager, "web-chat", "continue")
        .await
        .unwrap();
    assert_eq!(output.response, "done");

    let systems: Vec<String> = llm
        .request_bodies()
        .await
        .iter()
        .map(|body| {
            serde_json::from_str::<serde_json::Value>(body).unwrap()["messages"][0]["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(systems.len(), 3, "two tool batches, then the final turn");
    assert!(
        systems[0].contains("personality.md truncated"),
        "precondition: the first request is under budget pressure: {}",
        systems[0]
    );
    for (batch, rebuilt) in systems[1..].iter().enumerate() {
        assert_eq!(
            *rebuilt,
            format!("{}\n\n{}", systems[0], runtime.config.prompts.tool_loop),
            "rebuild after batch {} must reuse the first request's workspace budget",
            batch + 1
        );
    }
}

/// S10 (#192 review): under budget pressure, a turn's input length no longer
/// moves a truncated file's cut, so consecutive turns of a session send the
/// same system block and can hit the prompt cache. The personality is one
/// unbroken line, so without the 1 KiB rounding the cut would follow the
/// budget byte for byte: these two inputs differ by 37 tokens, which moves
/// the unrounded budget by 57 bytes.
#[tokio::test]
async fn different_input_lengths_get_a_byte_identical_system_prompt_under_pressure() {
    let (_dir, runtime) = budget_pressure_runtime(
        AgentId::new(),
        LlmConfig {
            mock: true,
            ..LlmConfig::default()
        },
    );
    let session_manager = SessionManager::new(SessionConfig::default());
    let session = session_manager.get_or_create(runtime.agent_id, "web-chat");

    let short_input = "hi";
    let long_input = "Please pick the review back up where you left it, and walk me through \
                      what still must change before it can land.";
    assert_eq!((short_input.len(), long_input.len()), (2, 112));

    let mut systems = Vec::new();
    for input in [short_input, long_input] {
        let built = runtime
            .build_context_with_budget(&session_manager, &session.id, "web-chat", input)
            .await
            .unwrap();
        systems.push(built.messages[0].content_str().to_string());
    }

    assert!(
        systems[0].contains("personality.md truncated"),
        "precondition: the personality is under budget pressure: {}",
        systems[0]
    );
    assert_eq!(
        systems[0], systems[1],
        "a 2-byte and a 112-byte input must cut the personality in the same place"
    );
}

/// S10, second half: an episodic block is charged at `run_summary_budget`,
/// not at its length. The block is rebuilt from the other sessions'
/// summaries on every run, so it grows and shrinks between turns; charged at
/// its length, a ~1,000-token difference here would move the budget across
/// a 1 KiB step and the personality's cut with it.
#[tokio::test]
async fn episodic_block_length_does_not_move_the_workspace_cut() {
    use alms_session::Session;
    use alms_session::sqlite::SqliteStore;

    let agent_id = AgentId::new();
    let (_dir, runtime) = budget_pressure_runtime(
        agent_id,
        LlmConfig {
            mock: true,
            ..LlmConfig::default()
        },
    );

    let short_summary = "Discussed the release checklist.".to_string();
    let long_summary = format!(
        "Discussed the release checklist. {}",
        "Details. ".repeat(330)
    );
    let mut built_rows = Vec::new();
    for summary in [&short_summary, &long_summary] {
        let store = SqliteStore::open_in_memory().unwrap();
        let other = Session::new(agent_id, "web-chat-other");
        let current = Session::new(agent_id, "web-chat");
        store.save_session(&other).unwrap();
        store.save_session(&current).unwrap();
        store
            .upsert_session_summary(agent_id, other.id, summary, None, Some("User chat"))
            .unwrap();
        let manager = SessionManager::with_store(SessionConfig::default(), store).unwrap();

        let built = runtime
            .build_context_with_budget(&manager, &current.id, "web-chat", "hi")
            .await
            .unwrap();
        assert!(
            built
                .messages
                .iter()
                .any(|m| m.content_str().contains(summary.as_str())),
            "precondition: the episodic block carries this row's summary"
        );
        built_rows.push((
            built.workspace_budget_bytes,
            built.messages[0].content_str().to_string(),
        ));
    }

    assert!(
        built_rows[0].1.contains("personality.md truncated"),
        "precondition: the personality is under budget pressure: {}",
        built_rows[0].1
    );
    assert_eq!(
        built_rows[0], built_rows[1],
        "a short and a long episodic block must leave the workspace budget and cut unchanged"
    );
}

#[test]
fn workspace_truncation_warning_is_reported_once_per_run() {
    use crate::workspace::{AgentWorkspace, WorkspaceFile};
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(WorkspaceFile::Personality, &"p".repeat(5000))
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = AgentRuntime {
        event_sender: Some(tx),
        agent_name: Some("alice".to_string()),
        ..AgentRuntime::new(
            AgentId::new(),
            AgentConfig {
                system_prompt: "base".into(),
                sandbox_root: "".into(),
                ..AgentConfig::default()
            },
            LlmClient::new(LlmConfig {
                mock: true,
                ..LlmConfig::default()
            })
            .unwrap(),
        )
        .unwrap()
        .with_workspace(workspace)
    };

    for _ in 0..3 {
        let prompt = runtime.assemble_system_prompt_with_budget("base", true, 1000);
        assert!(prompt.contains("personality.md truncated:"));
    }

    let RuntimeEvent::Warning {
        code,
        message,
        source_agent,
    } = rx
        .try_recv()
        .expect("the first partial view emits a warning")
    else {
        panic!("expected a workspace warning event");
    };
    assert_eq!(code, "WORKSPACE_PROMPT_TRUNCATED");
    assert!(message.contains("alice"));
    assert!(message.contains("personality.md"));
    assert!(source_agent.is_none());
    assert!(rx.try_recv().is_err(), "rebuilds do not repeat the warning");

    runtime.workspace.as_ref().unwrap().forget_shown_files();
    let _ = runtime.assemble_system_prompt_with_budget("base", true, 1000);
    assert!(matches!(
        rx.try_recv(),
        Ok(RuntimeEvent::Warning { code, .. }) if code == "WORKSPACE_PROMPT_TRUNCATED"
    ));
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
/// four-layer assembly and guards
/// `rebuild_system_prompt_for_tool_loop_with_budget` against accidental
/// reordering.
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
    // The one-line personality fits any budget, so none is imposed.
    runtime.rebuild_system_prompt_for_tool_loop_with_budget(
        &mut messages,
        false,
        Some("alice"),
        usize::MAX,
    );
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

/// An `episodic:` run gets the same system prompt every other non-user-facing
/// run gets — personality, goals and memories, **without** `## About the
/// User` — and the shown-view guard follows: a default (replacing)
/// `workspace_write` on `user` in that run is refused as `never_shown`,
/// where the user-facing control run on the same workspace is allowed.
///
/// `episodic:` is reserved for the summariser's internal sessions and every
/// other classifier already treats it as internal (`classify_session_type`,
/// `derive_source_label`, the gateway's `INTERNAL_SESSION_PREFIXES`). The
/// runtime's `is_user_facing_context` was written three days before the
/// prefix existed (#372) and is default-open, so an `episodic:` run fell
/// through to injection. This goes through `build_context` rather than the
/// classifier so it pins the prompt and the guard, not the boolean: the
/// control run proves the omission is the gate and not a missing file.
#[tokio::test]
async fn test_episodic_context_omits_user_md_from_the_system_prompt() {
    use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(WorkspaceFile::Personality, "I am Alice.")
        .unwrap();
    workspace
        .write_file_as_operator(
            WorkspaceFile::User,
            "Name: Alper. Prefers concise answers. USER_MD_MARKER",
        )
        .unwrap();

    let runtime = AgentRuntime::new(
        AgentId::new(),
        AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        },
        LlmClient::new(LlmConfig {
            mock: true,
            ..LlmConfig::default()
        })
        .unwrap(),
    )
    .unwrap()
    .with_workspace(workspace);
    let session_manager = SessionManager::new(SessionConfig::default());

    // The control runs FIRST: if the workspace never recorded `user.md` at
    // all, the episodic row's refusal would pass for the wrong reason.
    for (context_id, user_facing, expected_write) in [
        ("web-chat-1", true, CheckedWrite::Written),
        (
            "episodic:8f2c1a4e-0000-4000-8000-000000000000",
            false,
            CheckedWrite::Refused(RefusedWrite::NeverShown),
        ),
    ] {
        // Each run starts from a fresh record (`build_context` forgets the
        // previous run's views), so the guard's answer is this run's alone.
        let session = session_manager.get_or_create(runtime.agent_id, context_id);
        let messages = runtime
            .build_context(&session_manager, &session.id, context_id, "hi")
            .await
            .unwrap();
        let prompt = messages[0].content.clone().unwrap_or_default();

        assert!(
            prompt.contains("I am Alice."),
            "[{context_id}] the rest of the workspace is always injected. Got:\n{prompt}"
        );
        assert_eq!(
            prompt.contains("## About the User") && prompt.contains("USER_MD_MARKER"),
            user_facing,
            "[{context_id}] user.md injected iff the context is user-facing. Got:\n{prompt}"
        );

        // The guard follows the prompt: a default-mode `workspace_write` on
        // `user` replaces the file only in the run that was shown it.
        let workspace = runtime.workspace.as_ref().expect("workspace attached");
        assert_eq!(
            workspace
                .write_file_checked(WorkspaceFile::User, "Name: Someone Else")
                .unwrap(),
            expected_write,
            "[{context_id}]"
        );
        // The file agrees with the verdict. A refusal that had already
        // renamed the staging file into place would satisfy the return
        // value and lose the data anyway.
        let on_disk = workspace.read_file(WorkspaceFile::User).unwrap_or_default();
        if user_facing {
            assert_eq!(on_disk, "Name: Someone Else", "[{context_id}]");
        } else {
            assert!(
                on_disk.contains("USER_MD_MARKER"),
                "[{context_id}] a refused write must leave the file untouched; got: {on_disk:?}"
            );
        }
        // Restore for the next row.
        workspace
            .write_file_as_operator(
                WorkspaceFile::User,
                "Name: Alper. Prefers concise answers. USER_MD_MARKER",
            )
            .unwrap();
    }
}
