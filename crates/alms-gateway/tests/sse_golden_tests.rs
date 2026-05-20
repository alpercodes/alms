//! Golden tests for SSE event sequencing
//!
//! Tests event ordering and field alignment with docs/api.md

use alms_core::{RunId, SessionId, TokenUsage};
use alms_gateway::sse::{SseEventData, event_channel};

#[tokio::test]
async fn test_event_sequence_basic_run() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    // Send events per API spec
    tx.send(SseEventData::connected(run_id)).unwrap();
    tx.send(SseEventData::run_started(
        run_id,
        alms_core::SessionId::new(),
    ))
    .unwrap();
    tx.send(SseEventData::token_delta(run_id, "Hello world", None))
        .unwrap();
    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage::default(),
    ))
    .unwrap();

    // Collect events
    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];

    // Verify sequence
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].event_type, "connected");
    assert_eq!(events[1].event_type, "run_started");
    assert_eq!(events[2].event_type, "token_delta");
    assert_eq!(events[3].event_type, "run_finished");

    // Verify run_finished has 'ok' field
    let finished_data = &events[3].data;
    assert_eq!(finished_data["ok"], true);
    assert!(finished_data["ts"].is_string());
}

#[tokio::test]
async fn test_event_fields_match_spec() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    // Send run_started per API spec
    tx.send(SseEventData::run_started(
        run_id,
        alms_core::SessionId::new(),
    ))
    .unwrap();

    let event = rx.recv().await.unwrap();

    // Verify event structure per docs/api.md
    assert_eq!(event.event_type, "run_started");
    assert!(event.data["run_id"].is_string());
    assert!(event.data["ts"].is_string());

    // Verify ts is RFC3339 format
    let ts_str = event.data["ts"].as_str().unwrap();
    assert!(ts_str.contains("T")); // ISO8601 separator
}

#[tokio::test]
async fn test_event_sequence_with_error() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    tx.send(SseEventData::run_started(
        run_id,
        alms_core::SessionId::new(),
    ))
    .unwrap();
    tx.send(SseEventData::run_error(run_id, "Something went wrong"))
        .unwrap();

    let events = [rx.recv().await.unwrap(), rx.recv().await.unwrap()];

    assert_eq!(events[0].event_type, "run_started");
    assert_eq!(events[1].event_type, "run_error");

    // Verify error structure per API spec
    let error_data = &events[1].data;
    assert_eq!(error_data["error"]["code"], "INTERNAL");
    assert_eq!(error_data["error"]["message"], "Something went wrong");
}

#[tokio::test]
async fn test_event_sequence_with_cancellation() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();
    let session_id = alms_core::SessionId::new();

    // Simulate: run starts, then gets cancelled before finishing
    tx.send(SseEventData::run_started(run_id, session_id))
        .unwrap();
    tx.send(SseEventData::token_delta(run_id, "partial output", None))
        .unwrap();
    tx.send(SseEventData::run_cancelled(run_id)).unwrap();

    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];

    // Verify sequence
    assert_eq!(events[0].event_type, "run_started");
    assert_eq!(events[1].event_type, "token_delta");
    assert_eq!(events[2].event_type, "run_cancelled");

    // Verify run_cancelled structure per API spec
    let cancelled_data = &events[2].data;
    assert_eq!(cancelled_data["run_id"], run_id.0.to_string());
    assert!(
        cancelled_data["ts"].is_string(),
        "ts should be a string timestamp"
    );

    // Verify ts is RFC3339 format
    let ts_str = cancelled_data["ts"].as_str().unwrap();
    assert!(ts_str.contains("T"), "ts should be ISO8601/RFC3339");
}

/// Tests that a run with debug_mode enabled emits a `context_debug` SSE event
/// containing the assembled context snapshot (messages, tools, token counts).
/// This is a regression test for #517 where `debug_mode` was not applied to
/// the agent config, so `context_debug` events were never emitted.
#[tokio::test]
async fn test_event_sequence_with_debug_mode() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();
    let session_id = alms_core::SessionId::new();

    // Simulate a run where debug_mode is enabled: run_started, context_debug,
    // token_delta, run_finished.
    tx.send(SseEventData::run_started(run_id, session_id))
        .unwrap();
    tx.send(SseEventData::context_debug(
        run_id,
        serde_json::json!([
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Hello"},
        ]),
        vec!["shell_exec".to_string(), "fs_read".to_string()],
        1200,
        400,
        2,
        "00000000-0000-0000-0000-000000000abc".to_string(),
        Some("alpha".to_string()),
    ))
    .unwrap();
    tx.send(SseEventData::token_delta(run_id, "Hi there!", None))
        .unwrap();
    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage::default(),
    ))
    .unwrap();

    let events = [
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
        rx.recv().await.unwrap(),
    ];

    // Verify event sequence includes context_debug after run_started
    assert_eq!(events[0].event_type, "run_started");
    assert_eq!(events[1].event_type, "context_debug");
    assert_eq!(events[2].event_type, "token_delta");
    assert_eq!(events[3].event_type, "run_finished");

    // Verify context_debug structure
    let debug_data = &events[1].data;
    assert_eq!(debug_data["run_id"], run_id.0.to_string());
    assert_eq!(debug_data["total_tokens"], 1200);
    assert_eq!(debug_data["system_tokens"], 400);
    assert_eq!(debug_data["history_message_count"], 2);
    assert_eq!(debug_data["tool_names"].as_array().unwrap().len(), 2);
    assert_eq!(debug_data["messages"].as_array().unwrap().len(), 2);
    // #1003: agent attribution must reach the wire so the UI can label
    // the panel correctly — especially important on DM sessions where
    // two agents alternate turns on the same session.
    assert_eq!(
        debug_data["agent_id"],
        "00000000-0000-0000-0000-000000000abc"
    );
    assert_eq!(debug_data["agent_name"], "alpha");
    assert!(
        debug_data["ts"].is_string(),
        "ts should be a string timestamp"
    );
}

/// Tests that `run_finished` serialization is byte-identical to pre-#768
/// for non-reasoning runs: when `TokenUsage.reasoning_tokens` is `None`,
/// the field must be absent from the wire (not emitted as `null`).
#[tokio::test]
async fn test_run_finished_no_reasoning_tokens_field_when_absent() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 200,
            ..TokenUsage::default()
        },
    ))
    .unwrap();

    let event = rx.recv().await.unwrap();
    assert_eq!(event.event_type, "run_finished");
    let data = &event.data;
    assert_eq!(data["prompt_tokens"], 100);
    assert_eq!(data["completion_tokens"], 200);
    // `skip_serializing_if = "Option::is_none"` must keep the field off the
    // wire entirely for non-reasoning runs.
    assert!(
        data.get("reasoning_tokens").is_none(),
        "reasoning_tokens should be absent from the wire when None, not serialized as null; got: {data:?}"
    );
}

/// Tests that a reasoning run emits `reasoning_tokens: Some(N)` on the SSE
/// `run_finished` event, plumbed through from `TokenUsage.reasoning_tokens`
/// (#768). Previously the field was dropped at the SSE boundary — fixed in
/// response to Tim's review on #777 (C1).
#[tokio::test]
async fn test_run_finished_emits_reasoning_tokens_when_present() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage {
            prompt_tokens: 150,
            completion_tokens: 80,
            reasoning_tokens: Some(1024),
            ..TokenUsage::default()
        },
    ))
    .unwrap();

    let event = rx.recv().await.unwrap();
    assert_eq!(event.event_type, "run_finished");
    let data = &event.data;
    assert_eq!(data["prompt_tokens"], 150);
    assert_eq!(data["completion_tokens"], 80);
    assert_eq!(data["reasoning_tokens"], 1024);
}

/// Issue #766: Anthropic prompt-caching metrics flow through the SSE
/// `run_finished` event when populated. Absent from the wire when `None`
/// so non-Anthropic runs stay byte-identical to pre-#766.
#[tokio::test]
async fn test_run_finished_emits_cache_tokens_when_present() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage {
            prompt_tokens: 42,
            completion_tokens: 7,
            cache_creation_input_tokens: Some(1500),
            cache_read_input_tokens: Some(8200),
            ..TokenUsage::default()
        },
    ))
    .unwrap();

    let event = rx.recv().await.unwrap();
    assert_eq!(event.event_type, "run_finished");
    let data = &event.data;
    assert_eq!(data["prompt_tokens"], 42);
    assert_eq!(data["completion_tokens"], 7);
    assert_eq!(data["cache_creation_input_tokens"], 1500);
    assert_eq!(data["cache_read_input_tokens"], 8200);
    // reasoning_tokens was None — must not be on the wire.
    assert!(
        data.get("reasoning_tokens").is_none(),
        "reasoning_tokens should still be absent when None alongside cache fields"
    );
}

/// When cache tokens are `None` (non-Anthropic runs, or Anthropic runs
/// with caching disabled), the fields must be absent from the wire
/// entirely — matches the pre-#766 byte shape.
#[tokio::test]
async fn test_run_finished_cache_tokens_absent_when_none() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    tx.send(SseEventData::run_finished(
        run_id,
        true,
        TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 200,
            ..TokenUsage::default()
        },
    ))
    .unwrap();

    let event = rx.recv().await.unwrap();
    let data = &event.data;
    assert!(
        data.get("cache_creation_input_tokens").is_none(),
        "cache_creation_input_tokens must be absent when None; got: {data:?}"
    );
    assert!(
        data.get("cache_read_input_tokens").is_none(),
        "cache_read_input_tokens must be absent when None; got: {data:?}"
    );
}

/// Tests the pre-start cancellation path where `run_cancelled` is emitted
/// with NO preceding `run_started` — e.g. when a queued run is cancelled
/// before execution begins, or the server is shutting down.
/// See `crates/alms-gateway/src/runs/lifecycle.rs` lines 310-319.
#[tokio::test]
async fn test_event_sequence_pre_start_cancellation() {
    let (tx, mut rx) = event_channel();
    let run_id = RunId::new();

    // Only connected + run_cancelled — no run_started in between
    tx.send(SseEventData::connected(run_id)).unwrap();
    tx.send(SseEventData::run_cancelled(run_id)).unwrap();

    let events = [rx.recv().await.unwrap(), rx.recv().await.unwrap()];

    // Verify sequence: connected directly followed by run_cancelled
    assert_eq!(events[0].event_type, "connected");
    assert_eq!(events[1].event_type, "run_cancelled");

    // Verify run_cancelled structure
    let cancelled_data = &events[1].data;
    assert_eq!(cancelled_data["run_id"], run_id.0.to_string());
    assert!(
        cancelled_data["ts"].is_string(),
        "ts should be a string timestamp"
    );

    // Verify ts is RFC3339 format
    let ts_str = cancelled_data["ts"].as_str().unwrap();
    assert!(ts_str.contains("T"), "ts should be ISO8601/RFC3339");
}

// ---------------------------------------------------------------------------
// #1105 bg-path ordering invariant
// ---------------------------------------------------------------------------

/// Pins the ordering invariant that protects the #1105 bg-path reroute fix
/// (commit `481ef7c` on PR #1115): for a **background** `invoke_agent`
/// invocation, the parent's `tool_start (invoke_agent)` must precede
/// `subagent_started` on the parent's session SSE stream, and the two events
/// must carry the same `tool_invocation_id`.
///
/// Pre-fix the bg-channel handler synthesised `subagent_started` SSE directly
/// onto a session-level channel that was independent of the parent's
/// `runtime_tx`. Two consumer tasks drained those two channels in parallel,
/// so `subagent_started` could reach the client *before* the parent's
/// `tool_start (invoke_agent)`. The frontend's
/// `findSubagentByToolInvocationId` resolver would then find no entry to
/// attach the new `subagent_session_id` to.
///
/// Post-fix `route_bg_event` returns `None` for `RuntimeEvent::SubagentStarted`
/// and instead forwards it back onto the parent's runtime channel via
/// `bg_runtime_fwd` (a clone of `invoke_agent_fwd` held by the bg event task
/// in `lifecycle.rs`). The single-channel FIFO from that point preserves
/// ordering: the agent loop enqueues the parent's `ToolStart` onto
/// `runtime_tx` *before* `tool.execute()` runs, i.e. before the bg task can
/// observe `SubagentStarted` on `bg_event_rx` at all.
///
/// **Regression coverage**: if `route_bg_event` is reverted to synthesise SSE
/// directly for `SubagentStarted` (the pre-`481ef7c` shape), this test fails
/// on two fronts — the function would return `Some(SseEventData)` instead of
/// `None`, *and* nothing would land on the parent's runtime channel.
///
/// (Foreground-path ordering is already covered implicitly by
/// `forward_runtime_events` draining a single channel in FIFO order; the
/// bg path is the asymmetric case that needed the explicit reroute.)
#[tokio::test]
async fn test_bg_subagent_started_ordering_invariant() {
    use alms_gateway::runs::tools::route_bg_event;
    use alms_runtime::RuntimeEvent;
    use std::sync::Arc;
    use uuid::Uuid;

    // The parent's runtime channel — `forward_runtime_events` would drain
    // this in production. The bg task holds an `Arc<dyn EventForwarder>`
    // (clone of `invoke_agent_fwd`) whose sends land here.
    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    // A minimal `EventForwarder` that mirrors `RuntimeEventForwarder` from
    // `runs/tools.rs`: send each method's `RuntimeEvent` variant onto the
    // parent's channel. We only need `forward_subagent_started` to assert
    // the invariant, but implementing the whole trait keeps the test honest
    // to the production shape.
    #[derive(Debug)]
    struct TestForwarder {
        tx: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
    }
    impl alms_tools::EventForwarder for TestForwarder {
        fn forward_tool_start(
            &self,
            invocation_id: Uuid,
            tool: String,
            params: serde_json::Value,
            source_agent: Option<String>,
            task_id: Option<String>,
        ) {
            let _ = self.tx.send(RuntimeEvent::ToolStart {
                invocation_id,
                tool,
                params,
                source_agent,
                task_id,
            });
        }
        fn forward_tool_end(
            &self,
            invocation_id: Uuid,
            ok: bool,
            result: serde_json::Value,
            source_agent: Option<String>,
            task_id: Option<String>,
        ) {
            let _ = self.tx.send(RuntimeEvent::ToolEnd {
                invocation_id,
                ok,
                result,
                source_agent,
                task_id,
            });
        }
        fn forward_token_delta(&self, delta: String, source_agent: Option<String>) {
            let _ = self.tx.send(RuntimeEvent::TokenDelta {
                delta,
                source_agent,
            });
        }
        fn forward_status(&self, phase: String, detail: Option<String>) {
            let _ = self.tx.send(RuntimeEvent::Status { phase, detail });
        }
        fn forward_warning(&self, code: String, message: String, source_agent: Option<String>) {
            let _ = self.tx.send(RuntimeEvent::Warning {
                code,
                message,
                source_agent,
            });
        }
        fn forward_subagent_started(
            &self,
            tool_invocation_id: Uuid,
            subagent_name: Option<String>,
            subagent_session_id: Uuid,
        ) {
            let _ = self.tx.send(RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id: SessionId(subagent_session_id),
            });
        }
    }

    let bg_runtime_fwd: Arc<dyn alms_tools::EventForwarder> = Arc::new(TestForwarder {
        tx: runtime_tx.clone(),
    });
    let bg_run_id = RunId::new();

    // Shared invocation id — the parent's `invoke_agent` `ToolStart` and the
    // subagent's `SubagentStarted` must reference the same one for the
    // frontend resolver to attach the subagent session id.
    let tool_invocation_id = Uuid::new_v4();
    let subagent_session = SessionId::new();

    // ---- Step 1: agent loop enqueues parent's `ToolStart (invoke_agent)`
    //              onto `runtime_tx` BEFORE `tool.execute()` runs. ----
    runtime_tx
        .send(RuntimeEvent::ToolStart {
            invocation_id: tool_invocation_id,
            tool: "invoke_agent".to_string(),
            params: serde_json::json!({
                "agent_name": "helper",
                "background": true,
                "input": "go",
            }),
            source_agent: None,
            task_id: None,
        })
        .unwrap();

    // ---- Step 2: spawn_subagent enqueues `SubagentStarted` onto
    //              `bg_event_tx`; the bg event task routes it via
    //              `route_bg_event`. ----
    let outcome = route_bg_event(
        RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name: Some("helper".to_string()),
            subagent_session_id: subagent_session,
        },
        &*bg_runtime_fwd,
        bg_run_id,
    );

    // Invariant 1: `SubagentStarted` from the bg channel MUST NOT be
    // synthesised into SSE directly. A revert to the pre-`481ef7c` shape
    // returns `Some(SseEventData)` here and this assertion fails.
    assert!(
        outcome.is_none(),
        "route_bg_event must NOT synthesise SSE for SubagentStarted on \
         the bg path -- it must forward back onto the parent's runtime \
         channel (#1105). Got: {outcome:?}",
    );

    // ---- Step 3: drain the parent's runtime channel and assert ordering. ----
    let first = runtime_rx
        .recv()
        .await
        .expect("parent's runtime channel must have the ToolStart");
    let second = runtime_rx
        .recv()
        .await
        .expect("parent's runtime channel must have the rerouted SubagentStarted");

    // Invariant 2: parent's `ToolStart (invoke_agent)` is first on the
    // single-channel FIFO that `forward_runtime_events` will convert to SSE.
    match &first {
        RuntimeEvent::ToolStart {
            invocation_id,
            tool,
            ..
        } => {
            assert_eq!(tool, "invoke_agent");
            assert_eq!(*invocation_id, tool_invocation_id);
        }
        _ => panic!("expected ToolStart(invoke_agent) first, got a different variant"),
    }

    // Invariant 3: `SubagentStarted` lands on the SAME channel AFTER the
    // parent's `ToolStart`, with the matching `tool_invocation_id` so the
    // frontend resolver can attach the `subagent_session_id` to the right
    // SubagentBar entry. A revert that drops the reroute leaves this side
    // of the channel empty and `runtime_rx.recv()` would hang -- guarded by
    // the test runtime's default timeout.
    match &second {
        RuntimeEvent::SubagentStarted {
            tool_invocation_id: tid,
            subagent_session_id,
            subagent_name,
        } => {
            assert_eq!(
                *tid, tool_invocation_id,
                "SubagentStarted.tool_invocation_id must match the parent's \
                 invoke_agent invocation so the UI resolver can attach the \
                 session id to the right entry"
            );
            assert_eq!(*subagent_session_id, subagent_session);
            assert_eq!(subagent_name.as_deref(), Some("helper"));
        }
        _ => panic!("expected SubagentStarted second, got a different variant"),
    }

    // Belt-and-braces: nothing else should be in the parent's channel.
    // Drop our sender so `recv()` returns `None` instead of pending.
    drop(runtime_tx);
    drop(bg_runtime_fwd);
    assert!(
        runtime_rx.recv().await.is_none(),
        "no extra events should land on the parent's runtime channel"
    );
}
