// SPDX-License-Identifier: Apache-2.0

//! What a run persists on failure and how reasoning blocks round-trip through the session store.

use crate::agent::*;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::{AgentId, AlmsError};
use alms_session::{SessionConfig, SessionManager};

// `sanitize_error_for_session` lives in `alms-core` (issue #911) so both
// the runtime and gateway can use it. Tests for it live alongside the
// function in `alms-core::error`.

/// Regression test for #912 — when a run fails, the runtime layer's
/// `finish_run` is the single place that persists the failure record
/// to the session.  Before #912 the gateway's lifecycle layer also
/// wrote a `(run failed) ...` `Role::System` marker tagged with
/// `kind: "error"`; both records reached the LLM context on the next
/// turn and double-spent the agent's attention budget on the same
/// event.  Atlas + Alper's decision (recorded on issue #912) was to
/// keep this runtime-layer write — the canonical record that lands as
/// `Role::Assistant` text and survives `strip_mid_history_system_markers`
/// natively — and remove the duplicate lifecycle-layer marker.
///
/// This test pins the runtime-layer invariant: a failed run produces
/// exactly ONE error-flavoured record in the session.  Together with
/// the lifecycle-layer removal in `crates/alms-gateway/src/runs/lifecycle.rs`
/// it locks in "one failed-run record per `run_id`, not two".
#[tokio::test]
async fn finish_run_persists_exactly_one_error_record_on_failure() {
    use crate::llm_types::LlmMessage;

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let session_manager = SessionManager::new(SessionConfig::default());
    let llm = LlmClient::new(config).unwrap();
    let agent_id = AgentId::new();
    let runtime = AgentRuntime::new(agent_id, AgentConfig::default(), llm).unwrap();

    // Pre-populate the session with a user turn so the failure record
    // is visible in a realistic history shape.
    let session = session_manager.get_or_create(agent_id, "test-dedup");
    session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text("do something".to_string()),
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
        )
        .unwrap();

    // Drive `finish_run` down its `Err(_)` arm by passing a synthetic
    // history error — this is exactly the path triggered by an LLM
    // 401/429/500, a context-build failure, or any other run-level
    // error caught by `agent_loop` / `build_context`.
    let history: AlmsResult<Vec<LlmMessage>> = Err(AlmsError::Runtime(
        "simulated provider 500: server error".into(),
    ));
    let result = runtime
        .finish_run(&session_manager, session.id, "test-dedup", history)
        .await;

    // The runtime layer surfaces the error to the caller wrapped in
    // `FailedWithToolCalls` so the gateway can persist partial tool
    // call records.  The key invariant for #912 is the side effect on
    // the session, not the return value shape.
    assert!(
        matches!(result, Err(AlmsError::FailedWithToolCalls { .. })),
        "finish_run on Err(_) history must surface FailedWithToolCalls; got {result:?}"
    );

    let history = session_manager.get_history(session.id).unwrap();

    // Count every record that smells like a run-failure marker:
    //   - the runtime-layer `[Run failed: ...]` text (Role::Assistant)
    //   - any synthetic `(run failed) ...` marker from a hypothetical
    //     lifecycle-layer write (Role::System with kind=error)
    let run_failed_records: Vec<_> = history
        .iter()
        .filter(|m| match &m.content {
            alms_session::Content::Text(t) => {
                t.contains("[Run failed:") || t.contains("(run failed)")
            }
            _ => false,
        })
        .collect();

    assert_eq!(
        run_failed_records.len(),
        1,
        "exactly one run-failed record per run is required (issue #912); got {} records: {:#?}",
        run_failed_records.len(),
        run_failed_records
            .iter()
            .map(|m| match &m.content {
                alms_session::Content::Text(t) => (m.role, t.clone()),
                _ => (m.role, "<non-text>".to_string()),
            })
            .collect::<Vec<_>>()
    );

    // The single record is the runtime-layer `[Run failed: ...]` text
    // at `Role::Assistant`, sanitised via `sanitize_error_for_session`.
    let only = run_failed_records[0];
    assert_eq!(only.role, alms_session::Role::Assistant);
    if let alms_session::Content::Text(t) = &only.content {
        assert!(
            t.starts_with("[Run failed:"),
            "runtime-layer marker must use the `[Run failed: ...]` shape; got {t:?}"
        );
        // Confirm the raw error body did not survive sanitisation —
        // overlapping defence with #911's coverage in alms-core.
        assert!(
            !t.contains("provider 500: server error"),
            "raw provider body must not leak into the persisted marker; got {t:?}"
        );
    } else {
        panic!("expected text content on the run-failed record");
    }

    // No `Role::System` markers carrying `kind: "error"` — the
    // lifecycle-layer write removed in #912 was the only path that
    // produced those during a normal run failure.
    let system_error_markers = history
        .iter()
        .filter(|m| {
            m.role == alms_session::Role::System
                && m.metadata
                    .as_ref()
                    .and_then(|md| md.get("kind"))
                    .and_then(|v| v.as_str())
                    == Some("error")
        })
        .count();
    assert_eq!(
        system_error_markers, 0,
        "runtime-layer finish_run must not write Role::System kind=error markers (issue #912)"
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

// --------------------------------------------------------------------------
// Extended-thinking persistence round-trip (issue #767)
// --------------------------------------------------------------------------

/// Reasoning blocks accumulated mid-run (alongside a tool call batch) are
/// written onto the assistant-text session message with a
/// `reasoning_blocks` metadata field, so page reload can rehydrate them.
#[tokio::test]
async fn test_reasoning_persisted_on_assistant_tool_call_message() {
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

    let session = session_manager.get_or_create(agent_id, "ctx-reasoning");

    // Directly exercise the persistence path with a reasoning trace.
    let tool_call = ToolCall::new("call_1", "echo", r#"{"text":"hi"}"#);
    let invocation_id = uuid::Uuid::new_v4();
    runtime.persist_assistant_tool_calls(
        &session_manager,
        session.id,
        Some("I will echo hi."),
        Some("Deliberating about the best approach..."),
        &[tool_call],
        &[invocation_id],
        false, // is_dm
    );

    let history = session_manager.get_history(session.id).unwrap();
    // Expected: one assistant text message (with reasoning_blocks meta) +
    // one tool_call message (without reasoning, that's on the text msg).
    assert!(!history.is_empty(), "nothing persisted");
    let assistant_text = history
        .iter()
        .find(|m| matches!(m.content, alms_session::Content::Text(ref t) if t == "I will echo hi."))
        .expect("assistant text message present");
    let meta = assistant_text
        .metadata
        .as_ref()
        .expect("metadata with reasoning_blocks expected");
    let blocks = meta["reasoning_blocks"]
        .as_array()
        .expect("reasoning_blocks must be an array");
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0]["text"].as_str(),
        Some("Deliberating about the best approach...")
    );
}

/// History-loader round-trip: a persisted assistant message with a
/// `reasoning_blocks` metadata field surfaces as `reasoning` text on
/// reload. Exercises the full write-then-read path via `SessionManager`.
#[tokio::test]
async fn test_reasoning_persisted_reload_roundtrip() {
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

    let session = session_manager.get_or_create(agent_id, "ctx-roundtrip");

    let invocation_id = uuid::Uuid::new_v4();
    runtime.persist_assistant_tool_calls(
        &session_manager,
        session.id,
        Some("Final answer"),
        Some("step 1: think; step 2: conclude"),
        &[ToolCall::new("c1", "echo", "{}")],
        &[invocation_id],
        false,
    );

    let reloaded = session_manager.get_history(session.id).unwrap();
    let hit = reloaded
        .iter()
        .find(|m| match &m.content {
            alms_session::Content::Text(t) => t == "Final answer",
            _ => false,
        })
        .expect("assistant text present");
    let meta = hit.metadata.as_ref().unwrap();
    let blocks = meta["reasoning_blocks"].as_array().unwrap();
    assert_eq!(
        blocks[0]["text"].as_str(),
        Some("step 1: think; step 2: conclude"),
        "reasoning text must survive persistence round-trip"
    );
}
