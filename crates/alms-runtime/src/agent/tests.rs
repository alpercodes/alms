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
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let request =
        CompletionRequest::new("test").with_messages(vec![LlmMessage::user("hello world")]);

    let emitted = std::sync::atomic::AtomicBool::new(false);
    let activity = super::loop_impl::ActivityClock::new();
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
    let runtime = AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig::default(),
        llm: LlmClient::new(LlmConfig::default()).unwrap(),
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
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
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: Some("bob".to_string()),
        dm_implicit_reply: false,
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
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: Some("bob".to_string()),
        dm_implicit_reply: false,
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

#[tokio::test]
async fn test_guarded_posture_sequential_approvals() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // Use `math` — a tool that is NOT auto-approved — so the approval
    // workflow is exercised. (Auto-approved tools like `echo` skip approval.)
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
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
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_calls = vec![
        ToolCall::new("tc1", "math", r#"{"operation":"add","a":1,"b":2}"#),
        ToolCall::new("tc2", "math", r#"{"operation":"add","a":3,"b":4}"#),
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
                .execute_tool_call(tc, uuid::Uuid::new_v4(), &session_manager, session.id, None)
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

/// Regression test for #815: a user-denied approval under Guarded posture
/// must append an `AuditDecision::Deny` entry. Before the fix, the deny
/// branch of the approval gate emitted `tool_end` and returned the
/// `ToolExecution` error to the loop — but never called
/// `session_manager.append_audit`. That left a one-sided audit trail
/// (every approved call recorded, every denied call silently dropped),
/// which is useless for any forensic / compliance / post-incident review
/// that needs to answer "did the operator deny X at this time?".
///
/// Approve-side audit emission remains covered by other tests in this
/// module; the explicit positive assertion here is for the deny path.
#[tokio::test]
async fn test_denied_approval_appends_deny_audit_entry() {
    use alms_core::AuditDecision;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // `math` is NOT auto-approved, so the approval gate fires.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
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
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_call = ToolCall::new("tc-deny", "math", r#"{"operation":"add","a":1,"b":2}"#);

    // Spawn a handler that denies the approval (`decision_tx.send(false)`).
    let deny_handler = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let RuntimeEvent::ApprovalRequired { decision_tx, .. } = event {
                decision_tx.send(false).unwrap();
                break;
            }
        }
    });

    let result = runtime
        .execute_tool_call(
            &tool_call,
            uuid::Uuid::new_v4(),
            &session_manager,
            session.id,
            None,
        )
        .await;

    deny_handler.await.unwrap();

    // #1109: the deny path returns the distinct `user_denied` result body
    // (NOT an `Err` and NOT the `{"error": ...}` shape used by real tool
    // runtime errors) so the persisted row and the next run's context
    // rebuild carry an unambiguous user-policy signal.
    match &result {
        Ok(value) => {
            assert_eq!(
                value.get("user_denied").and_then(|v| v.as_bool()),
                Some(true),
                "expected user_denied: true in denial result, got {:?}",
                value
            );
            assert_eq!(
                value.get("message").and_then(|v| v.as_str()),
                Some(crate::agent::loop_impl::USER_DENIED_MESSAGE),
                "expected the concise denial gloss, got {:?}",
                value
            );
            assert!(
                value.get("error").is_none(),
                "denial body must not collide with the `error` shape used \
                 by real tool errors, got {:?}",
                value
            );
        }
        other => panic!("expected Ok(user_denied result), got {:?}", other),
    }

    // The audit log must contain exactly one entry: a `Deny` decision for
    // the `math` tool, with the params we passed in and a denial-shaped
    // error string. Before the #815 fix this assertion would fail with
    // an empty audit log.
    let audit = session_manager.get_audit(session.id).unwrap();
    let deny_entries: Vec<_> = audit
        .iter()
        .filter(|e| matches!(e.decision, AuditDecision::Deny))
        .collect();
    assert_eq!(
        deny_entries.len(),
        1,
        "expected exactly one Deny audit entry for the user-denied approval, \
         got {} (full audit: {:#?})",
        deny_entries.len(),
        audit
    );
    let entry = deny_entries[0];
    assert_eq!(entry.tool, "math", "deny entry must record tool name");
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|e| e.contains("denied by user")),
        "deny entry must record denial reason, got {:?}",
        entry.error
    );
    assert!(
        entry.result.is_none(),
        "deny entry must not carry a result payload, got {:?}",
        entry.result
    );
    // `params` should be the parsed JSON form of the tool call's arguments.
    assert_eq!(
        entry.params,
        serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        "deny entry must record the parsed tool arguments"
    );
    // No `Allow` entry should sneak in for a denied call.
    assert!(
        !audit
            .iter()
            .any(|e| matches!(e.decision, AuditDecision::Allow)),
        "denied call must not produce an Allow audit entry"
    );
}

/// Regression test for #1109: a user denial must behave like a cancel,
/// not a retryable tool error.
///
/// Denies the first approval of a two-tool Guarded batch with a live
/// cancel token: the batch must unwind with `Err(Cancelled)`, the second
/// tool must never prompt, the denied tool's `ToolEnd` must carry the
/// `user_denied` body, and the denial must reach the per-run records via
/// the cancel-unwind persistence pass (#1090) so the next run's rebuild
/// replays it instead of an `INTERRUPTED_TOOL_RESULT` marker.
#[tokio::test]
async fn test_denied_approval_cancels_run_and_stops_batch() {
    use tokio_util::sync::CancellationToken;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // `math` is NOT auto-approved, so the approval gate fires.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");
    let cancel_token = CancellationToken::new();

    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: Some(cancel_token.clone()),
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_calls = vec![
        ToolCall::new("tc-deny-1", "math", r#"{"operation":"add","a":1,"b":2}"#),
        ToolCall::new("tc-deny-2", "math", r#"{"operation":"add","a":3,"b":4}"#),
    ];
    let invocation_ids = vec![uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];

    // Deny every approval that arrives; count them and capture ToolEnds.
    let handler = tokio::spawn(async move {
        let mut approval_count = 0u32;
        let mut tool_ends: Vec<(bool, serde_json::Value)> = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                RuntimeEvent::ApprovalRequired { decision_tx, .. } => {
                    approval_count += 1;
                    let _ = decision_tx.send(false);
                }
                RuntimeEvent::ToolEnd { ok, result, .. } => {
                    tool_ends.push((ok, result));
                }
                _ => {}
            }
        }
        (approval_count, tool_ends)
    });

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &invocation_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // (1) Denial unwinds the batch through the cancel path.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected Err(Cancelled) after deny, got {:?}",
        result
    );
    assert!(
        cancel_token.is_cancelled(),
        "deny must cancel the run's own token so the gateway terminal \
         arm drives the run to `cancelled`"
    );

    drop(runtime);
    let (approval_count, tool_ends) = handler.await.unwrap();

    // (2) The second tool never fired its approval gate.
    assert_eq!(
        approval_count, 1,
        "denial must stop the batch before the next tool prompts"
    );

    // (3) Exactly one ToolEnd: the denied tool, ok=false, user_denied body.
    assert_eq!(
        tool_ends.len(),
        1,
        "expected a single ToolEnd (denied tool only), got {:?}",
        tool_ends
    );
    let (ok, body) = &tool_ends[0];
    assert!(!ok, "denied ToolEnd must carry ok=false");
    assert_eq!(
        body.get("user_denied").and_then(|v| v.as_bool()),
        Some(true),
        "denied ToolEnd must carry user_denied: true, got {:?}",
        body
    );
    assert!(
        body.get("error").is_none(),
        "denied ToolEnd must not use the `error` key, got {:?}",
        body
    );

    // (4) The denial result is persisted to the per-run records.
    let denied_record = tool_call_records
        .iter()
        .find(|r| {
            r.role == alms_core::ToolCallRole::Tool && r.tool_id.as_deref() == Some("tc-deny-1")
        })
        .expect("denied tool must have a persisted Tool-role record");
    assert!(
        denied_record
            .result
            .as_deref()
            .is_some_and(|s| s.contains("user_denied")),
        "persisted denial record must carry the user_denied body, got {:?}",
        denied_record.result
    );
    // The second tool never ran — no Tool-role record for it.
    assert!(
        !tool_call_records
            .iter()
            .any(|r| r.role == alms_core::ToolCallRole::Tool
                && r.tool_id.as_deref() == Some("tc-deny-2")),
        "the never-executed second tool must not have a result record"
    );
}

/// Regression test for #816: cancellation during approval-wait must emit a
/// matching `ToolEnd` for the `ToolStart` already fired before the approval
/// gate. Without that terminal event the UI's tool row stays in the spinner
/// state until the user reloads — live render diverges from persisted/reload
/// render, breaking the same invariant #800/#803 fixed for the
/// approve-then-resolve path.
#[tokio::test]
async fn test_cancel_during_approval_wait_emits_tool_end() {
    use tokio_util::sync::CancellationToken;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // `math` is NOT auto-approved, so the approval gate fires.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");
    let cancel_token = CancellationToken::new();

    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: Some(cancel_token.clone()),
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_call = ToolCall::new("tc1", "math", r#"{"operation":"add","a":1,"b":2}"#);
    let invocation_id = uuid::Uuid::new_v4();

    // Spawn a task that watches for `ApprovalRequired` and then cancels the
    // run instead of resolving the decision channel — simulating an operator
    // hitting `run cancel` while the approval prompt is open. We hold onto
    // `decision_tx` so it isn't dropped (which would unblock the await with
    // `false` and resolve as "denied" rather than "cancelled").
    let cancel_token_clone = cancel_token.clone();
    let approval_handler = tokio::spawn(async move {
        let mut held_tx = None;
        while let Some(event) = rx.recv().await {
            match event {
                RuntimeEvent::ApprovalRequired { decision_tx, .. } => {
                    held_tx = Some(decision_tx);
                    cancel_token_clone.cancel();
                }
                RuntimeEvent::ToolEnd { .. } => {
                    // Surface the terminal event back to the test body.
                    return (held_tx, Some(event));
                }
                _ => {}
            }
        }
        (held_tx, None)
    });

    let result = runtime
        .execute_tool_call(
            &tool_call,
            invocation_id,
            &session_manager,
            session.id,
            None,
        )
        .await;

    // Drop the runtime so the event channel closes and the handler's `recv`
    // loop terminates cleanly if `ToolEnd` was never observed.
    drop(runtime);

    // The loop must surface `Cancelled` — not a denial or success.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    let (_held_tx, tool_end) = approval_handler.await.unwrap();

    // The terminal event must have been emitted before unwind.
    let tool_end = tool_end.expect(
        "tool_end must be emitted on cancel-during-approval-wait — \
         every tool_start must have a matching terminal event (#816)",
    );
    match tool_end {
        RuntimeEvent::ToolEnd {
            invocation_id: end_id,
            ok,
            result,
            ..
        } => {
            assert_eq!(
                end_id, invocation_id,
                "tool_end must reference the same invocation_id as the tool_start"
            );
            assert!(!ok, "tool_end after cancel must report ok=false");
            // Result payload should mention cancellation so the UI / persisted
            // state can distinguish this from a denial or generic error.
            let err_str = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            assert!(
                err_str.contains("cancel"),
                "tool_end result.error should mention cancellation, got {:?}",
                result
            );
        }
        _ => panic!("expected RuntimeEvent::ToolEnd, got a different RuntimeEvent variant"),
    }
}

/// Regression test for #893: cancellation during approval-wait must append
/// an audit entry. Sibling gap to #815 in the same approval gate. Without
/// the fix, cancelling a run while the approval prompt is open emits a
/// `tool_end` event but leaves the audit log silent — operators can't
/// distinguish "approval pending then run cancelled" from "approval never
/// happened".
#[tokio::test]
async fn test_cancel_during_approval_wait_appends_audit_entry() {
    use alms_core::AuditDecision;
    use tokio_util::sync::CancellationToken;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // `math` is NOT auto-approved, so the approval gate fires.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");
    let cancel_token = CancellationToken::new();

    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: Some(cancel_token.clone()),
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_call = ToolCall::new("tc-cancel", "math", r#"{"operation":"add","a":1,"b":2}"#);
    let invocation_id = uuid::Uuid::new_v4();

    // Wait for `ApprovalRequired`, then cancel without resolving the
    // decision channel. Hold onto `decision_tx` so dropping it doesn't
    // unblock the await as "denied" before the cancel arm fires.
    let cancel_token_clone = cancel_token.clone();
    let approval_handler = tokio::spawn(async move {
        let mut held_tx = None;
        while let Some(event) = rx.recv().await {
            if let RuntimeEvent::ApprovalRequired { decision_tx, .. } = event {
                held_tx = Some(decision_tx);
                cancel_token_clone.cancel();
                break;
            }
        }
        held_tx
    });

    let result = runtime
        .execute_tool_call(
            &tool_call,
            invocation_id,
            &session_manager,
            session.id,
            None,
        )
        .await;

    drop(runtime);

    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    let _held_tx = approval_handler.await.unwrap();

    // The audit log must contain exactly one entry: a `Deny` decision for
    // the `math` tool with a cancellation-shaped error string. Before the
    // #893 fix this assertion failed with an empty audit log.
    let audit = session_manager.get_audit(session.id).unwrap();
    let deny_entries: Vec<_> = audit
        .iter()
        .filter(|e| matches!(e.decision, AuditDecision::Deny))
        .collect();
    assert_eq!(
        deny_entries.len(),
        1,
        "expected exactly one Deny audit entry for the cancelled approval, \
         got {} (full audit: {:#?})",
        deny_entries.len(),
        audit
    );
    let entry = deny_entries[0];
    assert_eq!(entry.tool, "math", "deny entry must record tool name");
    // VERBATIM equality on the error string (Tim's review on PR #925):
    // the audit-log discriminator IS the error string text — log-query
    // tooling that greps for "approval cancelled by run cancellation"
    // would silently break under a future wording refactor if the test
    // only asserted on a substring. Pin the wire shape exactly.
    assert_eq!(
        entry.error.as_deref(),
        Some("Tool 'math' approval cancelled by run cancellation"),
        "deny entry error string must match the exact #893 discriminator"
    );
    assert!(
        entry.result.is_none(),
        "deny entry must not carry a result payload, got {:?}",
        entry.result
    );
    assert_eq!(
        entry.params,
        serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        "deny entry must record the parsed tool arguments"
    );
    // No `Allow` entry should sneak in for a cancelled call.
    assert!(
        !audit
            .iter()
            .any(|e| matches!(e.decision, AuditDecision::Allow)),
        "cancelled call must not produce an Allow audit entry"
    );
}

/// Regression test for #894: when the approval `decision_rx` is closed
/// without a value (no cancel token branch active because `inflight`
/// is `None`), the function returns `Err(ToolExecution("Approval channel
/// closed"))` and must append an audit entry. Sibling gap to #815 / #893.
/// Mirrors the shape of `test_denied_approval_appends_deny_audit_entry`.
#[tokio::test]
async fn test_approval_channel_closed_appends_audit_entry() {
    use alms_core::AuditDecision;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");

    // Note: `cancel_token: None` is critical here — the channel-closed
    // branch only fires in the `else` arm of the cancel-token check.
    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_call = ToolCall::new("tc-closed", "math", r#"{"operation":"add","a":1,"b":2}"#);

    // Drop the approval `decision_tx` without sending a value. This closes
    // the channel and triggers the `Err(_)` branch of `decision_rx.await`.
    let close_handler = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let RuntimeEvent::ApprovalRequired { decision_tx, .. } = event {
                drop(decision_tx);
                break;
            }
        }
    });

    let result = runtime
        .execute_tool_call(
            &tool_call,
            uuid::Uuid::new_v4(),
            &session_manager,
            session.id,
            None,
        )
        .await;

    close_handler.await.unwrap();

    match &result {
        Err(AlmsError::ToolExecution(msg)) => {
            // VERBATIM equality on the wire-shape error string — see
            // the analogous comment in
            // `test_cancel_during_approval_wait_appends_audit_entry`
            // and Tim's review on PR #925.
            assert_eq!(
                msg, "Tool 'math' approval channel closed",
                "expected exact channel-closed error message, got {:?}",
                msg
            );
        }
        other => panic!(
            "expected ToolExecution(\"Tool 'math' approval channel closed\"), got {:?}",
            other
        ),
    }

    let audit = session_manager.get_audit(session.id).unwrap();
    let deny_entries: Vec<_> = audit
        .iter()
        .filter(|e| matches!(e.decision, AuditDecision::Deny))
        .collect();
    assert_eq!(
        deny_entries.len(),
        1,
        "expected exactly one Deny audit entry for the channel-closed unwind, \
         got {} (full audit: {:#?})",
        deny_entries.len(),
        audit
    );
    let entry = deny_entries[0];
    assert_eq!(entry.tool, "math", "deny entry must record tool name");
    // VERBATIM equality on the audit error string. The discriminator-by-
    // error-string approach (#815 / #893 / #894) means log-query tooling
    // depends on this exact text — pin it. See Tim's review on PR #925.
    assert_eq!(
        entry.error.as_deref(),
        Some("Tool 'math' approval channel closed"),
        "deny entry error string must match the exact #894 discriminator"
    );
    assert!(
        entry.result.is_none(),
        "deny entry must not carry a result payload, got {:?}",
        entry.result
    );
    assert_eq!(
        entry.params,
        serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        "deny entry must record the parsed tool arguments"
    );
    assert!(
        !audit
            .iter()
            .any(|e| matches!(e.decision, AuditDecision::Allow)),
        "channel-closed call must not produce an Allow audit entry"
    );
}

/// Regression for A2-1 (#1125): when the approval `decision_tx` is dropped
/// without a value on the no-cancel-token (`inflight == None`) path, the
/// channel-closed unwind MUST emit a matching `ToolEnd` for the `tool_start`
/// it already fired, so the 1:1 `tool_start`/`tool_end` invariant (#816/#846)
/// the cancel and deny sibling branches uphold is preserved here too.
/// Pre-fix this branch returned `Err(ToolExecution)` with no `ToolEnd`,
/// sticking the frontend spinner. Falsifiable: deleting the `ToolEnd` emit in
/// the channel-closed arm makes the `saw_tool_end` assertion fail.
#[tokio::test]
async fn test_approval_channel_closed_emits_matching_tool_end() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["math".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");

    // `cancel_token: None` is critical — the channel-closed branch only fires
    // in the `else` arm of the cancel-token check. With no cancel token the
    // sequential Guarded path passes `inflight: None` here.
    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            posture: Posture::Guarded,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tool_call = ToolCall::new("tc-closed", "math", r#"{"operation":"add","a":1,"b":2}"#);
    let invocation_id = uuid::Uuid::new_v4();

    // Collector task: drain every event until the channel closes (which
    // happens once `runtime` — and the `tx` clone it holds — is dropped after
    // the call returns). On `ApprovalRequired`, drop `decision_tx` to close the
    // approval channel and trigger the channel-closed unwind. Crucially we do
    // NOT drop `rx` early, so the `ToolEnd` the unwind emits is observed.
    // `RuntimeEvent` is not `Debug` (it carries a oneshot sender), so we
    // reduce each event to a small inspectable tuple inside the collector.
    let collector = tokio::spawn(async move {
        let mut tool_starts = 0usize;
        let mut tool_ends = 0usize;
        let mut matching_tool_end = false;
        while let Some(event) = rx.recv().await {
            match event {
                RuntimeEvent::ApprovalRequired { decision_tx, .. } => {
                    // Drop without sending → closes the approval channel and
                    // triggers the channel-closed unwind.
                    drop(decision_tx);
                }
                RuntimeEvent::ToolStart {
                    invocation_id: id,
                    tool,
                    ..
                } if id == invocation_id && tool == "math" => {
                    tool_starts += 1;
                }
                RuntimeEvent::ToolEnd {
                    invocation_id: id,
                    ok,
                    result,
                    source_agent,
                    task_id,
                } if id == invocation_id => {
                    tool_ends += 1;
                    if !ok
                        && source_agent.is_none()
                        && task_id.is_none()
                        && result == serde_json::json!({"error": "approval channel closed"})
                    {
                        matching_tool_end = true;
                    }
                }
                _ => {}
            }
        }
        (tool_starts, tool_ends, matching_tool_end)
    });

    let result = runtime
        .execute_tool_call(
            &tool_call,
            invocation_id,
            &session_manager,
            session.id,
            None,
        )
        .await;

    // Drop the runtime (and its `tx` clone) so the collector's channel closes.
    drop(runtime);
    let (tool_starts, tool_ends, matching_tool_end) = collector.await.unwrap();

    // The call still unwinds with the channel-closed error.
    assert!(
        matches!(&result, Err(AlmsError::ToolExecution(msg)) if msg == "Tool 'math' approval channel closed"),
        "expected channel-closed ToolExecution error, got {:?}",
        result
    );

    // A matching ToolStart must have been emitted for this invocation.
    assert_eq!(
        tool_starts, 1,
        "expected exactly one ToolStart for the math invocation, got {}",
        tool_starts
    );

    // THE REGRESSION ASSERTION: a matching terminal ToolEnd must follow, with
    // the exact shape the cancel/deny sibling branches use. Pre-fix the
    // channel-closed arm emitted no ToolEnd and this fails.
    assert!(
        matching_tool_end,
        "channel-closed unwind must emit a matching ToolEnd \
         {{ ok: false, result: {{\"error\": \"approval channel closed\"}} }} for \
         the prior ToolStart (A2-1 / #1125 1:1 invariant)"
    );

    // 1:1 invariant: exactly one ToolStart and exactly one ToolEnd for the
    // invocation, with no stray duplicate ToolEnd.
    assert_eq!(
        (tool_starts, tool_ends),
        (1, 1),
        "expected exactly one ToolStart and one ToolEnd for the invocation, got ({}, {})",
        tool_starts,
        tool_ends
    );
}

#[tokio::test]
async fn test_auto_approved_tool_bypasses_approval_in_guarded_posture() {
    // Prove that an auto-approved tool (echo) executes successfully under
    // Guarded posture WITHOUT ever emitting an ApprovalRequired event.
    // This is the positive-case complement to
    // `test_guarded_posture_sequential_approvals` (which verifies that
    // non-auto-approved tools DO require approval).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // Include only `echo` — an auto-approved builtin.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["echo".to_string()]);
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
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tc = ToolCall::new("tc1", "echo", r#"{"message":"hello"}"#);

    // Execute the auto-approved tool — should succeed without blocking.
    let result = runtime
        .execute_tool_call(
            &tc,
            uuid::Uuid::new_v4(),
            &session_manager,
            session.id,
            None,
        )
        .await;
    assert!(
        result.is_ok(),
        "Auto-approved tool should execute without approval"
    );

    // Drop the runtime's sender so the channel closes.
    drop(runtime);

    // Drain all events and verify no ApprovalRequired was ever emitted.
    let mut saw_approval = false;
    let mut saw_tool_start = false;
    let mut saw_tool_end = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            RuntimeEvent::ApprovalRequired { .. } => saw_approval = true,
            RuntimeEvent::ToolStart { .. } => saw_tool_start = true,
            RuntimeEvent::ToolEnd { .. } => saw_tool_end = true,
            _ => {}
        }
    }
    assert!(
        !saw_approval,
        "Auto-approved tool must NOT emit ApprovalRequired"
    );
    assert!(saw_tool_start, "Should have emitted ToolStart");
    assert!(saw_tool_end, "Should have emitted ToolEnd");
}

#[tokio::test]
async fn test_classifier_blocked_shell_surfaces_target_in_tool_end() {
    // Integration test for issue #758: verify the full chain
    //   SandboxError::ShellBlocked  →  AlmsError::ToolBlocked
    //                               →  RuntimeEvent::ToolEnd { result: {"target": ...} }
    // actually carries the structured `target` field the UI consumes.
    // All the unit tests in `classification.rs` cover the lower layers;
    // this locks in the runtime-level plumbing.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    // `with_builtins_sandboxed` registers `ShellTool` with the default
    // classification mode (`BlockDestructive`), which is exactly the policy
    // we want to exercise — `rm -rf /etc` is destructive.
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["shell".to_string()]);
    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let agent_id = AgentId::new();
    let session = session_manager.get_or_create(agent_id, "test");

    let runtime = AgentRuntime {
        agent_id,
        config: AgentConfig {
            // Autonomous — skip the approval gate so we go straight to
            // tool execution and the classifier fires.
            posture: Posture::Autonomous,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::BlockDestructive,
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    };

    let tc = ToolCall::new("tc-blocked", "shell", r#"{"command":"rm -rf /etc"}"#);
    let result = runtime
        .execute_tool_call(
            &tc,
            uuid::Uuid::new_v4(),
            &session_manager,
            session.id,
            None,
        )
        .await;
    assert!(
        result.is_err(),
        "classifier must block `rm -rf /etc` in BlockDestructive mode"
    );
    match result.unwrap_err() {
        AlmsError::ToolBlocked { target, .. } => {
            assert_eq!(
                target.as_deref(),
                Some("/etc"),
                "ToolBlocked must carry structured target"
            );
        }
        other => panic!("expected ToolBlocked, got {other:?}"),
    }

    drop(runtime);

    // Drain events and locate the ToolEnd for our invocation. Assert that its
    // result JSON carries `target == "/etc"` — this is the field the UI
    // reads to render the "Target" row on a blocked shell call.
    let mut saw_tool_end_with_target = false;
    while let Ok(event) = rx.try_recv() {
        if let RuntimeEvent::ToolEnd { ok, result, .. } = event {
            assert!(!ok, "blocked call must report ok=false");
            assert_eq!(
                result.get("target").and_then(|v| v.as_str()),
                Some("/etc"),
                "ToolEnd.result must carry structured target: {result:?}"
            );
            saw_tool_end_with_target = true;
        }
    }
    assert!(
        saw_tool_end_with_target,
        "runtime must emit ToolEnd with structured target on classifier block"
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

// ── tool_result_ok tests ─────────────────────────────────────────────────

#[test]
fn test_tool_result_ok_no_exit_code() {
    // Most tools return plain JSON without exit_code — treated as success.
    let val = serde_json::json!({"output": "hello"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_zero() {
    let val = serde_json::json!({"exit_code": 0, "stdout": "ok"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_nonzero() {
    let val = serde_json::json!({"exit_code": 1, "stderr": "fail"});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_negative() {
    // Killed by signal (e.g. -9) should also be failure.
    let val = serde_json::json!({"exit_code": -9});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_exit_code_non_integer() {
    // exit_code that is not an integer (e.g. string) is treated as success
    // because `as_i64()` returns None for non-integer JSON values.
    let val = serde_json::json!({"exit_code": "not_a_number"});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_string_value() {
    // Plain string result (e.g. echo tool) is always success.
    let val = serde_json::json!("just a string");
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_null_value() {
    let val = serde_json::Value::Null;
    assert!(helpers::tool_result_ok(&val));
}

// #1048 — additional in-band error conventions the runtime now treats
// as failure so the operator's at-a-glance row colour matches the
// tool's semantic outcome.

#[test]
fn test_tool_result_ok_in_band_error_field() {
    // `send_message`, `read_session`, `read_subagent_session`, and
    // `read_messages` all emit `{"error": "<message>", ...}` on the
    // `Ok(...)` path for in-band failures (agent-not-found, no DM
    // session, depth exceeded, access denied, etc.). Pre-#1048 the
    // runtime classified these as success because there was no
    // `exit_code` — fix should now flag them.
    let val = serde_json::json!({"error": "Agent 'X' not found. Use list_agents to see available agents."});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_in_band_error_empty_string_ignored() {
    // Defensive: empty-string error fields shouldn't trigger a
    // failure classification — they don't carry an actual error.
    let val = serde_json::json!({"error": "", "ok": true});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_in_band_error_non_string_ignored() {
    // A non-string `error` field (e.g. an HTTP `error` integer code,
    // or an `error` boolean) isn't the in-band-error convention this
    // helper targets — fall through to the other checks.
    let val = serde_json::json!({"error": 0});
    assert!(helpers::tool_result_ok(&val));
    let val2 = serde_json::json!({"error": null});
    assert!(helpers::tool_result_ok(&val2));
}

#[test]
fn test_tool_result_ok_http_status_5xx() {
    // `http_get` returns `Ok({"status": 503, "body": ...})` for an
    // upstream 5xx response — the transport succeeded but the job
    // failed. The UI should render this red.
    let val = serde_json::json!({
        "status": 503,
        "content_type": "text/plain",
        "headers": {},
        "body": "Service Unavailable",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_4xx() {
    let val = serde_json::json!({
        "status": 404,
        "content_type": "text/plain",
        "headers": {},
        "body": "Not Found",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_2xx_success() {
    let val = serde_json::json!({
        "status": 200,
        "content_type": "application/json",
        "headers": {},
        "body": {"ok": true},
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_http_status_3xx_success() {
    // 3xx redirects classify as success — `http_get` doesn't follow
    // them by default and a 301/302 is informational, not a failure
    // of the operator's job.
    let val = serde_json::json!({"status": 301, "body": ""});
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_failed_status() {
    // Background `shell` tasks that fail to spawn emit
    // `{status: "failed", error: "...", task_id: ...}` (no
    // `exit_code`). The in-band `error` check catches this; the
    // status-string check is the defensive fallback.
    let val = serde_json::json!({
        "task_id": "bg_3",
        "status": "failed",
        "command": "foo",
        "error": "spawn error",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_failed_without_error_field() {
    // Defensive fallback: a future caller that emits `status: "failed"`
    // without an `error` field should still classify as failure.
    let val = serde_json::json!({"task_id": "bg_4", "status": "failed"});
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_submitted_status() {
    // Background submission is a success — the operator initiated the
    // task successfully even though it hasn't completed yet.
    let val = serde_json::json!({
        "task_id": "bg_1",
        "status": "submitted",
        "message": "Task started",
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_completed_with_exit_code() {
    // Background task that completed cleanly — `exit_code: 0` plus
    // `status: "completed"`. Treated as success.
    let val = serde_json::json!({
        "task_id": "bg_2",
        "status": "completed",
        "command": "echo hi",
        "exit_code": 0,
        "stdout": "hi\n",
        "stderr": "",
    });
    assert!(helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_background_shell_completed_with_nonzero_exit() {
    // Background task that completed with non-zero exit — the
    // `exit_code` discriminator wins over the success-flavoured
    // `status: "completed"` because the underlying command failed.
    let val = serde_json::json!({
        "task_id": "bg_2",
        "status": "completed",
        "command": "false",
        "exit_code": 1,
        "stdout": "",
        "stderr": "",
    });
    assert!(!helpers::tool_result_ok(&val));
}

#[test]
fn test_tool_result_ok_status_string_unrecognised() {
    // String statuses other than "failed" / "error" don't flip the
    // classifier — `submitted`, `unknown`, `not_found_or_still_running`,
    // and `completed` all carry no failure signal on their own.
    for status in [
        "submitted",
        "unknown",
        "not_found_or_still_running",
        "completed",
        "in_progress",
    ] {
        let val = serde_json::json!({"status": status});
        assert!(
            helpers::tool_result_ok(&val),
            "status={status:?} should not flip the classifier"
        );
    }
}

#[test]
fn test_tool_result_ok_status_string_error_case_insensitive() {
    // Defensive: case shouldn't matter for the status-string check.
    for s in ["error", "ERROR", "Error", "failed", "FAILED", "Failed"] {
        let val = serde_json::json!({"status": s});
        assert!(
            !helpers::tool_result_ok(&val),
            "status={s:?} should classify as failure"
        );
    }
}

#[test]
fn test_tool_result_ok_user_denied() {
    // #1109: the approval-gate deny body `{"user_denied": true, ...}` is
    // a failed tool call for `ok`-flag purposes even though it carries no
    // `error` key (deliberately — the distinct key is what lets the agent
    // tell a user policy decision apart from a runtime error).
    let val = crate::agent::loop_impl::user_denied_result();
    assert!(!helpers::tool_result_ok(&val));

    // Only an explicit boolean `true` flips the classifier.
    for benign in [
        serde_json::json!({"user_denied": false}),
        serde_json::json!({"user_denied": "true"}),
        serde_json::json!({"user_denied": 1}),
    ] {
        assert!(
            helpers::tool_result_ok(&benign),
            "user_denied={benign:?} should not classify as failure"
        );
    }
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

// ---- send_message / ignore_message conflict detection tests (#364) ----
//
// These tests exercise `detect_dm_conflict` for request-level conflict
// detection, and `alms_core::ran_ignore_message_successfully` for
// result-level termination decisions (verifying the real code path).

/// When `ignore_message` appears alongside a `send_message` aimed at the
/// **current peer**, both should be blocked with error results (folding a
/// reply AND ending the conversation with them is contradictory). Neither
/// tool should execute, and the run should NOT terminate early (the agent
/// gets another iteration to choose one). (#1154 / S3)
#[test]
fn test_dm_conflict_blocks_both_tools() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![
        ToolCall::new(
            "tc_send",
            "send_message",
            r#"{"to":"alice","message":"hi"}"#,
        ),
        ToolCall::new(
            "tc_ignore",
            "ignore_message",
            r#"{"reason":"nothing to add"}"#,
        ),
    ];

    // Peer is "alice" — the send targets the current peer, so this conflicts.
    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));

    assert!(
        check.conflict,
        "Conflict should be detected when ignore_message is paired with a \
         send_message aimed at the current peer"
    );

    // Both DM tools (indices 0 and 1) should appear in the conflicting set.
    assert_eq!(
        check.conflicting_indices,
        vec![0, 1],
        "both the peer-directed send (0) and the ignore (1) should be blocked"
    );
}

/// When `ignore_message` is called alone (no `send_message`), there
/// should be no conflict.
#[test]
fn test_ignore_message_alone_no_conflict() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![ToolCall::new(
        "tc_ignore",
        "ignore_message",
        r#"{"reason":"not relevant"}"#,
    )];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));

    assert!(
        !check.conflict,
        "No conflict when only ignore_message is present"
    );
    assert!(
        check.conflicting_indices.is_empty(),
        "No tools should be flagged as conflicting"
    );
}

/// When both conflicting tools appear alongside a normal tool (e.g.
/// `echo`), only the conflicting tools should be blocked. The normal
/// tool should still be eligible for execution.
#[test]
fn test_dm_conflict_preserves_non_conflicting_tools() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![
        ToolCall::new("tc_echo", "echo", r#"{"message":"hello"}"#),
        ToolCall::new(
            "tc_send",
            "send_message",
            r#"{"to":"alice","message":"hi"}"#,
        ),
        ToolCall::new(
            "tc_ignore",
            "ignore_message",
            r#"{"reason":"nothing to add"}"#,
        ),
    ];

    // Peer is "alice" — the send targets the peer, so this conflicts.
    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(check.conflict);

    // The two DM tools (indices 1 and 2) should be blocked; echo (index 0)
    // is left to execute.
    assert_eq!(
        check.conflicting_indices,
        vec![1, 2],
        "the peer-directed send (1) and the ignore (2) should be blocked, echo (0) preserved"
    );

    // Partition: echo should be in the "execute" set, the other two
    // should be in the "conflict error" set.
    let exec_indices: Vec<usize> = (0..tool_calls.len())
        .filter(|i| !check.conflicting_indices.contains(i))
        .collect();

    assert_eq!(exec_indices, vec![0], "Only echo (index 0) should execute");
    assert_eq!(tool_calls[exec_indices[0]].function.name, "echo");

    // The two DM tools should be blocked.
    let blocked: Vec<&str> = check
        .conflicting_indices
        .iter()
        .map(|&i| tool_calls[i].function.name.as_str())
        .collect();
    assert_eq!(blocked.len(), 2);
    assert!(blocked.contains(&"send_message"));
    assert!(blocked.contains(&"ignore_message"));
}

/// When `send_message` is called alone (no `ignore_message`), there
/// should be no conflict.
#[test]
fn test_send_message_alone_no_conflict() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![ToolCall::new(
        "tc_send",
        "send_message",
        r#"{"to":"alice","message":"hi"}"#,
    )];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(
        !check.conflict,
        "No conflict when only send_message is present"
    );
    assert!(check.conflicting_indices.is_empty());
}

/// When neither DM tool is present, there should be no conflict.
#[test]
fn test_no_dm_tools_no_conflict() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![ToolCall::new("tc_echo", "echo", r#"{"message":"hello"}"#)];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(!check.conflict);
    assert!(check.conflicting_indices.is_empty());
}

/// S3 (#1154): `ignore_message` paired with a `send_message` aimed at a
/// **different** agent (not the current peer) is a LEGITIMATE combination
/// under implicit replies ("loop in Charlie, then end with the peer") and
/// must NOT be rejected. Pre-S3 the recipient-blind check wrongly flagged
/// this batch.
#[test]
fn test_dm_conflict_allows_ignore_with_send_to_third_agent() {
    use crate::llm_types::ToolCall;

    // Current peer is "alice"; the send targets "charlie" (a third agent).
    let tool_calls = vec![
        ToolCall::new(
            "tc_send",
            "send_message",
            r#"{"to":"charlie","message":"can you help?"}"#,
        ),
        ToolCall::new(
            "tc_ignore",
            "ignore_message",
            r#"{"reason":"handing off to charlie"}"#,
        ),
    ];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(
        !check.conflict,
        "ignore_message + send_message to a THIRD agent must not conflict (S3)"
    );
    assert!(
        check.conflicting_indices.is_empty(),
        "no tools should be flagged when the send targets a non-peer agent"
    );
}

/// #1160 (Codex P2): a MIXED batch holding `ignore_message`, a folded
/// `send_message` to the **current peer**, AND a legitimate `send_message`
/// hand-off to a **third agent** must block only the peer-directed send and
/// the ignore — the third-agent send (which shares the name `"send_message"`)
/// must survive. The pre-fix code returned the tool-name set
/// `["send_message", "ignore_message"]`, so both sends matched by name and
/// the third-agent hand-off was wrongly replaced with the DM conflict error.
/// Keying the conflict on batch *index* fixes that.
#[test]
fn test_dm_conflict_mixed_batch_preserves_third_agent_send() {
    use crate::llm_types::ToolCall;

    // Current peer is "alice". Batch order: ignore, peer-send (folded),
    // third-agent send (charlie). Indices 0 and 1 conflict; 2 executes.
    let tool_calls = vec![
        ToolCall::new(
            "tc_ignore",
            "ignore_message",
            r#"{"reason":"wrapping up with alice"}"#,
        ),
        ToolCall::new(
            "tc_send_peer",
            "send_message",
            r#"{"to":"alice","message":"bye"}"#,
        ),
        ToolCall::new(
            "tc_send_charlie",
            "send_message",
            r#"{"to":"charlie","message":"taking this over?"}"#,
        ),
    ];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(
        check.conflict,
        "a peer-directed send + ignore in the batch is still a conflict"
    );

    // Only the ignore (0) and the peer-directed send (1) are blocked; the
    // charlie hand-off (2) is NOT in the conflicting set.
    assert_eq!(
        check.conflicting_indices,
        vec![0, 1],
        "the ignore (0) and the peer-directed send (1) are blocked; \
         the third-agent send (2) is preserved"
    );

    // The surviving (execute) set is exactly the charlie hand-off.
    let exec_indices: Vec<usize> = (0..tool_calls.len())
        .filter(|i| !check.conflicting_indices.contains(i))
        .collect();
    assert_eq!(
        exec_indices,
        vec![2],
        "only the third-agent send (index 2) should execute"
    );
    assert_eq!(tool_calls[2].function.name, "send_message");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&tool_calls[2].function.arguments)
            .unwrap()
            .get("to")
            .and_then(|v| v.as_str()),
        Some("charlie"),
        "the surviving send must be the charlie hand-off, not the peer fold"
    );
}

/// S3 (#1154): recipient matching is case-insensitive, mirroring the
/// peer-fold check — `send_message(to: "Alice")` to peer "alice" still
/// conflicts with `ignore_message`.
#[test]
fn test_dm_conflict_peer_match_is_case_insensitive() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![
        ToolCall::new(
            "tc_send",
            "send_message",
            r#"{"to":"Alice","message":"hi"}"#,
        ),
        ToolCall::new("tc_ignore", "ignore_message", r#"{"reason":"done"}"#),
    ];

    let check = dm::detect_dm_conflict(&tool_calls, Some("alice"));
    assert!(
        check.conflict,
        "case-insensitive recipient match must still detect the peer-directed conflict"
    );
}

/// S3 (#1154): outside a DM run (`dm_peer = None`) no `send_message` can
/// target "the peer", so `send_message` + `ignore_message` never conflicts
/// — `ignore_message` simply fails on its own in a non-DM context.
#[test]
fn test_dm_conflict_no_peer_means_no_conflict() {
    use crate::llm_types::ToolCall;

    let tool_calls = vec![
        ToolCall::new(
            "tc_send",
            "send_message",
            r#"{"to":"alice","message":"hi"}"#,
        ),
        ToolCall::new("tc_ignore", "ignore_message", r#"{"reason":"x"}"#),
    ];

    let check = dm::detect_dm_conflict(&tool_calls, None);
    assert!(
        !check.conflict,
        "with no DM peer there is no peer-directed send, so no conflict"
    );
}

/// Build a minimal `AgentRuntime` with the `echo` builtin registered and no
/// cancel token, for the conflict-execution test below. `echo` is
/// auto-approved and executes instantly, so it stands in for "the tool that
/// must survive" without dragging the MessageBus / DM wiring into a unit
/// test — the execution-path filter is keyed purely on batch index now, so
/// the tool's *name* is irrelevant to what gets blocked.
fn make_runtime_for_conflict_exec_test(posture: Posture) -> AgentRuntime {
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools =
        crate::tools::ToolRegistry::with_builtins_sandboxed(None, true, &["echo".to_string()]);
    AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig {
            posture,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    }
}

/// #5: the tool-call record persisted for a result carries the SAME
/// correlator the `ToolEnd` event carries.
///
/// This is the property the whole issue turns on. `run_tool_calls` is the
/// sole persistence site for `Tool`-role rows, and it both emits `ToolEnd`
/// and pushes the record; before #5 the record stored only the *provider's*
/// `tool_id`, which no event, no `session_messages` metadata entry and no
/// frontend lookup ever sees. A row reconstructed from `run_tool_calls` was
/// therefore uncorrelatable with the live stream by construction.
///
/// Asserting the two against each other (rather than each against a literal)
/// is what makes this a pin on *agreement* rather than on two independent
/// spellings that could drift apart.
///
/// Scope: this covers the `Tool`-role push. The `Assistant`-role push lives
/// in `agent_loop`, which needs a full LLM round trip to reach; it consumes
/// the same `invocation_ids` slice, positionally, via `zip` over the very vec
/// it was built from.
#[tokio::test]
async fn tool_call_records_carry_the_streamed_invocation_id() {
    let tool_calls = vec![
        ToolCall::new("call_provider_a", "echo", r#"{"message":"a"}"#),
        ToolCall::new("call_provider_b", "echo", r#"{"message":"b"}"#),
    ];
    let invocation_ids: Vec<uuid::Uuid> = (0..2).map(|_| uuid::Uuid::new_v4()).collect();

    // Both execution arms: Guarded runs sequentially, Autonomous in parallel,
    // and they reach `persist_one_tool_result` by different routes.
    for posture in [Posture::Guarded, Posture::Autonomous] {
        let mut runtime = make_runtime_for_conflict_exec_test(posture);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runtime.event_sender = Some(tx);

        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut tool_seq: u32 = 0;
        let results = runtime
            .run_tool_calls(
                &tool_calls,
                &invocation_ids,
                &[],
                &session_manager,
                session.id,
                false,
                &mut tool_call_records,
                &mut tool_seq,
            )
            .await
            .expect("echo batch must succeed");

        // `run_tool_calls` emits the `ToolEnd` events and persists records only
        // on its cancel path; the completed-batch records are collected by
        // `process_tool_results`, which `agent_loop` calls next. Drive both, in
        // that order, so the assertion below spans the real pairing.
        let mut messages: Vec<LlmMessage> = Vec::new();
        runtime.process_tool_results(
            &tool_calls,
            results,
            &invocation_ids,
            &mut messages,
            &mut tool_call_records,
            &mut tool_seq,
            &session_manager,
            session.id,
            false,
        );

        drop(runtime);

        let mut streamed: Vec<String> = Vec::new();
        while let Some(event) = rx.recv().await {
            if let RuntimeEvent::ToolEnd { invocation_id, .. } = event {
                streamed.push(invocation_id.to_string());
            }
        }

        let persisted: Vec<Option<String>> = tool_call_records
            .iter()
            .map(|r| r.tool_invocation_id.clone())
            .collect();

        assert_eq!(
            persisted.len(),
            2,
            "{posture:?}: one Tool-role record per call"
        );
        assert_eq!(streamed.len(), 2, "{posture:?}: one ToolEnd per call");

        // The agreement assertion. Sorted because the parallel arm may emit
        // and persist in completion order rather than call order — the claim
        // is that the two SETS are the same, which is what correlation needs.
        let mut persisted_sorted: Vec<String> = persisted
            .into_iter()
            .map(|v| v.expect("every persisted record must carry a correlator"))
            .collect();
        let mut streamed_sorted = streamed.clone();
        persisted_sorted.sort();
        streamed_sorted.sort();
        assert_eq!(
            persisted_sorted, streamed_sorted,
            "{posture:?}: the persisted correlator must be the streamed one",
        );

        // And both must be the ids the caller supplied — not, say, freshly
        // minted ones that merely happen to agree with each other.
        let mut supplied: Vec<String> = invocation_ids.iter().map(|i| i.to_string()).collect();
        supplied.sort();
        assert_eq!(
            persisted_sorted, supplied,
            "{posture:?}: correlators must be the caller's invocation ids",
        );

        // The provider id is still recorded, and is still a different thing.
        let provider_ids: std::collections::HashSet<&str> = tool_call_records
            .iter()
            .filter_map(|r| r.tool_id.as_deref())
            .collect();
        assert_eq!(
            provider_ids,
            ["call_provider_a", "call_provider_b"].into_iter().collect(),
            "{posture:?}: tool_id must keep carrying the provider's id",
        );
    }
}

/// #1160 (Codex P2), execution path: drive `run_tool_calls` with the exact
/// mixed-batch shape — `ignore_message` (0) + peer-directed `send_message`
/// (1) + third-agent `send_message` (2) — and a `conflicting_indices` of
/// `[0, 1]` (what `detect_dm_conflict` now produces). The two blocked slots
/// must receive `DM_CONFLICT_MSG`; the third slot must EXECUTE and return its
/// tool output. The `send_message`/`ignore_message` calls are stand-in'd by
/// `echo` so the test stays a pure runtime unit test — index-based filtering
/// makes the tool name irrelevant, which is the whole point of the fix.
///
/// Covered for BOTH execution arms: `Posture::Guarded` (sequential) and
/// `Posture::Autonomous` (parallel) are separately reachable in
/// `run_tool_calls`, and the parallel arm's slot-exhaustion invariant
/// (exec_indices ⟂ conflicting_indices) is exercised here.
#[tokio::test]
async fn test_run_tool_calls_blocks_by_index_preserves_third_agent_send() {
    // Slot 2 carries a distinct echo payload so we can prove the *surviving*
    // call is the third-agent one (index 2), not the folded peer send.
    let tool_calls = vec![
        ToolCall::new("tc_ignore", "echo", r#"{"message":"ignored"}"#),
        ToolCall::new("tc_send_peer", "echo", r#"{"message":"peer-fold"}"#),
        ToolCall::new("tc_send_charlie", "echo", r#"{"message":"to-charlie"}"#),
    ];
    let invocation_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();
    // The set `detect_dm_conflict` produces for ignore(0) + peer-send(1).
    let conflicting_indices = [0usize, 1usize];

    for posture in [Posture::Guarded, Posture::Autonomous] {
        let runtime = make_runtime_for_conflict_exec_test(posture);
        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut tool_seq: u32 = 0;
        let results = runtime
            .run_tool_calls(
                &tool_calls,
                &invocation_ids,
                &conflicting_indices,
                &session_manager,
                session.id,
                false,
                &mut tool_call_records,
                &mut tool_seq,
            )
            .await
            .expect("non-cancelled batch must return Ok results");

        assert_eq!(
            results.len(),
            3,
            "{posture:?}: result vector must be in tool_calls order, one slot per call"
        );

        // Slots 0 and 1 are blocked with the DM conflict error.
        for &blocked in &conflicting_indices {
            match &results[blocked] {
                Err(AlmsError::ToolExecution(msg)) => assert_eq!(
                    msg,
                    super::dm::DM_CONFLICT_MSG,
                    "{posture:?}: slot {blocked} must carry the DM conflict message"
                ),
                other => {
                    panic!("{posture:?}: slot {blocked} expected DM conflict Err, got {other:?}")
                }
            }
        }

        // Slot 2 (the third-agent send) executed and returned its echo
        // output — NOT a conflict error.
        match &results[2] {
            Ok(v) => assert_eq!(
                v,
                &serde_json::json!("to-charlie"),
                "{posture:?}: the third-agent send must execute and return its output"
            ),
            other => panic!("{posture:?}: slot 2 (third-agent send) must execute, got {other:?}"),
        }
    }
}

/// Verify that streaming usage is accumulated (merged) across chunks rather
/// than overwritten.  Anthropic sends `input_tokens` in `message_start` and
/// `output_tokens` in `message_delta` as separate events; the accumulator
/// must combine them.
#[test]
fn test_streaming_usage_accumulation() {
    // Simulate two chunks: first has prompt_tokens, second has completion_tokens
    let first = Usage {
        prompt_tokens: 150,
        total_tokens: 150,
        ..Usage::default()
    };
    let second = Usage {
        completion_tokens: 75,
        total_tokens: 75,
        ..Usage::default()
    };

    // Replicate the accumulation logic from stream_llm_call
    let mut usage: Option<Usage> = None;

    // Process first chunk
    let chunk_usage = first;
    usage = Some(match usage {
        Some(prev) => Usage {
            prompt_tokens: prev.prompt_tokens.max(chunk_usage.prompt_tokens),
            completion_tokens: prev.completion_tokens.max(chunk_usage.completion_tokens),
            ..Usage::default()
        },
        None => chunk_usage,
    });
    if let Some(ref mut u) = usage {
        u.total_tokens = u.prompt_tokens + u.completion_tokens;
    }

    // Process second chunk
    let chunk_usage = second;
    usage = Some(match usage {
        Some(prev) => Usage {
            prompt_tokens: prev.prompt_tokens.max(chunk_usage.prompt_tokens),
            completion_tokens: prev.completion_tokens.max(chunk_usage.completion_tokens),
            ..Usage::default()
        },
        None => chunk_usage,
    });
    if let Some(ref mut u) = usage {
        u.total_tokens = u.prompt_tokens + u.completion_tokens;
    }

    let u = usage.expect("usage should be set");
    assert_eq!(
        u.prompt_tokens, 150,
        "prompt_tokens should come from first chunk"
    );
    assert_eq!(
        u.completion_tokens, 75,
        "completion_tokens should come from second chunk"
    );
    assert_eq!(u.total_tokens, 225, "total should be sum of both");
}

/// `Usage::reasoning_tokens_effective` prefers the nested OpenAI shape
/// over the flat DeepSeek/xAI shape when both happen to be present;
/// falls back to the flat field when only it is set. (#768)
#[test]
fn test_usage_reasoning_tokens_effective_priority() {
    // Nested wins when set.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        reasoning_tokens: Some(5),
        completion_tokens_details: Some(crate::llm_types::CompletionTokensDetails {
            reasoning_tokens: Some(15),
        }),
        ..Usage::default()
    };
    assert_eq!(u.reasoning_tokens_effective(), Some(15));

    // Flat wins when nested is missing.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        reasoning_tokens: Some(7),
        ..Usage::default()
    };
    assert_eq!(u.reasoning_tokens_effective(), Some(7));

    // None when neither.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        ..Usage::default()
    };
    assert!(u.reasoning_tokens_effective().is_none());
}

/// Sibling-workspace reads guard test (#242, restated for the v2
/// project-root-as-sandbox model — #945).
///
/// Scenario: a project root contains the standard v2 layout
/// `<project>/.alms/agents/<name>/personality.md`. A parent agent is
/// constructed with `with_project_root(<project>)` and
/// `with_workspace(<agents_dir>/parent/)` — mirroring exactly what the
/// gateway does on a `POST /runs`. The parent must still be able to
/// `fs_read('.alms/agents/<sibling>/personality.md')` for a sibling
/// agent's metadata.
///
/// This is the load-bearing #242 invariant: child agents must be able
/// to read parent personality and vice versa for multi-agent
/// coordination. Pre-#945 it was satisfied by `with_workspace`'s
/// `extra_read_roots = [parent_of_workspace]` shim. After #945 it is
/// satisfied "by accident" — every agent's metadata lives at
/// `<project>/.alms/agents/<name>/`, naturally inside the project-root
/// sandbox, so no extras are needed at the runtime layer. Tim's v2
/// proposal explicitly named this as a load-bearing invariant; if a
/// future refactor breaks it, child agents will silently lose access
/// to parent metadata.
#[tokio::test]
async fn test_sibling_workspace_reads_under_project_root_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Parent and sibling metadata live under `.alms/agents/`, the v2
    // canonical layout. The sandbox root for the parent agent is the
    // project root, so both metadata directories sit inside it.
    let parent_meta_dir = agents_dir.join("parent");
    let sibling_meta_dir = agents_dir.join("sibling");
    std::fs::create_dir_all(&parent_meta_dir).unwrap();
    std::fs::create_dir_all(&sibling_meta_dir).unwrap();

    // Write a personality.md file in the sibling's metadata directory.
    let sibling_personality = sibling_meta_dir.join("personality.md");
    std::fs::write(&sibling_personality, "I am a helpful sibling agent.\n").unwrap();

    // Build the parent agent: project-root sandbox + workspace
    // attachment, exactly as the gateway wires it.
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        // Empty sandbox_root keeps `AgentRuntime::new` from canonicalising
        // a stray "." against the test's cwd; `with_project_root` below
        // is the single source of truth for the sandbox root in v2.
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

    // The parent agent reads the sibling's personality.md via a
    // project-relative path — exactly the canonical v2 path shape from
    // the issue's acceptance criterion: `.alms/agents/<sibling>/personality.md`.
    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({ "path": ".alms/agents/sibling/personality.md" }),
        )
        .await;
    assert!(
        result.is_ok(),
        "parent must be able to read sibling's personality.md under v2 project-root sandbox: {:?}",
        result.err()
    );
    let value = result.unwrap();
    assert!(
        value["content"]
            .as_str()
            .unwrap_or("")
            .contains("helpful sibling agent"),
        "expected sibling's personality content, got {}",
        value
    );
}

/// Acceptance pin: project-relative `fs_read` and `fs_write` operate
/// against the project directory itself (#945).
///
/// This is the issue's first acceptance bullet:
///
/// > Default install with no `alms.toml` knobs: `fs_read('./src/main.rs')`
/// > works and `shell('cat src/main.rs')` works, when the gateway was
/// > started in the project dir.
///
/// We exercise the fs_read / fs_write half here (the shell half is
/// platform-specific and gets covered in a separate sandbox unit test
/// rather than in this runtime-level integration test). The test
/// constructs a project tree on a tempdir, builds the runtime exactly
/// as the gateway does (`with_project_root` then `with_workspace`),
/// and confirms:
///   1. `fs_read('./src/main.rs')` returns the project file's contents.
///   2. `fs_write('./src/new.rs', ...)` lands the bytes at
///      `<project>/src/new.rs` — NOT under any `.alms/` subdirectory.
#[tokio::test]
async fn test_project_relative_fs_read_and_fs_write_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(project_root.join("src")).unwrap();

    // Drop a real project file at `<project>/src/main.rs` so fs_read
    // has something to find via a project-relative path.
    let main_rs = project_root.join("src").join("main.rs");
    std::fs::write(&main_rs, "fn main() { println!(\"hi\"); }\n").unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "alice"));

    // (1) fs_read('./src/main.rs') resolves under the project root.
    let read_result = runtime
        .tools()
        .execute("fs_read", serde_json::json!({ "path": "src/main.rs" }))
        .await
        .expect("fs_read should succeed");
    assert!(
        read_result["content"]
            .as_str()
            .unwrap_or("")
            .contains("println!"),
        "expected main.rs body, got {read_result}"
    );

    // The fs_write tool is read-before-write guarded by FileStateCache
    // (#273 / read-before-write contract). The fs_read above primes the
    // cache so the subsequent fs_write to a project-relative new file
    // path is allowed.
    let new_rs = project_root.join("src").join("new.rs");
    let write_result = runtime
        .tools()
        .execute(
            "fs_write",
            serde_json::json!({
                "path": "src/new.rs",
                "content": "// generated by agent\n",
            }),
        )
        .await
        .expect("fs_write should succeed");
    assert!(
        write_result.get("ok").and_then(|v| v.as_bool()) == Some(true)
            || write_result.get("path").is_some(),
        "fs_write returned an unexpected payload shape: {write_result}"
    );

    // (2) The bytes landed at the project location, not under `.alms/`.
    let on_disk = std::fs::read_to_string(&new_rs)
        .expect("fs_write must have created `<project>/src/new.rs`");
    assert!(on_disk.contains("generated by agent"));

    // Defensive: nothing should have been created under `.alms/` for
    // this write — that path was the v1 sandbox shape.
    assert!(
        !agents_dir.join("alice").join("src").exists(),
        "fs_write must not have routed bytes under `.alms/agents/<name>/`"
    );
}

/// Acceptance pin: `fs_*` and `shell` agree on the same sandbox root
/// after `with_project_root` (#945).
///
/// This covers the issue's last acceptance bullet:
///
/// > Audit-log payloads on `fs_*` and `shell` agree on the sandbox root
/// > they're enforcing against.
///
/// We can't easily inspect the audit-log payload from a unit test, but
/// we can pin the upstream invariant: after `with_project_root`, the
/// runtime's `resolved_sandbox_root` field — which is the single
/// source of truth threaded into both the fs_* tools' sandbox check
/// and the shell tool's `with_policy` constructor — points at the
/// canonicalised project root. If a future refactor splits the two
/// roots back apart, this assertion fires.
#[tokio::test]
async fn test_fs_and_shell_share_one_sandbox_root_v2() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone());

    let canonical_project = std::fs::canonicalize(&project_root).unwrap();
    assert_eq!(
        runtime.resolved_sandbox_root.as_deref(),
        Some(canonical_project.as_path()),
        "after with_project_root, the runtime's single sandbox root must \
         match the canonicalised project root — fs_* and shell both read \
         from this field at registration time"
    );
}

/// Acceptance pin: `workspace_write` lands files at
/// `<project>/.alms/agents/<name>/personality.md` (#945).
///
/// This is the issue's third acceptance bullet:
///
/// > `<project>/.alms/agents/<name>/personality.md` is the canonical
/// > metadata location; `workspace_write` still writes there.
#[tokio::test]
async fn test_workspace_write_lands_under_alms_agents_path_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "alice"));

    // `workspace_write` is the one tool that targets agent metadata
    // files specifically. It writes via `AgentWorkspace::write_file`,
    // which puts the file at `<workspace_dir>/<name>/<filename>`.
    let result = runtime
        .tools()
        .execute(
            "workspace_write",
            serde_json::json!({
                "file": "personality",
                "content": "I am Alice and I help with code.\n",
                "mode": "write",
            }),
        )
        .await
        .expect("workspace_write should succeed");
    assert_eq!(result["ok"], serde_json::json!(true), "result: {result}");

    // The canonical v2 location is `<project>/.alms/agents/<name>/personality.md`.
    let canonical = project_root
        .join(".alms")
        .join("agents")
        .join("alice")
        .join("personality.md");
    let on_disk = std::fs::read_to_string(&canonical)
        .expect("workspace_write must land at `<project>/.alms/agents/<name>/personality.md`");
    assert!(on_disk.contains("I am Alice"));
}

/// Sandbox-escape guard: paths outside the project root are still
/// rejected for `fs_write` even under the v2 single-root model (#945).
///
/// Pre-#945 there was a tighter "writes are confined to the workspace
/// dir, reads can extend to siblings via extras" split. v2 collapses
/// to a single sandbox root — the project root — so writes to sibling
/// metadata at `.alms/agents/<sibling>/personality.md` are *allowed*
/// (they're inside the project root) and that is the intended new
/// behaviour. What still must NOT work is escaping the project root
/// entirely; this test pins that invariant for `fs_write`.
#[tokio::test]
async fn test_v2_fs_write_rejects_paths_outside_project_root() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    // Two sibling tempdirs: one is the project root, the other is
    // explicitly outside it. fs_write to the outside dir must fail.
    let project_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();
    let project_root = project_dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

    // fs_write to a path outside the project root must fail — the
    // project-root sandbox is the single source of truth in v2.
    let outside_target = outside_dir.path().join("escape.txt");
    let result = runtime
        .tools()
        .execute(
            "fs_write",
            serde_json::json!({
                "path": outside_target.to_str().unwrap(),
                "content": "should not land",
            }),
        )
        .await;
    assert!(
        result.is_err(),
        "fs_write must reject paths outside the project-root sandbox"
    );
    assert!(
        !outside_target.exists(),
        "the file must not have been created"
    );
}

/// Ephemeral subagent v2 behaviour: shares the parent's project-root
/// sandbox (#945). Pre-#945 the runtime used the ephemeral workspace
/// dir as the sandbox root and therefore *could not* read a sibling
/// named-agent's metadata; v2 unifies on the project root, so an
/// ephemeral subagent CAN read sibling metadata at
/// `<project>/.alms/agents/<name>/personality.md` — same allowance the
/// parent itself has. This test documents the v2 semantics so a future
/// refactor that tightens isolation cannot silently land without
/// updating the surrounding contracts (#946 is the proper place to
/// reintroduce per-subagent isolation as an opt-in).
#[tokio::test]
async fn test_ephemeral_subagent_inherits_project_root_sandbox_v2() {
    use crate::workspace::AgentWorkspace;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let project_root = dir.path().to_path_buf();
    let agents_dir = project_root.join(".alms").join("agents");

    // Named agent lives at `<project>/.alms/agents/researcher/`.
    let named_meta_dir = agents_dir.join("researcher");
    std::fs::create_dir_all(&named_meta_dir).unwrap();
    let named_memories = named_meta_dir.join("memories.md");
    std::fs::write(&named_memories, "researcher notes\n").unwrap();

    // Ephemeral subagent metadata lives at
    // `<project>/.alms/agents/.ephemeral/<task_id>/` — flat under the
    // agents dir. Its sandbox is the project root, same as any other
    // subagent.
    let task_id = "test-task-00000000";
    let ephemeral_meta_dir = agents_dir.join(".ephemeral").join(task_id);
    std::fs::create_dir_all(&ephemeral_meta_dir).unwrap();

    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(config).unwrap(),
    )
    .expect("runtime")
    .with_project_root(project_root.clone())
    .with_workspace(AgentWorkspace::with_dir(ephemeral_meta_dir.clone()));

    // Under v2 the ephemeral subagent CAN read the named agent's
    // memories.md — both live inside the same project-root sandbox.
    let result = runtime
        .tools()
        .execute(
            "fs_read",
            serde_json::json!({
                "path": ".alms/agents/researcher/memories.md",
            }),
        )
        .await;
    assert!(
        result.is_ok(),
        "ephemeral subagent should inherit the project-root sandbox in v2: {:?}",
        result.err()
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

/// `AgentConfig.anthropic_thinking_budget` threads through to every LLM
/// request the agent loop issues. Without this invariant, per-agent
/// overrides would land in config but never be seen by the provider.
///
/// This is a shape-assertion test — we don't run a real LLM; we build
/// the `CompletionRequest` exactly as the agent loop does and check the
/// thinking budget follows.
#[test]
fn test_agent_config_thinking_budget_threads_into_request() {
    let cfg = AgentConfig {
        anthropic_thinking_budget: 4096,
        max_tokens: 2048,
        ..AgentConfig::default()
    };

    // Build the request the same way `agent_loop` does. We can't call
    // `stream_llm_call` here (no mock LLM setup), but the request
    // builder is a one-liner so we replicate it directly.
    let mut request = CompletionRequest::new("claude-sonnet-4-20250514")
        .with_messages(vec![LlmMessage::user("hi")])
        .with_max_tokens(cfg.max_tokens);
    if cfg.anthropic_thinking_budget > 0 {
        request = request.with_thinking_budget(cfg.anthropic_thinking_budget);
    }

    assert_eq!(request.thinking_budget_tokens, Some(4096));

    // And when budget is 0, the field is None (adapter skips the wire
    // field in that case — see anthropic.rs tests).
    let cfg0 = AgentConfig {
        anthropic_thinking_budget: 0,
        ..AgentConfig::default()
    };
    let mut req0 = CompletionRequest::new("x").with_messages(vec![LlmMessage::user("hi")]);
    if cfg0.anthropic_thinking_budget > 0 {
        req0 = req0.with_thinking_budget(cfg0.anthropic_thinking_budget);
    }
    assert!(req0.thinking_budget_tokens.is_none());
}

// =====================================================================
// #846 — cancel-during-tool-execution must emit a synthetic ToolEnd.
//
// Sibling of #816 (cancel during approval-wait, fixed in #845). Both
// cancel arms in `run_tool_calls` (Guarded sequential, line ~603, and
// FullControl/Autonomous parallel, line ~637) race against the inner
// `execute_tool_call` future. When the cancel arm wins, the inner future
// is dropped at an `await` point — `tool_start` was already emitted but
// `tool_end` was not. The runtime must synthesise a matching `ToolEnd`
// before unwinding so consumers (UI, audit log, persisted state) see
// the 1:1 invariant honoured. The frontend defensive sweep that
// previously masked this bug (`use-session-stream.js:1018-1023`, added
// in #594) is removed in the same PR — this test stands alone.
// =====================================================================

/// Test helper: a tool whose `execute()` awaits on a oneshot receiver
/// before returning, letting the test deterministically hold the tool
/// in-flight until it deliberately fires the cancel token.
///
/// Marked `is_auto_approved = true` so it bypasses the Guarded approval
/// gate and the inner future immediately reaches `tools.execute().await`
/// (the cancel-during-tool-execution race window).
#[derive(Debug)]
struct BlockingTestTool {
    name: String,
    /// Drained on each call. Tests pre-load this with one or more
    /// receivers; each `execute()` invocation pops one and awaits it.
    /// Once the channel sender is dropped (without sending), `await`
    /// returns Err and the tool returns an error.
    rx_queue: tokio::sync::Mutex<std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>>,
}

impl BlockingTestTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            rx_queue: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    async fn enqueue(&self, rx: tokio::sync::oneshot::Receiver<()>) {
        self.rx_queue.lock().await.push_back(rx);
    }
}

#[async_trait::async_trait]
impl alms_sandbox::Tool for BlockingTestTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "test-only tool that blocks on a oneshot receiver"
    }
    fn is_auto_approved(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> alms_sandbox::SandboxResult<serde_json::Value> {
        let rx = {
            let mut q = self.rx_queue.lock().await;
            q.pop_front()
        };
        if let Some(rx) = rx {
            // Block until the test fires the sender, OR until the future
            // is dropped (cancel-during-tool-execution race). Drop is the
            // expected path for #846 tests.
            let _ = rx.await;
        }
        Ok(serde_json::json!({"ok": true}))
    }
}

fn make_runtime_for_cancel_test(
    posture: Posture,
    tool: std::sync::Arc<dyn alms_sandbox::Tool>,
    cancel_token: tokio_util::sync::CancellationToken,
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
) -> AgentRuntime {
    let llm_config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    let tools = ToolRegistry::new();
    tools.register(tool);
    AgentRuntime {
        agent_id: AgentId::new(),
        config: AgentConfig {
            posture,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(llm_config).unwrap(),
        summary_llm: None,
        tools,
        workspace: None,
        event_sender: Some(tx),
        run_id: None,
        cancel_token: Some(cancel_token),
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: None,
        dm_implicit_reply: false,
    }
}

/// Spawn a watcher task that consumes events from `rx`, mirrors
/// `(invocation_id, ok, result)` of every ToolEnd into `ends`, marks
/// every ToolStart's `invocation_id` in `starts`, and once the
/// observed `ToolStart` count reaches `cancel_after_n_starts` fires
/// `cancel_token`. This deterministically arranges for the cancel to
/// land after every expected inner future has registered with the
/// in-flight tracker and is parked at its blocking await — removing
/// the dependency on tokio scheduling order that an "always cancel on
/// first start" variant would have in the multi-tool parallel case
/// (Tim's nit on #846).
// Test helper return type is intentionally a tuple of two collections
// — splitting into a named alias would obscure the call sites without
// any reuse benefit (only one caller shape).
#[allow(clippy::type_complexity)]
fn spawn_cancel_on_tool_start(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeEvent>,
    cancel_token: tokio_util::sync::CancellationToken,
    cancel_after_n_starts: usize,
) -> tokio::task::JoinHandle<(
    std::collections::HashSet<uuid::Uuid>,
    Vec<(uuid::Uuid, bool, serde_json::Value)>,
)> {
    assert!(
        cancel_after_n_starts >= 1,
        "cancel_after_n_starts must be at least 1"
    );
    tokio::spawn(async move {
        let mut starts = std::collections::HashSet::new();
        let mut ends = Vec::new();
        let mut cancelled = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                RuntimeEvent::ToolStart { invocation_id, .. } => {
                    starts.insert(invocation_id);
                    // Cancel only once we've observed the expected
                    // number of ToolStarts. For the parallel test (n=3)
                    // this guarantees all 3 inner futures have
                    // registered with the in-flight tracker before the
                    // cancel arm fires, regardless of how the runtime
                    // interleaves the watcher task with `join_all`'s
                    // synchronous walk.
                    if !cancelled && starts.len() >= cancel_after_n_starts {
                        cancel_token.cancel();
                        cancelled = true;
                    }
                }
                RuntimeEvent::ToolEnd {
                    invocation_id,
                    ok,
                    result,
                    ..
                } => {
                    ends.push((invocation_id, ok, result));
                }
                _ => {}
            }
        }
        (starts, ends)
    })
}

/// #846 — Guarded sequential cancel arm: a non-conflicting tool starts
/// executing under Guarded posture (auto-approved → no approval gate),
/// the test cancels mid-execution, and `run_tool_calls` must synthesise
/// a matching `ToolEnd` for the in-flight tool before returning
/// `Cancelled`.
#[tokio::test]
async fn test_cancel_during_tool_execution_emits_tool_end_guarded() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Pre-load one receiver — the matching sender is held by the test
    // and never fired, so the tool will block until its future is
    // dropped by the cancel arm.
    let (_tx_release, rx_release) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(rx_release).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![ToolCall::new("tc1", "block_test", "{}")];
    let invocation_id = uuid::Uuid::new_v4();
    let invocation_ids = vec![invocation_id];

    // Guarded sequential cancel arm — single tool, cancel after the
    // first (only) ToolStart.
    let watcher = spawn_cancel_on_tool_start(rx, cancel_token.clone(), 1);

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &invocation_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime so the event channel closes and the watcher task's
    // `recv` loop terminates.
    drop(runtime);
    let (starts, ends) = watcher.await.unwrap();

    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );
    assert!(
        starts.contains(&invocation_id),
        "tool_start must have been emitted before cancel landed (#846)"
    );

    let our_ends: Vec<_> = ends
        .iter()
        .filter(|(id, _, _)| *id == invocation_id)
        .collect();
    assert_eq!(
        our_ends.len(),
        1,
        "exactly one ToolEnd must be emitted for invocation {} — got {:?}",
        invocation_id,
        ends
    );
    let (_id, ok, result_val) = our_ends[0];
    assert!(!ok, "synthetic tool_end after cancel must report ok=false");
    let err_str = result_val
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        err_str.contains("cancel"),
        "synthetic tool_end result.error should mention cancellation, got {:?}",
        result_val
    );
}

/// #846 — FullControl/Autonomous parallel cancel arm: 3 tools run
/// concurrently in `join_all` under FullControl, the test cancels
/// mid-execution, and `run_tool_calls` must synthesise a matching
/// `ToolEnd` for *each* in-flight tool before returning `Cancelled`.
/// This exercises the harder of the two arms — multiple in-flight calls
/// at cancel time, each needs its own synthetic event.
#[tokio::test]
async fn test_cancel_during_tool_execution_emits_tool_end_parallel() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Three calls — pre-load three receivers, hold all three senders
    // unfired so all three futures park at their blocking awaits.
    let mut held_senders = Vec::new();
    for _ in 0..3 {
        let (s, r) = tokio::sync::oneshot::channel::<()>();
        blocking.enqueue(r).await;
        held_senders.push(s);
    }

    let runtime = make_runtime_for_cancel_test(
        Posture::FullControl,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![
        ToolCall::new("tc1", "block_test", "{}"),
        ToolCall::new("tc2", "block_test", "{}"),
        ToolCall::new("tc3", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();

    // Cancel only after observing all 3 ToolStarts. This removes the
    // dependency on tokio scheduling order — instead of trusting that
    // `join_all`'s synchronous walk polls all 3 inner futures through
    // their first await before the watcher task is scheduled, we wait
    // until the watcher has actually seen 3 ToolStart events, which
    // proves all 3 invocations are registered in the in-flight tracker
    // before the cancel arm fires (Tim's nit on #846).
    let watcher = spawn_cancel_on_tool_start(rx, cancel_token.clone(), 3);

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    drop(runtime);
    drop(held_senders);
    let (starts, ends) = watcher.await.unwrap();

    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    for inv in &inv_ids {
        assert!(
            starts.contains(inv),
            "tool_start missing for invocation {} — test setup broken",
            inv
        );
        let our_ends: Vec<_> = ends.iter().filter(|(id, _, _)| id == inv).collect();
        assert_eq!(
            our_ends.len(),
            1,
            "expected exactly one ToolEnd for invocation {} — got {:?} \
             (the parallel cancel arm must synthesise one ToolEnd per \
             in-flight tool, no more no less; #846)",
            inv,
            ends
        );
        let (_id, ok, result_val) = our_ends[0];
        assert!(
            !ok,
            "synthetic tool_end after cancel must report ok=false (inv {})",
            inv
        );
        let err_str = result_val
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            err_str.contains("cancel"),
            "synthetic tool_end result.error should mention cancellation \
             (inv {}), got {:?}",
            inv,
            result_val
        );
    }
}

/// #1078 — A mixed parallel batch where tool A completes before the
/// cancel arrives and tools B / C are still in-flight at cancel time.
/// The fix in #1078 says: A's `Tool`-role row MUST be persisted to
/// `session_messages`, AND a matching record MUST land in
/// `tool_call_records`, before `Err(Cancelled)` propagates. Pre-fix,
/// A's result was buffered inside `join_all`'s internal vec and dropped
/// when the cancel arm won — the next run's rebuild would synthesise an
/// `INTERRUPTED_TOOL_RESULT` for tool A even though it had succeeded.
///
/// Sequencing is event-driven (Tim's nit on #846, same pattern as the
/// no-double-emission test below): the watcher fires
/// `cancel_token.cancel()` the moment it observes A's `ToolEnd`, which
/// proves A reached its sync terminal section (unregister + audit +
/// emit) BEFORE the cancel could possibly land. Under the single-
/// threaded `#[tokio::test]` executor, that also guarantees the drain
/// loop has written `completed_results[A] = Some(...)` before the
/// select! cancel arm wins, because the drain loop runs
/// cooperatively without yielding between "fu.next() returned A's
/// result" and "next fu.next().await parks on B / C".
#[tokio::test]
async fn test_cancel_persists_completed_parallel_results_1078() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Three calls. We pre-fire A's release sender so its `rx.await`
    // inside `execute()` resolves immediately when polled. B and C's
    // senders are held by the test and never fired — those two futures
    // will stay parked at their blocking awaits forever, so the
    // FuturesUnordered drain loop yields control after collecting A's
    // result, and the cancel arm of the outer `select!` can win.
    let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = release_a_tx.send(());
    blocking.enqueue(release_a_rx).await;

    let (held_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_b_rx).await;
    let (held_c_tx, release_c_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_c_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::FullControl,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    // Three distinct tool_call_ids so we can identify A's persisted
    // `ToolResult` row in the session history afterwards. The first
    // tool call ("tc-A") is the one whose release sender we pre-fired.
    let tool_calls = vec![
        ToolCall::new("tc-A", "block_test", "{}"),
        ToolCall::new("tc-B", "block_test", "{}"),
        ToolCall::new("tc-C", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..3).map(|_| uuid::Uuid::new_v4()).collect();

    // Watcher: fires `cancel_token.cancel()` on the first observed
    // `ToolEnd` (which is tool A's success, because B and C never
    // complete). Mirrors the event-driven sequencing pattern from
    // `test_no_double_tool_end_when_tool_ok_then_cancel`.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut starts = std::collections::HashSet::new();
            let mut ends: Vec<(uuid::Uuid, bool, serde_json::Value)> = Vec::new();
            let mut cancelled = false;
            while let Some(ev) = rx.recv().await {
                match ev {
                    RuntimeEvent::ToolStart { invocation_id, .. } => {
                        starts.insert(invocation_id);
                    }
                    RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        ..
                    } => {
                        ends.push((invocation_id, ok, result));
                        if !cancelled {
                            cancel_token.cancel();
                            cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
            (starts, ends)
        })
    };

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            /* is_dm */ false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime + held senders so the event channel + the still-
    // parked B/C inner futures clean up and the watcher's loop exits.
    drop(runtime);
    drop(held_b_tx);
    drop(held_c_tx);
    let (_starts, ends) = watcher.await.unwrap();

    // 1) The outer call returns Cancelled — A's persistence happens
    //    on the cancel arm of the parallel branch in `run_tool_calls`.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    // 2) Exactly one of the three `ToolEnd` events must report ok=true
    //    — the real success event for tool A. The other two are
    //    synthetic cancel events (ok=false, `error: "run cancelled"`).
    let ok_ends: Vec<_> = ends.iter().filter(|(_, ok, _)| *ok).collect();
    assert_eq!(
        ok_ends.len(),
        1,
        "expected exactly one ok=true ToolEnd (the completed tool A) \
         and two synthetic cancel ToolEnds for B + C, got {:?}",
        ends
    );

    // 3) The session message log must contain exactly one `Role::Tool`
    //    row, and that row's `tool_id` must be tool A's id. Pre-#1078
    //    this row was missing — `process_tool_results` was bypassed by
    //    the cancel arm and A's result was silently dropped.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_rows: Vec<&alms_session::Message> = history
        .iter()
        .filter(|m| matches!(m.role, alms_session::Role::Tool))
        .collect();
    assert_eq!(
        tool_rows.len(),
        1,
        "exactly one Tool-role row must be persisted after cancel \
         (tool A — the one that completed before the cancel landed); \
         tools B and C must NOT be persisted because they were still \
         in-flight when the cancel arrived. got {} rows: {:?}",
        tool_rows.len(),
        tool_rows
    );

    let persisted_tool_id = match &tool_rows[0].content {
        alms_session::Content::ToolResult { tool_id, .. } => tool_id.clone(),
        other => panic!("unexpected Tool-role content shape: {:?}", other),
    };
    assert_eq!(
        persisted_tool_id, "tc-A",
        "the persisted tool result must be tool A's (the one that \
         completed), not B or C. got tool_id={}",
        persisted_tool_id
    );

    // 4) The persisted row's metadata must carry `ok: true` for tool A
    //    (it succeeded before the cancel), so the rebuild pipeline
    //    treats it as a real success and not an interrupted call.
    let meta = tool_rows[0]
        .metadata
        .as_ref()
        .expect("persisted tool result must have metadata");
    assert_eq!(
        meta.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "tool A's persisted ok flag must be true — its execute() returned \
         Ok before the cancel arm fired. metadata={:?}",
        meta
    );

    // 5) `tool_call_records` must include a `ToolCallRole::Tool` entry
    //    for tool A so the gateway's per-run records (#696) surface the
    //    completed tool to the UI / `GET /runs/{id}/tool-calls`.
    let tool_records: Vec<&alms_core::ToolCallRecord> = tool_call_records
        .iter()
        .filter(|r| matches!(r.role, alms_core::ToolCallRole::Tool))
        .collect();
    assert_eq!(
        tool_records.len(),
        1,
        "exactly one Tool-role ToolCallRecord must be appended on the \
         cancel arm (for tool A). got {} records: {:?}",
        tool_records.len(),
        tool_records
    );
    assert_eq!(
        tool_records[0].tool_id.as_deref(),
        Some("tc-A"),
        "the per-run record must reference tool A's tool_id"
    );
}

/// #1090 — Guarded sequential cancel arm: a two-tool batch where tool A
/// completes before the cancel arrives and tool B is still in-flight at
/// cancel time. Same shape as `test_cancel_persists_completed_parallel_results_1078`
/// but for the Guarded `Posture::Guarded` posture's sequential cancel
/// path. Pre-#1090, A's `Tool`-role row was silently dropped because the
/// Guarded cancel arm bypassed `process_tool_results` — the only
/// persistence site for completed tool results — and the next run's
/// rebuild would synthesise `INTERRUPTED_TOOL_RESULT` for A even though
/// it had succeeded.
///
/// Two cancel branches exist in the Guarded arm:
///
/// * **Branch 1**: inter-tool `is_cancelled()` check (cancel landed
///   between iterations). Persists prior `results` entries before
///   unwinding.
/// * **Branch 2**: outer `tokio::select!` cancel arm (cancel arrived
///   mid-tool-execution). Persists prior `results` entries, then
///   synthesises `ToolEnd`s for the in-flight subset via
///   `synthesize_cancel_tool_ends`.
///
/// Under tokio's single-threaded `#[tokio::test]` executor, this test
/// deterministically lands in Branch 2: A's tool body completes
/// synchronously (its release sender is pre-fired so `rx.await` polls
/// straight through), the runtime task pushes A's Ok to `results` and
/// proceeds to B's iteration without yielding, B's `execute_tool_call`
/// emits ToolStart and parks at B's blocking `rx.await`. The watcher
/// task is finally scheduled, sees A's ToolEnd, fires
/// `cancel_token.cancel()`. The outer select's cancel arm wins because
/// B's inner future is parked. Both branches route through the same
/// `persist_completed_guarded_results_on_cancel` helper, so a Branch 2
/// test covers the helper for Branch 1 too — the only difference
/// between branches is which `return Err(Cancelled)` site is reached,
/// and the persistence pass that precedes them is byte-identical.
#[tokio::test]
async fn test_cancel_persists_completed_guarded_results_1090() {
    use tokio_util::sync::CancellationToken;

    // Bring the trait into scope so `is_auto_approved()` resolves below.
    use alms_sandbox::Tool as _;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Pin the auto-approval invariant: this test deterministically
    // covers Branch 2 (cancel-during-tool-execution inside the inner
    // `select!`) only if `BlockingTestTool` bypasses the Guarded
    // approval gate. If a future maintainer flips
    // `is_auto_approved()` to `false` (or adds a manual-approval
    // variant) the runtime would instead park on the approval queue
    // and the cancel arm in `execute_tool_call`'s approval-wait
    // branch would fire — a different code path that does not run
    // `persist_completed_guarded_results_on_cancel`. Failing fast
    // here is preferable to a green test that exercises the wrong
    // branch. (Tim's nit on #1090.)
    debug_assert!(
        blocking.is_auto_approved(),
        "BlockingTestTool must be auto-approved for this test to land in \
         Branch 2 of `run_tool_calls`'s Guarded arm; otherwise the runtime \
         parks on the approval queue and the cancel persistence path under \
         test is bypassed."
    );

    // Two calls — A's release sender is pre-fired so A's `rx.await`
    // inside `execute()` resolves immediately. B's release sender is
    // held by the test and never fired, so B's `execute()` parks at its
    // blocking await forever and the FuturesUnordered drain loop yields
    // control after collecting A's result. Same pattern as the parallel
    // test, scaled down to two tools because the Guarded arm runs
    // sequentially — A must complete before B's iteration even starts.
    let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = release_a_tx.send(());
    blocking.enqueue(release_a_rx).await;

    let (held_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_b_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    // Two distinct tool_call_ids so we can identify A's persisted
    // `ToolResult` row in the session history afterwards.
    let tool_calls = vec![
        ToolCall::new("tc-A", "block_test", "{}"),
        ToolCall::new("tc-B", "block_test", "{}"),
    ];
    let inv_ids: Vec<uuid::Uuid> = (0..2).map(|_| uuid::Uuid::new_v4()).collect();

    // Watcher: fires `cancel_token.cancel()` on the first observed
    // `ToolEnd` (which is tool A's success — B never completes). Mirrors
    // the event-driven sequencing pattern from
    // `test_cancel_persists_completed_parallel_results_1078`.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut starts = std::collections::HashSet::new();
            let mut ends: Vec<(uuid::Uuid, bool, serde_json::Value)> = Vec::new();
            let mut cancelled = false;
            while let Some(ev) = rx.recv().await {
                match ev {
                    RuntimeEvent::ToolStart { invocation_id, .. } => {
                        starts.insert(invocation_id);
                    }
                    RuntimeEvent::ToolEnd {
                        invocation_id,
                        ok,
                        result,
                        ..
                    } => {
                        ends.push((invocation_id, ok, result));
                        if !cancelled {
                            cancel_token.cancel();
                            cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
            (starts, ends)
        })
    };

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &inv_ids,
            &[],
            &session_manager,
            session.id,
            /* is_dm */ false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;

    // Drop runtime + held sender so the event channel + the still-
    // parked B inner future clean up and the watcher's loop exits.
    drop(runtime);
    drop(held_b_tx);
    let (_starts, ends) = watcher.await.unwrap();

    // 1) The outer call returns Cancelled — A's persistence happens on
    //    the cancel arm of the Guarded branch in `run_tool_calls`.
    assert!(
        matches!(result, Err(AlmsError::Cancelled)),
        "expected AlmsError::Cancelled, got {:?}",
        result
    );

    // 2) Exactly one of the two `ToolEnd` events must report ok=true
    //    — the real success event for tool A. The other is the synthetic
    //    cancel event for B (ok=false, `error: "run cancelled"`).
    let ok_ends: Vec<_> = ends.iter().filter(|(_, ok, _)| *ok).collect();
    assert_eq!(
        ok_ends.len(),
        1,
        "expected exactly one ok=true ToolEnd (the completed tool A) \
         and one synthetic cancel ToolEnd for B, got {:?}",
        ends
    );

    // 3) The session message log must contain exactly one `Role::Tool`
    //    row, and that row's `tool_id` must be tool A's id. Pre-#1090
    //    this row was missing — `process_tool_results` was bypassed by
    //    the Guarded cancel arm and A's result was silently dropped.
    let history = session_manager.get_history(session.id).unwrap();
    let tool_rows: Vec<&alms_session::Message> = history
        .iter()
        .filter(|m| matches!(m.role, alms_session::Role::Tool))
        .collect();
    assert_eq!(
        tool_rows.len(),
        1,
        "exactly one Tool-role row must be persisted after Guarded cancel \
         (tool A — the one that completed before the cancel landed); \
         tool B must NOT be persisted because it was still in-flight \
         when the cancel arrived. got {} rows: {:?}",
        tool_rows.len(),
        tool_rows
    );

    let persisted_tool_id = match &tool_rows[0].content {
        alms_session::Content::ToolResult { tool_id, .. } => tool_id.clone(),
        other => panic!("unexpected Tool-role content shape: {:?}", other),
    };
    assert_eq!(
        persisted_tool_id, "tc-A",
        "the persisted tool result must be tool A's (the one that \
         completed), not B. got tool_id={}",
        persisted_tool_id
    );

    // 4) The persisted row's metadata must carry `ok: true` for tool A.
    let meta = tool_rows[0]
        .metadata
        .as_ref()
        .expect("persisted tool result must have metadata");
    assert_eq!(
        meta.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "tool A's persisted ok flag must be true — its execute() returned \
         Ok before the cancel arm fired. metadata={:?}",
        meta
    );

    // 5) `tool_call_records` must include a `ToolCallRole::Tool` entry
    //    for tool A so the gateway's per-run records surface the
    //    completed tool to the UI / `GET /runs/{id}/tool-calls`.
    let tool_records: Vec<&alms_core::ToolCallRecord> = tool_call_records
        .iter()
        .filter(|r| matches!(r.role, alms_core::ToolCallRole::Tool))
        .collect();
    assert_eq!(
        tool_records.len(),
        1,
        "exactly one Tool-role ToolCallRecord must be appended on the \
         Guarded cancel arm (for tool A). got {} records: {:?}",
        tool_records.len(),
        tool_records
    );
    assert_eq!(
        tool_records[0].tool_id.as_deref(),
        Some("tc-A"),
        "the per-run record must reference tool A's tool_id"
    );
}

/// #846 — No-double-emission regression: a tool that finishes Ok
/// followed (in real time) by a cancel arrival must not produce two
/// `ToolEnd` events for the same invocation. The unregister-before-emit
/// protocol inside `execute_tool_call` ensures the entry is gone from
/// the in-flight tracker before the outer cancel arm could even see it.
///
/// Sequencing is event-driven, not wall-clock based (Tim's nit on
/// #846): the watcher task fires `cancel_token.cancel()` the moment it
/// observes a `ToolEnd`, which proves the success path's
/// unregister-before-emit step has already run by the time the cancel
/// could possibly land. Nothing depends on `tokio::time::sleep`.
#[tokio::test]
async fn test_no_double_tool_end_when_tool_ok_then_cancel() {
    use tokio_util::sync::CancellationToken;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let cancel_token = CancellationToken::new();

    let blocking = std::sync::Arc::new(BlockingTestTool::new("block_test"));
    // Single call with a sender that we WILL fire before cancelling —
    // forces the inner branch of `select!` to win and emit the normal
    // success ToolEnd. The cancel arrives after, by which point the
    // tracker is empty so no synthetic should be emitted.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    blocking.enqueue(release_rx).await;

    let runtime = make_runtime_for_cancel_test(
        Posture::Guarded,
        blocking.clone() as std::sync::Arc<dyn alms_sandbox::Tool>,
        cancel_token.clone(),
        tx,
    );

    let session_config = SessionConfig::default();
    let session_manager = SessionManager::new(session_config);
    let session = session_manager.get_or_create(runtime.agent_id, "test");

    let tool_calls = vec![ToolCall::new("tc1", "block_test", "{}")];
    let invocation_id = uuid::Uuid::new_v4();
    let invocation_ids = vec![invocation_id];

    // Watcher: counts ToolEnd events for this invocation, and on the
    // FIRST observed ToolEnd fires `cancel_token.cancel()`. Because
    // the success ToolEnd is sent only AFTER the inner future has
    // already removed itself from the in-flight tracker (the
    // unregister-before-emit protocol in `execute_tool_call`), this
    // guarantees that the cancel — if it races into the outer
    // `run_tool_calls` cancel arm at all — finds an empty tracker and
    // emits zero synthetic ToolEnds. Replaces a fragile sleep(20ms)/
    // sleep(200ms) pair with deterministic event-based sequencing.
    let watcher = {
        let cancel_token = cancel_token.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            let mut tool_end_count = 0usize;
            while let Some(ev) = rx.recv().await {
                if let RuntimeEvent::ToolEnd {
                    invocation_id: id, ..
                } = ev
                    && id == invocation_id
                {
                    tool_end_count += 1;
                    if tool_end_count == 1 {
                        cancel_token.cancel();
                    }
                }
            }
            tool_end_count
        })
    };

    // Release the tool synchronously — no wall-clock wait needed. The
    // runtime has not started yet, but the inner await on the receiver
    // sees the value as soon as it polls.
    let _ = release_tx.send(());

    let mut tool_call_records: Vec<alms_core::ToolCallRecord> = Vec::new();
    let mut tool_seq: u32 = 0;
    let result = runtime
        .run_tool_calls(
            &tool_calls,
            &invocation_ids,
            &[],
            &session_manager,
            session.id,
            false,
            &mut tool_call_records,
            &mut tool_seq,
        )
        .await;
    drop(runtime);

    let tool_end_count = watcher.await.unwrap();

    assert!(
        result.is_ok(),
        "tool finished normally, run_tool_calls should return Ok, got {:?}",
        result
    );
    assert_eq!(
        tool_end_count, 1,
        "exactly one ToolEnd must be emitted per invocation — even if a \
         cancel arrives after the inner future already removed itself \
         from the in-flight tracker (#846 no-double-emission protocol)"
    );
}

/// #866: `with_summary_llm` populates the dedicated summary client.
///
/// Default state: `summary_llm` is `None` so the in-loop compact
/// path (renamed from `sliding-summary` in #869) falls back to
/// `self.llm` (pre-#866 behaviour).
///
/// After `with_summary_llm`, the field is `Some(client)` whose provider
/// can differ from `self.llm`'s provider. This is the path the gateway
/// uses when `[context].summary_provider` is configured: it clones the
/// agent's resolved client, calls `with_provider_and_secrets` on the
/// clone, and hands the re-targeted client to the runtime via
/// `with_summary_llm`. The runtime never re-resolves the provider — it
/// just reads back what the gateway plumbed in.
#[tokio::test]
async fn test_with_summary_llm_sets_dedicated_client_for_866() {
    use crate::LlmConfig;

    let agent_llm = LlmClient::new(LlmConfig {
        provider: "anthropic".into(),
        default_model: "claude-sonnet-4-20250514".into(),
        ..LlmConfig::default()
    })
    .unwrap();

    // Build a separate client for the summary task on a different
    // provider — exactly what the gateway does when
    // `[context].summary_provider = "openrouter"` is set.
    let summary_llm = LlmClient::new(LlmConfig {
        provider: "openrouter".into(),
        default_model: "minimax/minimax-m2.7".into(),
        ..LlmConfig::default()
    })
    .unwrap();

    let runtime =
        AgentRuntime::new(AgentId::new(), AgentConfig::default(), agent_llm.clone()).unwrap();

    // Default: no summary client wired -> agent's llm is used.
    assert!(
        runtime.summary_llm.is_none(),
        "AgentRuntime::new must leave summary_llm as None for back-compat"
    );
    assert_eq!(runtime.llm.provider(), "anthropic");

    // After with_summary_llm, the dedicated client is present and points
    // at a different provider than the agent's llm. The agent's primary
    // llm (used for the main run loop) is untouched.
    let runtime = runtime.with_summary_llm(summary_llm);
    let summary_client = runtime
        .summary_llm
        .as_ref()
        .expect("with_summary_llm must populate the field");
    assert_eq!(
        summary_client.provider(),
        "openrouter",
        "summary client must keep the override provider"
    );
    assert_eq!(
        runtime.llm.provider(),
        "anthropic",
        "agent's main llm must NOT be re-targeted by with_summary_llm"
    );
}

// ── #851 — In-loop tool-output truncation integration ───────────────────────

mod tool_output_truncate_integration {
    use super::*;
    use crate::tool_output_truncate::ToolOutputTruncatePolicy;

    /// Build a minimal `AgentRuntime` with the in-loop truncation service
    /// active and pointed at `run_dir`. No LLM or workspace — we are
    /// driving `process_tool_results` directly.
    fn make_runtime_with_truncate(run_dir: std::path::PathBuf) -> AgentRuntime {
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let mut policy = ToolOutputTruncatePolicy::with_run_dir(run_dir);
        // Use small caps so the test stays fast and the assertions are
        // concrete (32 KB is too big to construct quickly in a unit test
        // and offers no extra coverage).
        policy.max_bytes = 4 * 1024;
        policy.max_lines = 100;
        AgentRuntime {
            agent_id: AgentId::new(),
            config: AgentConfig::default(),
            llm: LlmClient::new(llm_config).unwrap(),
            summary_llm: None,
            tools: ToolRegistry::new(),
            workspace: None,
            event_sender: None,
            run_id: None,
            cancel_token: None,
            resolved_sandbox_root: None,
            shell_unrestricted: true,
            shell_default_env: std::collections::HashMap::new(),
            shell_permissions: alms_core::config::ShellPermissions::default(),
            shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
            shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
            tool_output_truncate_policy: policy,
            extra_fs_read_roots: Vec::new(),
            agent_name: None,
            dm_implicit_reply: false,
        }
    }

    /// Regression for the subagent-shaped builder path (#921): constructing
    /// the runtime through `AgentRuntime::new(...).with_tool_output_truncate(...)`
    /// must activate the policy, place spill files under the supplied
    /// `tool-output/sub-{task_id}/` directory, bound both the in-loop and
    /// audit/SSE previews, and persist the truncation metadata used when the
    /// session is reconstructed on a later run.
    #[tokio::test]
    async fn builder_shaped_subagent_runtime_truncates_oversized_tool_output() {
        let data_dir = tempfile::tempdir().unwrap();
        let task_id = uuid::Uuid::new_v4();
        let sub_run_dir = data_dir
            .path()
            .join(crate::tool_output_truncate::TOOL_OUTPUT_DIR_NAME)
            .join(format!("sub-{task_id}"));

        let config = AgentConfig {
            tool_output_truncate: alms_core::config::ToolOutputTruncateConfig {
                enabled: true,
                max_bytes: 4 * 1024,
                max_lines: 100,
                retention_days: 7,
            },
            ..AgentConfig::default()
        };
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let runtime =
            AgentRuntime::new(AgentId::new(), config, LlmClient::new(llm_config).unwrap())
                .unwrap()
                .with_tool_output_truncate(sub_run_dir.clone(), true, 4 * 1024, 100);

        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(AgentId::new(), "subagent-test");
        let tool_call = ToolCall::new("call_subagent_huge", "fake_tool", "{}");
        let result_value = serde_json::Value::String("x".repeat(100 * 1024));
        let mut messages = Vec::new();
        let mut records = Vec::new();
        let mut seq = 0;
        let invocation_ids = vec![uuid::Uuid::new_v4()];

        runtime.process_tool_results(
            std::slice::from_ref(&tool_call),
            vec![Ok(result_value.clone())],
            &invocation_ids,
            &mut messages,
            &mut records,
            &mut seq,
            &session_manager,
            session.id,
            false,
        );

        let spill_path = sub_run_dir.join("tool_call_subagent_huge.txt");
        assert!(
            spill_path.exists(),
            "subagent spill must land under tool-output/sub-{}/: {}",
            task_id,
            spill_path.display()
        );
        assert_eq!(
            std::fs::read_to_string(&spill_path).unwrap(),
            result_value.to_string(),
            "spill bytes must match the original JSON-stringified result"
        );

        assert_eq!(messages.len(), 1);
        let preview = messages[0].content_str();
        assert!(
            preview.len() < 8 * 1024,
            "subagent in-loop preview must be capped: {} bytes",
            preview.len()
        );

        let emit_value = runtime.truncate_for_emit(&tool_call.id, &result_value);
        let emit_preview = emit_value
            .as_str()
            .expect("truncated emit value must be a JSON string");
        assert!(
            emit_preview.len() < 8 * 1024,
            "subagent audit/SSE preview must be capped: {} bytes",
            emit_preview.len()
        );

        let history = session_manager.get_history(session.id).unwrap();
        let tool_msg = history
            .iter()
            .find(|message| matches!(message.role, alms_session::Role::Tool))
            .expect("subagent tool result must be persisted");
        let metadata = tool_msg.metadata.as_ref().expect("metadata must be set");
        assert_eq!(
            metadata
                .get("truncated_in_loop")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            metadata
                .get("spill_path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| path.contains("sub-")),
            "persisted spill_path must reference the subagent dir: {metadata:?}"
        );
    }

    /// Drive `process_tool_results` with a single oversized tool result
    /// and assert: the in-loop messages carry the truncated preview, the
    /// session DB row has `truncated_in_loop: true` metadata, and the
    /// spill file on disk holds the full original bytes.
    #[tokio::test]
    async fn process_tool_results_truncates_oversized_output() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        // Synthesize a tool call + a 100 KB JSON-string result. 100 KB of
        // 'x' characters serialise to a string longer than the 4 KB
        // policy cap, so truncation will fire.
        let tool_call = ToolCall::new("call_huge", "fake_tool", "{}");
        let raw_text = "x".repeat(100 * 1024);
        let result_value = serde_json::Value::String(raw_text.clone());

        let mut messages: Vec<LlmMessage> = Vec::new();
        let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut seq: u32 = 0;
        let invocation_ids = vec![uuid::Uuid::new_v4()];

        runtime.process_tool_results(
            std::slice::from_ref(&tool_call),
            vec![Ok(result_value)],
            &invocation_ids,
            &mut messages,
            &mut records,
            &mut seq,
            &session_manager,
            session.id,
            /* is_dm */ false,
        );

        // 1) The in-loop messages vec must carry the truncated preview,
        //    not the original.
        assert_eq!(messages.len(), 1);
        let preview = messages[0].content_str();
        assert!(
            preview.len() < raw_text.len(),
            "preview must be smaller than the original: preview={} raw={}",
            preview.len(),
            raw_text.len()
        );
        assert!(
            preview.contains("call_huge") || preview.contains("tool_call_huge"),
            "preview must reference the spill file by id"
        );
        assert!(preview.contains("Use `fs_grep`"));

        // 2) The spill file must exist and hold the full original bytes.
        let spill_path = dir.path().join("tool_call_huge.txt");
        assert!(spill_path.exists(), "spill file must be written");
        let spilled = std::fs::read_to_string(&spill_path).unwrap();
        assert_eq!(spilled, serde_json::Value::String(raw_text).to_string());

        // 3) The persisted session message must carry the truncation flag
        //    so `session_msg_to_llm` skips its own re-truncation pass.
        let history = session_manager.get_history(session.id).unwrap();
        let tool_msg = history
            .iter()
            .find(|m| matches!(m.role, alms_session::Role::Tool))
            .expect("tool result message must be persisted");
        let meta = tool_msg.metadata.as_ref().expect("metadata must be set");
        assert_eq!(
            meta.get("truncated_in_loop").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(meta.get("spill_path").and_then(|v| v.as_str()).is_some());
        assert!(
            meta.get("original_bytes")
                .and_then(|v| v.as_u64())
                .is_some()
        );
    }

    /// Symmetric counterpart: when the tool output is small, no
    /// truncation, no spill file, no `truncated_in_loop` flag.
    #[tokio::test]
    async fn process_tool_results_passes_small_output_through() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let tool_call = ToolCall::new("call_small", "fake_tool", "{}");
        let result_value = serde_json::json!({"hello": "world"});

        let mut messages: Vec<LlmMessage> = Vec::new();
        let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut seq: u32 = 0;
        let invocation_ids = vec![uuid::Uuid::new_v4()];

        runtime.process_tool_results(
            std::slice::from_ref(&tool_call),
            vec![Ok(result_value)],
            &invocation_ids,
            &mut messages,
            &mut records,
            &mut seq,
            &session_manager,
            session.id,
            false,
        );

        assert_eq!(messages.len(), 1);
        assert!(messages[0].content_str().contains("\"hello\":\"world\""));

        // No spill file should exist.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "small output must not create a spill file"
        );

        // Metadata must NOT carry the truncated_in_loop flag.
        let history = session_manager.get_history(session.id).unwrap();
        let tool_msg = history
            .iter()
            .find(|m| matches!(m.role, alms_session::Role::Tool))
            .expect("tool result message must be persisted");
        let meta = tool_msg.metadata.as_ref().expect("metadata must be set");
        let flag = meta
            .get("truncated_in_loop")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(!flag, "small output must not be marked truncated_in_loop");
    }

    /// #851 acceptance test: simulate three sequential 50 KB tool calls
    /// inside a single run loop and assert the cumulative size of the
    /// LLM-visible messages stays bounded by the policy cap rather than
    /// growing 50/100/150 KB across turns.
    #[tokio::test]
    async fn three_sequential_50kb_calls_stay_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

        let session_manager = SessionManager::new(SessionConfig::default());
        let session = session_manager.get_or_create(runtime.agent_id, "test");

        let mut messages: Vec<LlmMessage> = Vec::new();
        let mut records: Vec<alms_core::ToolCallRecord> = Vec::new();
        let mut seq: u32 = 0;

        for i in 0..3u32 {
            let tool_call = ToolCall::new(format!("call_{i}"), "fake_tool", "{}");
            let result_value = serde_json::Value::String("x".repeat(50 * 1024));
            let invocation_ids = vec![uuid::Uuid::new_v4()];

            runtime.process_tool_results(
                std::slice::from_ref(&tool_call),
                vec![Ok(result_value)],
                &invocation_ids,
                &mut messages,
                &mut records,
                &mut seq,
                &session_manager,
                session.id,
                false,
            );
        }

        // Each preview is well under the 4 KB cap + ~1 KB hint, so the
        // cumulative size of the in-loop messages must stay under
        // 3 * (4 KB + 2 KB) = 18 KB instead of the unbounded 3 * 50 KB
        // = 150 KB pre-#851 case.
        let cumulative: usize = messages.iter().map(|m| m.content_str().len()).sum();
        assert!(
            cumulative < 18 * 1024,
            "three 50 KB calls must collapse under the cap: got {cumulative} bytes"
        );

        // All three spills must exist on disk so the agent can recover
        // any of them via fs_read.
        for i in 0..3 {
            let spill = dir.path().join(format!("tool_call_{i}.txt"));
            assert!(
                spill.exists(),
                "spill {i} must be written: {}",
                spill.display()
            );
        }
    }

    /// `truncate_for_emit` must:
    ///   - return the original `Value` unchanged when truncation is a
    ///     no-op (small payload OR disabled policy)
    ///   - return a `Value::String(preview)` when truncation fires
    ///   - write the spill file as a side effect when truncation fires
    ///
    /// Pre-#921 review fix #4, the audit log + `ToolEnd` SSE emitted the
    /// raw untruncated bytes regardless of size. Post-fix, both surfaces
    /// see the same preview the agent loop sees.
    #[tokio::test]
    async fn truncate_for_emit_passes_small_value_through() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

        let value = serde_json::json!({"hello": "world"});
        let out = runtime.truncate_for_emit("call_small", &value);
        assert_eq!(out, value, "small value must pass through unchanged");

        // No spill file should have been written.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "small value must not create a spill file"
        );
    }

    #[tokio::test]
    async fn truncate_for_emit_truncates_oversized_value() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = make_runtime_with_truncate(dir.path().to_path_buf());

        // Use a 100 KB string — above the 4 KB cap configured by
        // `make_runtime_with_truncate`.
        let value = serde_json::Value::String("y".repeat(100 * 1024));
        let out = runtime.truncate_for_emit("call_emit_big", &value);

        // The emitted value must be a JSON string (the preview), not the
        // original 100 KB structured value.
        let s = out.as_str().expect("truncated emit value must be a string");
        assert!(
            s.len() < 100 * 1024,
            "preview must be smaller than the original: {} vs {}",
            s.len(),
            100 * 1024
        );
        assert!(s.contains("call_emit_big"));
        assert!(s.contains("Use `fs_grep`"));

        // Spill file must exist.
        let spill = dir.path().join("tool_call_emit_big.txt");
        assert!(spill.exists(), "spill must be written by truncate_for_emit");
    }

    /// `truncate_for_emit` and `process_tool_results` are independent
    /// callers of the same `truncate` service. When the service is
    /// disabled, neither path should write a spill file.
    #[tokio::test]
    async fn truncate_for_emit_is_no_op_when_policy_disabled() {
        // Construct a runtime with an explicitly disabled policy.
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = make_runtime_with_truncate(dir.path().to_path_buf());
        runtime.tool_output_truncate_policy =
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled();

        let value = serde_json::Value::String("z".repeat(100 * 1024));
        let out = runtime.truncate_for_emit("call_disabled", &value);
        assert_eq!(
            out, value,
            "disabled policy must pass even an oversized value through unchanged"
        );

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "disabled policy must not create a spill file"
        );
    }
}

/// Tests for the #921 review fix #1 — `extra_fs_read_roots` accumulator
/// pattern. The pre-fix code re-registered fs_* tools inside both
/// `with_shell_spill` and `with_tool_output_truncate`, but `with_workspace`
/// (called LAST in the gateway lifecycle order) silently overwrote those
/// extras. The fix replaces the per-builder fs_* re-registration with a
/// single `extra_fs_read_roots` accumulator that every fs_* registration
/// site reads from.
#[cfg(test)]
mod fs_read_roots_accumulator {
    use super::*;
    use crate::workspace::AgentWorkspace;

    fn make_runtime_with_sandbox(sandbox_root: std::path::PathBuf) -> AgentRuntime {
        let cfg = AgentConfig {
            sandbox_root: sandbox_root.to_string_lossy().into_owned(),
            shell_policy: "sandboxed".into(),
            ..AgentConfig::default()
        };
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap()).unwrap()
    }

    /// `with_workspace` followed by `with_shell_spill` and
    /// `with_tool_output_truncate` must end up with the same accumulated
    /// extras as the documented gateway order
    /// (`with_shell_spill` → `with_tool_output_truncate` → `with_workspace`).
    ///
    /// Pre-fix the documented order silently dropped the spill extras
    /// because `with_workspace` overwrote them. Post-fix the accumulator
    /// is the single source of truth and the order does not matter.
    #[test]
    fn extras_survive_either_call_order() {
        let sandbox = tempfile::tempdir().unwrap();
        // Create the workspace as a subdirectory so canonicalize() resolves.
        let ws_dir = sandbox.path().join("agent");
        std::fs::create_dir_all(&ws_dir).unwrap();
        let shell_dir = sandbox.path().join("shell_output").join("run-1");
        let trunc_dir = sandbox.path().join("tool-output").join("run-1");
        std::fs::create_dir_all(&shell_dir).unwrap();
        std::fs::create_dir_all(&trunc_dir).unwrap();

        // Order A: spill → truncate → workspace (the documented gateway order)
        let runtime_a = make_runtime_with_sandbox(sandbox.path().to_path_buf())
            .with_shell_spill(shell_dir.clone(), true)
            .with_tool_output_truncate(trunc_dir.clone(), true, 32 * 1024, 2000)
            .with_workspace(AgentWorkspace::with_dir(ws_dir.clone()));

        // Order B: workspace first, then the spill builders. Pre-fix this
        // would have produced a different (broader) extras set than A
        // because workspace's overwrite of fs_* tools dropped the trunc
        // dir. Post-fix the accumulator collects every spill dir
        // regardless of when the workspace registration happened.
        let runtime_b = make_runtime_with_sandbox(sandbox.path().to_path_buf())
            .with_workspace(AgentWorkspace::with_dir(ws_dir.clone()))
            .with_shell_spill(shell_dir.clone(), true)
            .with_tool_output_truncate(trunc_dir.clone(), true, 32 * 1024, 2000);

        // Both runtimes must have both spill dirs in their accumulator,
        // regardless of call order. (We compare as a sorted set so a
        // future re-ordering of the push sites doesn't break the test.)
        let mut a: Vec<_> = runtime_a.extra_fs_read_roots.iter().collect();
        let mut b: Vec<_> = runtime_b.extra_fs_read_roots.iter().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "spill extras must be order-independent");
        assert!(
            a.iter()
                .any(|p| p.ends_with("shell_output/run-1") || p.ends_with("shell_output\\run-1")),
            "shell_output spill dir must be in extras: {:?}",
            a
        );
        assert!(
            a.iter()
                .any(|p| p.ends_with("tool-output/run-1") || p.ends_with("tool-output\\run-1")),
            "tool-output spill dir must be in extras: {:?}",
            a
        );
    }
}

/// Tests for #947 — the `[security].allow_full_os_access` operator
/// escape hatch from the project-root sandbox. The runtime exposes
/// `AgentRuntime::with_unrestricted_filesystem()` which the gateway
/// invokes for listed agents in lieu of `with_project_root`.
#[cfg(test)]
mod allow_full_os_access {
    use super::*;

    /// Build a runtime whose `AgentRuntime::new` resolved the
    /// project-as-sandbox path. Mirrors what the gateway does for an
    /// agent that is NOT on `allow_full_os_access`. This is the baseline
    /// the test compares the unsandboxed variant against.
    fn make_sandboxed_runtime(sandbox_root: std::path::PathBuf) -> AgentRuntime {
        let cfg = AgentConfig {
            sandbox_root: sandbox_root.to_string_lossy().into_owned(),
            shell_policy: "sandboxed".into(),
            ..AgentConfig::default()
        };
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap()).unwrap()
    }

    /// `with_unrestricted_filesystem` clears `resolved_sandbox_root` so
    /// every fs_* tool now operates without a path prefix to enforce.
    /// This is the runtime-side proof of the issue's "listed agent:
    /// `fs_read('/etc/passwd')` succeeds" acceptance criterion.
    #[test]
    fn clears_resolved_sandbox_root() {
        let sandbox = tempfile::tempdir().unwrap();
        let runtime = make_sandboxed_runtime(sandbox.path().to_path_buf());
        // Before: sandbox is active.
        assert!(
            runtime.resolved_sandbox_root.is_some(),
            "AgentRuntime::new must populate resolved_sandbox_root from \
             config.sandbox_root for an agent that is NOT on allow_full_os_access"
        );

        let runtime = runtime.with_unrestricted_filesystem();
        assert!(
            runtime.resolved_sandbox_root.is_none(),
            "with_unrestricted_filesystem must clear resolved_sandbox_root \
             so fs_* tools have no path prefix to enforce (#947)"
        );
        assert!(
            runtime.shell_unrestricted,
            "with_unrestricted_filesystem must set shell_unrestricted = true \
             so the runtime invariant `resolved_sandbox_root.is_none() ↔ \
             shell_unrestricted` is preserved"
        );
    }

    /// After `with_unrestricted_filesystem`, an `fs_read` of an
    /// out-of-sandbox path must succeed (subject only to OS perms),
    /// while the sandboxed baseline blocks the same path. This is the
    /// load-bearing acceptance check from the issue: listed agent reads
    /// outside project root, unlisted agent does not.
    #[tokio::test]
    async fn unsandboxed_fs_read_reaches_outside_sandbox() {
        // Create two sibling tempdirs: the sandbox root and a sibling
        // "outside" directory holding a file the agent must not be able
        // to read in the sandboxed case.
        let sandbox = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret_path = outside.path().join("outside.txt");
        std::fs::write(&secret_path, b"hidden\n").unwrap();

        // Baseline: sandboxed agent — fs_read of the outside path is rejected.
        let sandboxed = make_sandboxed_runtime(sandbox.path().to_path_buf());
        let result_blocked = sandboxed
            .tools
            .execute(
                "fs_read",
                serde_json::json!({ "path": secret_path.to_string_lossy() }),
            )
            .await;
        assert!(
            result_blocked.is_err(),
            "sandboxed agent must NOT be able to fs_read a path outside \
             the project-root sandbox — #945 boundary"
        );

        // Listed agent: fs_read of the same path succeeds.
        let unsandboxed =
            make_sandboxed_runtime(sandbox.path().to_path_buf()).with_unrestricted_filesystem();
        let result_ok = unsandboxed
            .tools
            .execute(
                "fs_read",
                serde_json::json!({ "path": secret_path.to_string_lossy() }),
            )
            .await
            .expect("listed agent must be able to fs_read outside the project sandbox");
        let body = result_ok
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default();
        assert!(
            body.contains("hidden"),
            "fs_read should return the file's content for the listed agent — \
             got: {result_ok}"
        );
    }

    /// `shell_permissions` deny entries (#717) still apply to a listed
    /// agent — `allow_full_os_access` only drops the project-root
    /// filesystem boundary, not the operator's independent denylist
    /// policy. This is the issue's
    /// "shell_permissions deny entries still block listed agents"
    /// acceptance check.
    #[tokio::test]
    async fn shell_permissions_deny_still_applies() {
        let sandbox = tempfile::tempdir().unwrap();
        let cfg = AgentConfig {
            sandbox_root: sandbox.path().to_string_lossy().into_owned(),
            shell_policy: "sandboxed".into(),
            shell_permissions: alms_core::config::ShellPermissions {
                allowed_commands: vec![],
                denied_commands: vec![r"^rm\b".into()],
                classifier_overrides: vec![],
            },
            // Disable the classifier floor so the test exercises the
            // shell_permissions denylist path specifically — without
            // this, `rm -rf /` would be blocked by the classifier
            // before the denylist check runs.
            shell_classification_mode: alms_core::config::ShellClassificationMode::Off,
            ..AgentConfig::default()
        };
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let runtime = AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap())
            .expect("AgentRuntime::new")
            .with_unrestricted_filesystem();

        let denied = runtime
            .tools
            .execute(
                "shell",
                serde_json::json!({ "command": "rm -rf /tmp/deleteme" }),
            )
            .await;
        // The shell tool surfaces the deny as either an Err or a
        // success-with-error-payload depending on how it normalises
        // policy violations. Either shape proves the denylist was
        // honoured — what we must NOT see is the command running.
        let denied_outcome = format!("{denied:?}");
        assert!(
            denied_outcome.to_lowercase().contains("denied")
                || denied_outcome.to_lowercase().contains("blocked")
                || denied_outcome.to_lowercase().contains("permission")
                || denied.is_err(),
            "shell_permissions denylist must block `rm -rf` even for an \
             agent on allow_full_os_access — got: {denied_outcome}"
        );
    }

    /// The destructive-command classifier (#745) still applies to a
    /// listed agent — `allow_full_os_access` only drops the project-root
    /// filesystem boundary, not the classifier floor. This is the
    /// issue's acceptance criterion 5b ("classifier still blocks listed
    /// agents") and the sibling check to
    /// `shell_permissions_deny_still_applies`: that test isolates the
    /// `shell_permissions` denylist by setting
    /// `shell_classification_mode = Off`; this test isolates the
    /// classifier by leaving the denylist empty and setting the
    /// classifier to its default `BlockDestructive` mode. Together they
    /// prove both independent operator-policy gates remain active for
    /// an unsandboxed agent.
    #[tokio::test]
    async fn classifier_floor_still_blocks_unsandboxed_agent() {
        let sandbox = tempfile::tempdir().unwrap();
        let cfg = AgentConfig {
            sandbox_root: sandbox.path().to_string_lossy().into_owned(),
            shell_policy: "sandboxed".into(),
            // Empty permissions — no allowlist, no denylist — so the
            // classifier is the only thing standing between this
            // command and execution. The classifier mode is the
            // `BlockDestructive` default, made explicit here for the
            // test record.
            shell_permissions: alms_core::config::ShellPermissions {
                allowed_commands: vec![],
                denied_commands: vec![],
                classifier_overrides: vec![],
            },
            shell_classification_mode: alms_core::config::ShellClassificationMode::BlockDestructive,
            ..AgentConfig::default()
        };
        let llm_config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let runtime = AgentRuntime::new(AgentId::new(), cfg, LlmClient::new(llm_config).unwrap())
            .expect("AgentRuntime::new")
            .with_unrestricted_filesystem();

        // `rm -rf /etc` is the canonical destructive-command target the
        // classifier flags as `Destructive`. With
        // `allow_full_os_access` lifting the sandbox, the only thing
        // blocking this command is the classifier floor — exactly what
        // we are testing.
        let blocked = runtime
            .tools
            .execute("shell", serde_json::json!({ "command": "rm -rf /etc" }))
            .await;
        let blocked_outcome = format!("{blocked:?}");
        assert!(
            blocked_outcome.to_lowercase().contains("classifier")
                || blocked_outcome.to_lowercase().contains("blocked")
                || blocked_outcome.to_lowercase().contains("destructive")
                || blocked.is_err(),
            "destructive-command classifier must block `rm -rf /etc` \
             even for an agent on allow_full_os_access — got: \
             {blocked_outcome}"
        );
    }

    /// Worktree-mode sibling-reads guard test (#946).
    ///
    /// When an agent runs in `WorktreeMode::Git`, the gateway pins the
    /// sandbox at `<project>/.alms/worktrees/<name>/` instead of the
    /// project root. The agent must STILL be able to read sibling
    /// personality metadata at
    /// `<project>/.alms/agents/<sibling>/personality.md`, which sits
    /// OUTSIDE the worktree directory.
    ///
    /// The gateway's run-lifecycle wiring achieves this by calling
    /// `with_extra_fs_read_root(<project>/.alms/agents/)` BEFORE
    /// `with_project_root(<worktree_path>)`. This test pins that
    /// invariant: if a future refactor reorders the calls or drops
    /// the extra read root, the parent agent loses access to sibling
    /// personality and multi-agent coordination silently breaks.
    #[tokio::test]
    async fn test_worktree_mode_sibling_reads_via_extra_read_root() {
        use crate::workspace::AgentWorkspace;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let project_root = dir.path().to_path_buf();
        let agents_dir = project_root.join(".alms").join("agents");
        let worktree_dir = project_root.join(".alms").join("worktrees").join("parent");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::create_dir_all(&worktree_dir).unwrap();

        // Sibling metadata lives at `.alms/agents/sibling/` — OUTSIDE
        // the worktree dir, but reachable via the extra-read-roots
        // shim the gateway wires up.
        let sibling_meta_dir = agents_dir.join("sibling");
        std::fs::create_dir_all(&sibling_meta_dir).unwrap();
        let sibling_personality = sibling_meta_dir.join("personality.md");
        std::fs::write(&sibling_personality, "I am the sibling.\n").unwrap();

        // Build the parent agent exactly as the worktree-mode-git
        // path of the gateway run lifecycle wires it: extra read
        // root FIRST (sibling reads), then `with_project_root` at
        // the worktree, then `with_workspace` for the parent's own
        // metadata directory inside the project.
        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        let runtime = AgentRuntime::new(
            AgentId::new(),
            agent_config,
            LlmClient::new(config).unwrap(),
        )
        .expect("runtime")
        .with_extra_fs_read_root(agents_dir.clone())
        .with_project_root(worktree_dir.clone())
        .with_workspace(AgentWorkspace::new(&agents_dir, "parent"));

        // Resolve the absolute path to the sibling's personality file.
        // We pass an absolute path because, from inside the worktree,
        // `.alms/agents/...` would resolve relative to the worktree
        // and miss the sibling tree entirely. The extra-read-roots
        // shim accepts the absolute path as long as it's inside one
        // of the registered extras.
        let abs = std::fs::canonicalize(&sibling_personality)
            .expect("canonicalize sibling personality path");

        let result = runtime
            .tools()
            .execute(
                "fs_read",
                serde_json::json!({ "path": abs.to_string_lossy() }),
            )
            .await;

        assert!(
            result.is_ok(),
            "parent in worktree mode must be able to read sibling \
             personality via extra_fs_read_roots: {:?}",
            result.err(),
        );
        let value = result.unwrap();
        assert!(
            value["content"]
                .as_str()
                .unwrap_or("")
                .contains("I am the sibling"),
            "expected sibling personality contents, got {value}"
        );
    }

    /// Defensive: a parent in worktree mode WITHOUT the extra read
    /// root cannot reach the sibling personality file. This pins
    /// the load-bearing nature of the `with_extra_fs_read_root`
    /// call in `runs/lifecycle.rs::execute_run` — drop the call and
    /// the test passes (read fails) where it should fail (read
    /// succeeds).
    #[tokio::test]
    async fn test_worktree_mode_sibling_reads_blocked_without_extras() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let project_root = dir.path().to_path_buf();
        let agents_dir = project_root.join(".alms").join("agents");
        let worktree_dir = project_root.join(".alms").join("worktrees").join("parent");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::create_dir_all(&worktree_dir).unwrap();

        let sibling_meta_dir = agents_dir.join("sibling");
        std::fs::create_dir_all(&sibling_meta_dir).unwrap();
        let sibling_personality = sibling_meta_dir.join("personality.md");
        std::fs::write(&sibling_personality, "I am the sibling.\n").unwrap();

        let config = LlmConfig {
            mock: true,
            ..LlmConfig::default()
        };
        let agent_config = AgentConfig {
            sandbox_root: "".into(),
            ..AgentConfig::default()
        };
        // No `with_extra_fs_read_root` call — only `with_project_root`
        // pinning the sandbox at the worktree dir.
        let runtime = AgentRuntime::new(
            AgentId::new(),
            agent_config,
            LlmClient::new(config).unwrap(),
        )
        .expect("runtime")
        .with_project_root(worktree_dir.clone());

        let abs = std::fs::canonicalize(&sibling_personality)
            .expect("canonicalize sibling personality path");

        let result = runtime
            .tools()
            .execute(
                "fs_read",
                serde_json::json!({ "path": abs.to_string_lossy() }),
            )
            .await;

        assert!(
            result.is_err() || result.as_ref().ok().and_then(|v| v.get("error")).is_some(),
            "parent without extra_fs_read_roots must NOT be able to \
             read outside the worktree — got Ok({:?})",
            result.ok(),
        );
    }
}

// ──────────────────────────────────────────────────────────────────────
// #1003 — Restored Debug mode: ContextDebug emit attribution
//
// Previously, the runtime emitted `ContextDebug` events only with the
// payload (messages, tool_names, token counts). #1003 adds `agent_id`
// and `agent_name` so the UI can attribute each snapshot to the
// agent whose perspective produced it. This matters most for DM
// sessions where two agents alternate turns on the same session and
// each emits independent per-perspective context windows — without
// attribution, back-to-back panels are indistinguishable.
//
// The plumbing splits into two slices:
//
//   1. `AgentRuntime::emit_context_debug` reads `self.agent_id` /
//      `self.agent_name` and stuffs them into the event. This is
//      pure plumbing and the tests below pin both fields.
//
//   2. The agent loop's `if self.config.debug_mode` gate decides
//      whether to call emit at all. Per-agent `debug_mode` is layered
//      onto the resolved `AgentConfig` in the gateway's
//      `resolve_agent_config`; subagents see the parent's flag
//      through inheritance and the coordinator filters their emits
//      from the parent's SSE stream. The gateway-side merge is tested
//      in `crates/alms-gateway/src/agents.rs::tests` and
//      `crates/alms-session/src/sqlite/agents.rs::tests`; the runtime
//      side is what these tests cover.
//
// DM-perspective coverage: a DM session triggers `run_on_session`,
// which calls `build_context()` (with DM perspective mapping —
// `crates/alms-runtime/src/context/perspective.rs`) and reaches
// `finish_run`, which is where the `if self.config.debug_mode`
// emit gate fires. The same `emit_context_debug` runs for both the
// `run()` (webchat) and `run_on_session()` (DM) entry points, so
// pinning the attribution shape here covers both surfaces by
// construction. The gateway-level golden test in
// `crates/alms-gateway/tests/sse_golden_tests.rs` pins the SSE wire
// shape that the UI consumes.

fn build_test_runtime_for_emit(
    agent_id: AgentId,
    agent_name: Option<String>,
    event_sender: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
) -> AgentRuntime {
    let config = LlmConfig {
        mock: true,
        ..LlmConfig::default()
    };
    AgentRuntime {
        agent_id,
        config: AgentConfig {
            // debug_mode is irrelevant for the helper-level tests
            // below — they call `emit_context_debug` directly. The
            // run-loop-level gate is exercised at the gateway layer.
            debug_mode: true,
            ..AgentConfig::default()
        },
        llm: LlmClient::new(config).unwrap(),
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: Some(event_sender),
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name,
        dm_implicit_reply: false,
    }
}

#[tokio::test]
async fn emit_context_debug_carries_agent_id_and_name() {
    // Pin the wire-shape contract: `agent_id` is always populated
    // from `self.agent_id`, `agent_name` is populated from
    // `self.agent_name` (which is `Some` for named agents from the
    // registry, `None` for legacy unnamed runtimes).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let agent_id = AgentId::new();
    let runtime = build_test_runtime_for_emit(agent_id, Some("alpha".to_string()), tx);

    let messages = vec![
        LlmMessage::system("You are alpha."),
        LlmMessage::user("ping"),
    ];
    runtime.emit_context_debug(&messages);

    let event = rx
        .recv()
        .await
        .expect("emit must send a ContextDebug event");
    match event {
        RuntimeEvent::ContextDebug {
            agent_id: emitted_id,
            agent_name: emitted_name,
            history_message_count,
            ..
        } => {
            assert_eq!(
                emitted_id,
                agent_id.0.to_string(),
                "emitted agent_id must match self.agent_id"
            );
            assert_eq!(
                emitted_name.as_deref(),
                Some("alpha"),
                "emitted agent_name must match self.agent_name"
            );
            // `history_message_count` is `total - 1 (system) - 1 (last
            // user)` = 0 for this 2-message snapshot. Sanity check.
            assert_eq!(history_message_count, 0);
        }
        _ => panic!("Expected ContextDebug, got a different RuntimeEvent variant"),
    }
}

#[tokio::test]
async fn emit_context_debug_serializes_agent_name_as_null_for_unnamed_runtime() {
    // Legacy unnamed runtimes (no per-agent record, e.g. in-memory
    // tests or pre-registry deployments) carry `agent_name = None`.
    // The wire shape must serialise the field as `null` so the UI
    // can fall back to a placeholder label rather than treat the
    // missing field as an error. This pins both the `RuntimeEvent`
    // structure and the downstream JSON shape.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let agent_id = AgentId::new();
    let runtime = build_test_runtime_for_emit(agent_id, None, tx);

    runtime.emit_context_debug(&[LlmMessage::user("hi")]);

    let event = rx
        .recv()
        .await
        .expect("emit must send a ContextDebug event");
    match event {
        RuntimeEvent::ContextDebug {
            agent_id: emitted_id,
            agent_name: emitted_name,
            ..
        } => {
            assert_eq!(emitted_id, agent_id.0.to_string());
            assert!(
                emitted_name.is_none(),
                "unnamed runtime must emit agent_name = None"
            );
        }
        _ => panic!("Expected ContextDebug, got a different RuntimeEvent variant"),
    }
}

#[tokio::test]
async fn emit_context_debug_attributes_dm_perspective_to_each_agent() {
    // DM-perspective scenario: two agents (alpha + beta) alternate
    // turns on a shared DM session. Each agent's runtime fires
    // `emit_context_debug` during its own turn, populating
    // `agent_id` / `agent_name` from its own runtime fields.
    // The UI uses the pair to label per-turn panels so back-to-back
    // emits for the same DM session are visually unambiguous.
    //
    // Pinning here means a future refactor that accidentally drops
    // attribution from DM emits (e.g. by sharing a single emit
    // helper across runtimes) regresses this test instead of
    // silently breaking the UI's DM Debug panel.
    let alpha_id = AgentId::new();
    let beta_id = AgentId::new();

    let (tx_alpha, mut rx_alpha) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let (tx_beta, mut rx_beta) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    let alpha = build_test_runtime_for_emit(alpha_id, Some("alpha".to_string()), tx_alpha);
    let beta = build_test_runtime_for_emit(beta_id, Some("beta".to_string()), tx_beta);

    // Alpha's turn — perspective-mapped messages (alpha sees its
    // own outgoing as assistant; the DM mapper does the role
    // translation, but the emit contract doesn't care about the
    // payload here, only the attribution).
    alpha.emit_context_debug(&[
        LlmMessage::system("You are alpha. The peer 'beta' is in this DM."),
        LlmMessage::assistant("hi beta"),
        LlmMessage::user("hello alpha"),
    ]);

    // Beta's turn — its perspective sees the same conversation
    // mirrored, but emits attributed to beta.
    beta.emit_context_debug(&[
        LlmMessage::system("You are beta. The peer 'alpha' is in this DM."),
        LlmMessage::user("hi beta"),
        LlmMessage::assistant("hello alpha"),
    ]);

    let alpha_event = rx_alpha
        .recv()
        .await
        .expect("alpha must emit a ContextDebug event");
    let beta_event = rx_beta
        .recv()
        .await
        .expect("beta must emit a ContextDebug event");

    let (alpha_emitted_id, alpha_emitted_name) = match alpha_event {
        RuntimeEvent::ContextDebug {
            agent_id,
            agent_name,
            ..
        } => (agent_id, agent_name),
        _ => panic!("alpha: expected ContextDebug, got a different RuntimeEvent variant"),
    };
    let (beta_emitted_id, beta_emitted_name) = match beta_event {
        RuntimeEvent::ContextDebug {
            agent_id,
            agent_name,
            ..
        } => (agent_id, agent_name),
        _ => panic!("beta: expected ContextDebug, got a different RuntimeEvent variant"),
    };

    // Each emit lands on its own runtime's channel — alpha never
    // appears on beta's stream and vice versa.
    assert_eq!(alpha_emitted_id, alpha_id.0.to_string());
    assert_eq!(alpha_emitted_name.as_deref(), Some("alpha"));
    assert_eq!(beta_emitted_id, beta_id.0.to_string());
    assert_eq!(beta_emitted_name.as_deref(), Some("beta"));

    // Cross-talk check: the UI relies on the IDs being distinct so
    // it can correctly group panels per perspective. If a future
    // refactor accidentally inverts the attribution (e.g. emits the
    // peer's name instead of self's), this assertion catches it.
    assert_ne!(
        alpha_emitted_id, beta_emitted_id,
        "DM perspective emits must carry distinct agent IDs per turn"
    );
}

// ──────────────────────────────────────────────────────────────────────
// #1154 design default #3 — loop-level DM empty-reply nudge (Tim's S2
// on PR #1156). The gate-side cases (empty reply → Errored exit etc.)
// are covered in the gateway's integration tests; this exercises the
// RUNTIME side: a scripted two-turn LLM where the first turn yields no
// deliverable text, asserting the loop (a) emits the
// `DM_EMPTY_REPLY_RETRY` warning, (b) injects the nudge message into
// the next LLM request, and (c) returns the second turn's text as a
// deliverable output.

/// Read one full HTTP request (headers + Content-Length body) from a
/// socket so the test can assert on the JSON payload the agent loop
/// actually sent to the LLM on each turn.
async fn read_full_http_request(sock: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = match sock.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text[..header_end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn dm_empty_reply_nudge_retries_once_then_delivers_second_turn_text() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const REPLY_TEXT: &str = "Hello alice, here is a real reply.";

    // Scripted two-turn LLM: turn 1 streams no content (empty delta +
    // stop), turn 2 streams a real text reply. OpenAI-style SSE bodies
    // because the default provider (`openrouter`) parses the OpenAI
    // wire shape.
    let turn1_body = concat!(
        "data: {\"id\":\"t1\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let turn2_body = format!(
        concat!(
            "data: {{\"id\":\"t2\",\"object\":\"chat.completion.chunk\",\"created\":2,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        REPLY_TEXT
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Capture the request bodies so the nudge-injection assertion can
    // look at what the loop actually sent on the second turn.
    let captured: std::sync::Arc<tokio::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured_writer = captured.clone();
    tokio::spawn(async move {
        for body in [turn1_body.to_string(), turn2_body] {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let req = read_full_http_request(&mut sock).await;
            captured_writer.lock().await.push(req);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        ..AgentConfig::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply()
    .with_event_sender(tx);

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // (c) The loop must complete with the SECOND turn's text — i.e. it
    // retried after the empty first turn — and that text must be a
    // deliverable DM reply for the gateway's completion gate.
    assert!(tool_calls.is_empty(), "no tool calls were scripted");
    let output = result.expect("agent_loop must succeed after the nudge retry");
    assert_eq!(output.response, REPLY_TEXT);
    assert!(
        alms_core::deliverable_dm_reply(&output.response, output.reasoning.as_deref()).is_some(),
        "the second-turn text must classify as deliverable"
    );

    // (a) Exactly one DM_EMPTY_REPLY_RETRY warning, and no terminal
    // DM_EMPTY_REPLY (the retry succeeded).
    let mut retry_warnings = 0;
    let mut exhausted_warnings = 0;
    while let Ok(event) = rx.try_recv() {
        if let RuntimeEvent::Warning { code, .. } = event {
            match code.as_str() {
                "DM_EMPTY_REPLY_RETRY" => retry_warnings += 1,
                "DM_EMPTY_REPLY" => exhausted_warnings += 1,
                _ => {}
            }
        }
    }
    assert_eq!(
        retry_warnings, 1,
        "exactly one DM_EMPTY_REPLY_RETRY warning must be emitted"
    );
    assert_eq!(
        exhausted_warnings, 0,
        "the nudge succeeded, so no terminal DM_EMPTY_REPLY warning"
    );

    // (b) The nudge message was injected into the second LLM request.
    let requests = captured.lock().await;
    assert_eq!(requests.len(), 2, "the loop must have made two LLM calls");
    assert!(
        !requests[0].contains("ERROR: Your run produced no reply text"),
        "first request must NOT contain the nudge"
    );
    assert!(
        requests[1].contains("ERROR: Your run produced no reply text"),
        "second request must carry the DM_EMPTY_REPLY_RETRY nudge message; got: {}",
        requests[1]
    );
}

/// B3 (#987 / #1154): an agent that keeps calling tools without ever
/// producing a final reply must be terminated by the iteration cap instead
/// of looping forever. The loop makes exactly `max_iterations` LLM calls and
/// then returns an `Err` whose message identifies the iteration limit. On a
/// DM run that `Err` is what `finish_run` wraps into `FailedWithToolCalls`,
/// which the gateway's `handle_dm_run_failure` converts into an `Errored`
/// conversation end so the peer is notified rather than stranded.
#[tokio::test]
async fn agent_loop_iteration_cap_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // A streaming SSE chunk that always requests one `echo` tool call and
    // never produces final text — so the loop can never terminate on its
    // own and must hit the iteration cap.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Serve the tool-call response on every accepted connection until the
    // test drops the listener. Count how many LLM calls the loop actually
    // made so we can assert it stopped at the cap.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                tool_call_body.len(),
                tool_call_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    const MAX_ITERS: u32 = 3;
    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: MAX_ITERS,
        // Disable the duration cap so this test isolates the iteration cap.
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // The loop must terminate with an error (NOT loop forever, NOT Cancelled
    // — a non-Cancelled Err is what the gateway routes through
    // `handle_dm_run_failure` to notify the DM peer). `AgentLoopOutput` is
    // not `Debug`, so match rather than `expect_err`.
    let err = match result {
        Ok(_) => panic!("iteration cap must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "the cap trip must be a failure, not a cancellation; got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("iteration") && msg.contains(&MAX_ITERS.to_string()),
        "error must identify the iteration cap; got: {msg}"
    );

    // Exactly `max_iterations` LLM calls were made — the cap fires before the
    // (would-be) 4th call.
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        MAX_ITERS as usize,
        "the loop must make exactly max_iterations LLM calls before tripping the cap"
    );

    // The sanitiser surfaces a clear, distinct label for session history /
    // the peer notification (not the generic "Runtime error").
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after reaching its iteration limit"
    );
}

/// #1162 / #1163 + Codex P2: a streaming per-chunk **stall** must FALL THROUGH
/// to the buffered `complete()` fallback and can recover there. The buffered
/// path is non-streaming, so its first-byte wait (`req.send()`) is bounded by
/// the full `timeout_secs`, not the per-chunk guard — a slow-*generating* model
/// whose stream went quiet still succeeds on the re-issue. The mock stalls the
/// first (streaming) call past the 1s per-chunk window, then returns a complete
/// non-streaming JSON on the second (buffered) call; the run must succeed with
/// that content, having made exactly TWO upstream calls.
#[tokio::test]
async fn agent_loop_stalled_stream_falls_back_to_buffered_and_recovers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // A valid SSE prefix with no event terminator: the parser stays in "need
    // more data", and because the server then holds the connection open the
    // per-chunk idle guard fires (-> "stream stalled", not "operation timed out").
    let partial = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\
                   \"choices\":[{\"delta\":{\"content\":\"hel";
    // The buffered (non-streaming) retry's full response.
    let buffered_body = r#"{"id":"cmpl-1","object":"chat.completion","created":1,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"recovered via buffered"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            // `fetch_add` returns the prior value: 0 = streaming, 1 = buffered.
            let n = call_count_writer.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Streaming attempt: headers + partial, then stall past the 1s
                // per-chunk window so the idle guard faults it ("stream stalled").
                let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                               Content-Length: 8192\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.write_all(partial.as_bytes()).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                drop(sock);
            } else {
                // Buffered `complete()`: one full, valid non-streaming JSON.
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    buffered_body.len(),
                    buffered_body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        // Per-chunk window (1s) below the total deadline (30s): the streaming
        // idle guard faults the stall; the buffered retry completes well within.
        timeout_secs: 30,
        stream_chunk_timeout_secs: 1,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: 1000,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // The stall must NOT short-circuit: the buffered retry recovers the run.
    let output = match result {
        Ok(o) => o,
        Err(e) => panic!("a recoverable stall must succeed via the buffered fallback, got: {e}"),
    };
    assert!(
        output.response.contains("recovered via buffered"),
        "run must return the buffered retry's content; got: {:?}",
        output.response
    );
    // Defining assertion: TWO upstream calls — the stalled stream, then the
    // buffered retry that succeeded (proving the recovery the narrowing exists
    // for; pre-narrowing the stall short-circuited at one call).
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "a stall must fall through to the buffered retry (stream + buffered = 2)"
    );
}

/// #1162 / #1163: a streaming **total** timeout (`operation timed out` — the
/// whole call blew `timeout_secs`) IS genuinely futile to re-issue, so it must
/// short-circuit the buffered fallback — exactly ONE upstream call, with the
/// timeout diagnostic surfaced. (Preserves the original minimax-fix coverage,
/// now scoped to the total-timeout class per Codex P2.) The mock holds the
/// stream open while the per-chunk window stays generous, so the reqwest total
/// `.timeout()` is what faults the read.
#[tokio::test]
async fn agent_loop_total_timeout_stream_skips_futile_buffered_fallback() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let partial = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\
                   \"choices\":[{\"delta\":{\"content\":\"hel";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, Ordering::SeqCst);
            // Headers + partial, then hold open past the total deadline.
            let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                           Content-Length: 8192\r\nConnection: close\r\n\r\n";
            let _ = sock.write_all(headers.as_bytes()).await;
            let _ = sock.write_all(partial.as_bytes()).await;
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            drop(sock);
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        // Total deadline (2s) BELOW the per-chunk window (30s): the reqwest
        // total `.timeout()` faults the read first -> "operation timed out".
        timeout_secs: 2,
        stream_chunk_timeout_secs: 30,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        max_iterations: 1000,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let start = std::time::Instant::now();
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;
    let elapsed = start.elapsed();

    let err = match result {
        Ok(_) => panic!("a total-timeout stream must terminate the run with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "a total timeout is a failure, not a cancellation; got {err:?}"
    );
    // The surfaced error is the timeout diagnostic, carrying the model.
    let msg = err.to_string();
    assert!(
        msg.contains("operation timed out"),
        "expected the total-timeout diagnostic to surface; got: {msg}"
    );
    assert!(
        msg.contains("model=test-model"),
        "the surfaced error must carry the model for attribution; got: {msg}"
    );
    // Defining assertion: exactly ONE upstream call — the buffered fallback is
    // futile for a total timeout and must be skipped.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "a total-timeout streaming failure must NOT trigger the buffered fallback"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "total-timeout run should fail fast (one call); took {elapsed:?}"
    );
}

/// B3 (#987 / #1154): the wall-clock duration cap also terminates a wedged
/// loop, independently of the iteration cap. A mock that sleeps ~1.2s per
/// turn against a 1-second run-duration budget trips the between-iteration
/// elapsed check on the second iteration; the loop returns an error
/// identifying the time limit.
#[tokio::test]
async fn agent_loop_duration_cap_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    // Always requests one `echo` tool call, but sleeps ~1.2s before
    // responding so a 1-second duration budget is exceeded.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                tool_call_body.len(),
                tool_call_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Iteration cap high so it cannot be the cause; the 1-second duration
        // budget is what trips (after the first ~1.2s tool round-trip).
        max_iterations: 1000,
        max_run_duration_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("duration cap must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(!matches!(err, alms_core::AlmsError::Cancelled));
    let msg = err.to_string();
    assert!(
        msg.contains("duration"),
        "error must identify the duration cap; got: {msg}"
    );
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after reaching its time limit"
    );
}

/// B3 (#987 / #1160): `0` is the documented escape hatch — `max_iterations =
/// 0` and `max_run_duration_secs = 0` mean "no cap", NOT "trip immediately".
/// Both guards in the loop are `value > 0 && …`, so a `0` short-circuits the
/// check; this test locks that semantics in so a future refactor that drops
/// the `> 0` gate (turning `0` into an instant trip on iteration 1) is caught.
///
/// The mock serves three `echo` tool-call turns and then a final text reply,
/// so the loop makes four LLM calls. If either cap treated `0` as a trip, the
/// loop would error on the first iteration instead of completing; asserting it
/// runs to the final text proves `0` disabled both caps.
#[tokio::test]
async fn agent_loop_zero_caps_disable_both_limits() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const REPLY_TEXT: &str = "Done after three tool turns.";

    // A streaming SSE chunk that requests one `echo` tool call (no final
    // text), used for the first three turns.
    let tool_call_body = concat!(
        "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
        "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
        "\"tool_calls\":[{\"index\":0,\"id\":\"call_loop\",\"type\":\"function\",",
        "\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"message\\\":\\\"hi\\\"}\"}}]},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    // Final turn: real text reply, no tool call — the loop terminates here.
    let final_body = format!(
        concat!(
            "data: {{\"id\":\"tf\",\"object\":\"chat.completion.chunk\",\"created\":4,",
            "\"model\":\"test-model\",\"choices\":[{{\"index\":0,",
            "\"delta\":{{\"role\":\"assistant\",\"content\":\"{}\"}},",
            "\"finish_reason\":\"stop\"}}]}}\n\n",
            "data: [DONE]\n\n"
        ),
        REPLY_TEXT
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        let bodies = [
            tool_call_body.clone(),
            tool_call_body.clone(),
            tool_call_body,
            final_body,
        ];
        for body in bodies {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Both caps disabled — must NOT trip on iteration 1.
        max_iterations: 0,
        max_run_duration_secs: 0,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    // With both caps at 0 the loop ran all four turns to the final text
    // instead of erroring on iteration 1 — `0` is "no cap", not "trip now".
    let output = result.expect("zero caps must disable both limits; the loop must complete");
    assert_eq!(output.response, REPLY_TEXT);
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "the loop must make all four LLM calls — three tool turns plus the final text"
    );
}

/// Test-only tool that sleeps a fixed duration and emits no progress signal
/// while it runs. Used by the #1150 phase-aware inactivity-timer integration
/// tests to drive the `ExecutingTools` (P3) phase either past the
/// `tool_phase_ceiling_secs` backstop (a stall trip) or comfortably under it
/// (a productive run that survives). It reports `is_auto_approved() == true`
/// so it runs under the default Guarded posture with no approval round-trip —
/// exactly like the builtin `echo` the sibling B3 cap tests drive.
#[derive(Debug)]
struct SleepTool {
    ms: u64,
}

#[async_trait::async_trait]
impl alms_sandbox::Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep_tool"
    }
    fn description(&self) -> &str {
        "sleeps for a fixed duration (test only)"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> alms_sandbox::error::SandboxResult<serde_json::Value> {
        tokio::time::sleep(std::time::Duration::from_millis(self.ms)).await;
        Ok(serde_json::json!({"ok": true}))
    }
    fn is_auto_approved(&self) -> bool {
        true
    }
}

/// A streaming SSE body that requests exactly one `sleep_tool` call and no
/// final text, so the loop keeps iterating (the model never "finishes") and
/// the phase-aware timer is what must stop it.
const SLEEP_TOOL_SSE_BODY: &str = concat!(
    "data: {\"id\":\"t\",\"object\":\"chat.completion.chunk\",\"created\":1,",
    "\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
    "\"tool_calls\":[{\"index\":0,\"id\":\"call_sleep\",\"type\":\"function\",",
    "\"function\":{\"name\":\"sleep_tool\",\"arguments\":\"{}\"}}]},",
    "\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n"
);

/// #1150 (P3 ceiling): the phase-aware inactivity timer terminates a run whose
/// tool batch overruns the `tool_phase_ceiling_secs` backstop. A `sleep_tool`
/// that runs ~1.3s — producing no token / reasoning / tool-start signal while
/// it sleeps — against a 1-second ceiling leaves the run idle past the budget,
/// so the next top-of-loop checkpoint, now in the `ExecutingTools` phase, trips
/// with the distinct "stalled" error. The iteration / wall-clock / P1 caps are
/// all disabled so this isolates P3. (No watchdog: the slow tool finishes
/// first; the run is terminated at the following checkpoint, not mid-tool.)
#[tokio::test]
async fn agent_loop_inactivity_tool_ceiling_terminates_with_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    // Serve the sleep-tool response on every accepted connection. Count the
    // calls so we can assert the ceiling tripped right after the first batch.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                SLEEP_TOOL_SSE_BODY.len(),
                SLEEP_TOOL_SSE_BODY
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Isolate the P3 ceiling: iteration / wall-clock / P1 all disabled.
        max_iterations: 1000,
        max_run_duration_secs: 0,
        between_iterations_secs: 0,
        tool_phase_ceiling_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();
    // The slow tool sleeps past the 1-second ceiling.
    runtime
        .tools
        .register(std::sync::Arc::new(SleepTool { ms: 1300 }));

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("the tool-phase ceiling must terminate the loop with an error"),
        Err(e) => e,
    };
    assert!(
        !matches!(err, alms_core::AlmsError::Cancelled),
        "the ceiling trip must be a failure, not a cancellation; got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("stalled") && msg.contains("executing tools"),
        "error must identify the stalled tool phase; got: {msg}"
    );

    // The ceiling tripped at the FIRST checkpoint after the slow batch, so the
    // loop made exactly one LLM call — it never reached a second turn.
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the loop must trip on the ceiling after exactly one tool round-trip"
    );

    // The sanitiser surfaces the distinct stalled label (not the iteration or
    // time-limit labels, and not the generic "Runtime error").
    assert_eq!(
        alms_core::sanitize_error_for_session(&alms_core::AlmsError::Runtime(msg)),
        "Agent stopped after stalling (no activity)"
    );
}

/// #1150 (productive run survives): the inactivity timer must NOT clip a run
/// that keeps making progress, even when the run's total wall-clock far
/// exceeds the (small) inactivity budgets. A `sleep_tool` batch of ~400ms per
/// turn stays comfortably under the 1-second P3 ceiling, and every turn
/// produces activity (the tool-call response + the tool start), so no
/// checkpoint ever trips the stall guard. Across four turns the run spans
/// ~1.6s — well past the 1-second budgets — yet it terminates on the
/// *iteration* cap, never on a stall. This is the regression guard for the
/// whole point of #1150: replacing the flat wall-clock cap with a
/// progress-aware one so long-but-productive runs are not killed.
#[tokio::test]
async fn agent_loop_inactivity_productive_run_survives() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());

    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call_count_writer = call_count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let _ = read_full_http_request(&mut sock).await;
            call_count_writer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                SLEEP_TOOL_SSE_BODY.len(),
                SLEEP_TOOL_SSE_BODY
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    const MAX_ITERS: u32 = 4;
    let llm_config = LlmConfig {
        base_url,
        api_key: "test-key".to_string(),
        default_model: "test-model".to_string(),
        timeout_secs: 5,
        stream_chunk_timeout_secs: 5,
        ..LlmConfig::default()
    };
    let agent_config = AgentConfig {
        sandbox_root: "".into(),
        // Small inactivity budgets (1s each) and the wall-clock backstop
        // disabled, so ONLY the iteration cap can stop a productive run. If the
        // inactivity timer were progress-blind it would trip long before the
        // 4th turn; that it does not is the property under test.
        max_iterations: MAX_ITERS,
        max_run_duration_secs: 0,
        between_iterations_secs: 1,
        tool_phase_ceiling_secs: 1,
        ..AgentConfig::default()
    };
    let runtime = AgentRuntime::new(
        AgentId::new(),
        agent_config,
        LlmClient::new(llm_config).unwrap(),
    )
    .unwrap()
    .with_agent_name("bob".to_string())
    .with_dm_implicit_reply();
    // ~400ms per batch: under the 1-second ceiling, but four of them span
    // ~1.6s — comfortably past the 1-second budgets.
    runtime
        .tools
        .register(std::sync::Arc::new(SleepTool { ms: 400 }));

    let session_manager = SessionManager::new(SessionConfig::default());
    let dm_context = "dm:alice:bob";
    let session_id = alms_core::SessionId::deterministic_dm("alice", "bob");
    let _session = session_manager.get_or_create_shared(session_id, dm_context);

    let messages = vec![LlmMessage::system("You are bob."), LlmMessage::user("ping")];
    let (_tool_calls, result) = runtime
        .agent_loop(
            &session_manager,
            session_id,
            messages,
            /* is_dm */ true,
            /* include_user */ false,
            /* dm_peer */ Some("alice"),
        )
        .await;

    let err = match result {
        Ok(_) => panic!("the loop must terminate on the iteration cap, not run forever"),
        Err(e) => e,
    };
    let msg = err.to_string();
    // It survived every inactivity checkpoint and stopped on the iteration cap.
    assert!(
        msg.contains("iteration") && msg.contains(&MAX_ITERS.to_string()),
        "a productive run must terminate on the iteration cap, not a stall; got: {msg}"
    );
    assert!(
        !msg.contains("stalled"),
        "the inactivity timer must NOT clip a productive run; got: {msg}"
    );
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        MAX_ITERS as usize,
        "all four productive turns must run before the iteration cap trips"
    );
}

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
        config: AgentConfig::default(),
        llm: LlmClient::new(LlmConfig::default()).unwrap(),
        summary_llm: None,
        tools: ToolRegistry::new(),
        workspace: None,
        event_sender: None,
        run_id: None,
        cancel_token: None,
        resolved_sandbox_root: None,
        shell_unrestricted: true,
        shell_default_env: std::collections::HashMap::new(),
        shell_permissions: alms_core::config::ShellPermissions::default(),
        shell_classification_mode: alms_core::config::ShellClassificationMode::default(),
        shell_spill_policy: alms_sandbox::shell::spill::ShellSpillPolicy::disabled(),
        tool_output_truncate_policy:
            crate::tool_output_truncate::ToolOutputTruncatePolicy::disabled(),
        extra_fs_read_roots: Vec::new(),
        agent_name: Some("reviewer".to_string()),
        dm_implicit_reply: false,
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

// ── #1310: the shown-view guard, wired into a run ──────────────────────────

/// Build a mock runtime with `workspace` attached, the way the gateway does.
fn runtime_with_workspace(workspace: &crate::workspace::AgentWorkspace) -> AgentRuntime {
    AgentRuntime::new(
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
    .expect("runtime")
    .with_workspace(workspace.clone())
}

/// The tool-loop rebuild re-reads the workspace, so an agent that appends in
/// one tool batch and replaces in the next is judged against what it can
/// actually see.
///
/// This is a correction to the premise #1305 and #1310 were both written on
/// — "`build_context` runs once per run, so the memories snapshot in the
/// system prompt is fixed at run start". It is fixed only until the first
/// tool call: `agent_loop` calls `rebuild_system_prompt_for_tool_loop` after
/// every batch, which goes through `assemble_system_prompt` ->
/// `build_system_prompt_prefix` and reads the files again, replacing
/// `messages[0]` outright so the stale copy is not even in the array any
/// more.
///
/// It matters here because it decides how far the guard should reach. The
/// window between the rebuild and the next replacement is a real one — a
/// single tool batch containing both an append and a `mode: "write"` never
/// sees a rebuild in between, and neither does a concurrent writer — but a
/// guard that refused *every* mid-run replacement would refuse this one too,
/// where the agent is looking straight at the file. Both halves are asserted:
/// refused before the rebuild, written after, and the rebuilt prompt actually
/// containing the appended entry, which is what makes the second half
/// legitimate rather than merely permissive.
#[tokio::test]
async fn the_tool_loop_rebuild_re_shows_the_workspace_and_unblocks_the_next_replacement() {
    use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};

    let dir = tempfile::tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    workspace
        .write_file_as_operator(WorkspaceFile::Memories, "- M1\n- M2")
        .unwrap();
    let runtime = runtime_with_workspace(&workspace);

    // The run's first system prompt, assembled before the agent gets a turn.
    let initial = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);
    assert!(
        initial.contains("- M2"),
        "precondition: memories are injected"
    );
    assert!(
        !initial.contains("- M3"),
        "precondition: M3 does not exist yet"
    );

    // The agent appends inside the run.
    workspace
        .append_file(WorkspaceFile::Memories, "- M3")
        .unwrap();

    assert_eq!(
        workspace
            .write_file_checked(WorkspaceFile::Memories, "- M1\n- M2, merged")
            .unwrap(),
        CheckedWrite::Refused(RefusedWrite::ChangedSinceShown),
        "before any rebuild, the agent's only copy of the file predates its own append"
    );

    // The rebuild `agent_loop` performs after a tool batch.
    let mut messages = vec![LlmMessage::system(initial)];
    runtime.rebuild_system_prompt_for_tool_loop(&mut messages, true, None);
    let rebuilt = messages[0].content.as_deref().unwrap();
    assert!(
        rebuilt.contains("- M3"),
        "the rebuild re-reads memories.md, so the appended entry is back in the prompt"
    );

    assert_eq!(
        workspace
            .write_file_checked(WorkspaceFile::Memories, "- M1\n- M2\n- M3, merged")
            .unwrap(),
        CheckedWrite::Written,
        "after the rebuild the agent has been re-shown the file, so replacing it is an \
         informed choice and must not be refused"
    );
}

/// `build_context` starts each run with no record of what the agent has been
/// shown, and the prompt assembly immediately refills it.
///
/// The two rows are the two ways to get this wrong. Without the reset, a
/// user-facing run's view of `user.md` outlives its context and authorises a
/// blind replacement in the DM run that follows — the file is injected only
/// for user-facing contexts, so the second run's agent has no copy of it.
/// With the reset in the wrong place (after the prompt assembly rather than
/// before), every replacement in every run is refused instead.
#[tokio::test]
async fn build_context_scopes_the_shown_record_to_one_run() {
    use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};

    for (label, second_context, expected) in [
        (
            "a second user-facing run re-shows user.md",
            "webchat-2",
            CheckedWrite::Written,
        ),
        (
            "a DM run does not, and cannot inherit the first run's view",
            "dm:alice:bob",
            CheckedWrite::Refused(RefusedWrite::NeverShown),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = AgentWorkspace::new(dir.path(), "alice");
        workspace
            .write_file_as_operator(WorkspaceFile::User, "Name: Alper")
            .unwrap();
        let runtime = runtime_with_workspace(&workspace);
        let session_manager = SessionManager::new(SessionConfig::default());

        for context_id in ["webchat-1", second_context] {
            let session = session_manager.get_or_create(runtime.agent_id, context_id);
            runtime
                .build_context(&session_manager, &session.id, context_id, "hi")
                .await
                .unwrap();
        }

        assert_eq!(
            workspace
                .write_file_checked(WorkspaceFile::User, "Name: Someone Else")
                .unwrap(),
            expected,
            "{label}"
        );
    }
}

/// The two workspace tools are registered from one workspace, so a
/// `workspace_read` actually unblocks the `workspace_write` that was refused.
///
/// Asserted through `runtime.tools()` rather than on the tool structs,
/// because the thing that can break is the wiring: `with_workspace` cloning
/// two independent `AgentWorkspace` values instead of one would leave each
/// tool with its own record, and the sequence below would refuse twice.
#[tokio::test]
async fn the_registered_workspace_tools_share_one_view_of_the_files() {
    use crate::workspace::{AgentWorkspace, MEMORIES_INJECTION_CAP, WorkspaceFile};

    let dir = tempfile::tempdir().unwrap();
    let workspace = AgentWorkspace::new(dir.path(), "alice");
    let mut memories = String::new();
    let mut n = 0;
    while memories.len() < MEMORIES_INJECTION_CAP + 500 {
        memories.push_str(&format!("- entry {n}\n"));
        n += 1;
    }
    workspace
        .write_file_as_operator(WorkspaceFile::Memories, &memories)
        .unwrap();
    let runtime = runtime_with_workspace(&workspace);
    let _ = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);

    let write_params = serde_json::json!({
        "file": "memories",
        "content": "- everything, compacted",
        "mode": "write",
    });

    let refused = runtime
        .tools()
        .execute("workspace_write", write_params.clone())
        .await
        .expect("a refusal is an in-band result, not a tool error");
    assert_eq!(refused["refused"], "shown_partially", "{refused}");

    let read = runtime
        .tools()
        .execute("workspace_read", serde_json::json!({"file": "memories"}))
        .await
        .expect("workspace_read must be registered alongside workspace_write");
    assert_eq!(read["content"].as_str().unwrap(), memories);

    // The rebuild `agent_loop` performs after every tool batch, and the step
    // that makes this sequence production-shaped rather than merely
    // tool-shaped. The read and the write CANNOT be one batch -- the model
    // has to see the read result to compose the replacement -- so exactly one
    // rebuild always lands between them. Without it this test passed while
    // the recovery it describes was unreachable in production (Tim, #1314).
    let _ = runtime.assemble_system_prompt(&runtime.config.system_prompt, true);

    let written = runtime
        .tools()
        .execute("workspace_write", write_params)
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    assert_eq!(
        workspace.read_file(WorkspaceFile::Memories).unwrap(),
        "- everything, compacted"
    );
}
