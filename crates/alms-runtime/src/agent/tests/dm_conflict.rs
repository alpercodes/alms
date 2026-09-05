// SPDX-License-Identifier: Apache-2.0

//! #364 — `send_message` / `ignore_message` conflict detection and the per-batch block.

use super::base_runtime;
use crate::agent::*;
use crate::events::RuntimeEvent;
use crate::llm_client::LlmClient;
use crate::llm_types::*;
use alms_core::AlmsError;
use alms_session::{SessionConfig, SessionManager};

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
        config: AgentConfig {
            posture,
            ..AgentConfig::default()
        },
        tools,
        ..base_runtime(LlmClient::new(llm_config).unwrap())
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
                    crate::agent::dm::DM_CONFLICT_MSG,
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
