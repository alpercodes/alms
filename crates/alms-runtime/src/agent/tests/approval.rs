// SPDX-License-Identifier: Apache-2.0

//! Approval-gate behaviour under the Guarded posture: sequential approvals, denials, cancellation while waiting, and the auto-approved bypass.

use super::base_runtime;
use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::{AgentId, AlmsError};
use alms_session::{SessionConfig, SessionManager};

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
        tools,
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(config).unwrap())
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
        tools,
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        cancel_token: Some(cancel_token.clone()),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        cancel_token: Some(cancel_token.clone()),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        cancel_token: Some(cancel_token.clone()),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
        tools,
        event_sender: Some(tx),
        ..base_runtime(LlmClient::new(config).unwrap())
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
        tools,
        event_sender: Some(tx),
        shell_classification_mode: alms_core::config::ShellClassificationMode::BlockDestructive,
        ..base_runtime(LlmClient::new(config).unwrap())
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
