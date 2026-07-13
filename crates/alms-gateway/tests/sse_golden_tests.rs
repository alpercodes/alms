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
        fn forward_stream_reset(&self) {}
        fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
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
            background: bool,
        ) {
            let _ = self.tx.send(RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id: SessionId(subagent_session_id),
                background,
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
    let parent_session = SessionId::new();
    let outcome = route_bg_event(
        RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name: Some("helper".to_string()),
            subagent_session_id: subagent_session,
            background: true,
        },
        // Parent alive: pass the live forwarder so the reroute fires.
        Some(&*bg_runtime_fwd),
        bg_run_id,
        parent_session,
    );

    // Invariant 1: `SubagentStarted` from the bg channel MUST NOT be
    // synthesised into SSE directly while the parent is alive. A revert to the
    // pre-`481ef7c` shape returns `Some(SseEventData)` here and this assertion
    // fails.
    assert!(
        outcome.is_none(),
        "route_bg_event must NOT synthesise SSE for SubagentStarted on \
         the bg path while the parent is alive -- it must forward back onto \
         the parent's runtime channel (#1105). Got: {outcome:?}",
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
            background,
        } => {
            assert_eq!(
                *tid, tool_invocation_id,
                "SubagentStarted.tool_invocation_id must match the parent's \
                 invoke_agent invocation so the UI resolver can attach the \
                 session id to the right entry"
            );
            assert_eq!(*subagent_session_id, subagent_session);
            assert_eq!(subagent_name.as_deref(), Some("helper"));
            // The `background` flag must survive the reroute so the
            // foreground-only #1125 (A1-1) marker stays suppressed for bg
            // subagents in `forward_runtime_events`.
            assert!(
                *background,
                "bg-path SubagentStarted must keep background=true through \
                 the route_bg_event reroute"
            );
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

/// Regression test for the #1115 background-subagent hang.
///
/// PR #1115 changed the gateway's background event-forwarder task to reroute
/// `SubagentStarted` back onto the parent's `runtime_tx` via a clone of
/// `invoke_agent_fwd`. The original commit held that clone as a **strong**
/// `Arc`, which created a deadlock:
///
///   - The bg event-forwarder task lives as long as `bg_event_rx` has any
///     sender, and `bg_event_tx` (inside `bg_event_fwd`) is cloned into the
///     coordinator's subagent→parent *relay* task. That relay task only drops
///     its `bg_event_tx` clone when the detached background subagent's loop
///     finishes — which can be many seconds (or its full TTL) after the parent
///     run's own `runtime.run()` returns.
///   - A strong `invoke_agent_fwd` clone in the bg task is therefore a live
///     `runtime_tx` sender for the entire background-subagent lifetime.
///   - `forward_runtime_events` (awaited at `forwarder_handle.await` in
///     `execute_run`) only completes once ALL `runtime_tx` senders close. With
///     a lingering strong bg sender, `forwarder_handle.await` blocks and the
///     parent run hangs in `Running` until the bg subagent completes.
///
/// The fix holds the bg-task forwarder as a `Weak` reference so it does not
/// count toward the `runtime_tx` sender total. This test reconstructs the
/// exact channel topology and asserts that the parent's drain
/// (`forward_runtime_events`-equivalent) completes promptly even though a
/// `bg_event_tx` clone is still alive (simulating the in-flight bg subagent).
///
/// A revert to the strong-`Arc` shape makes the parent-drain `recv()` below
/// hang forever, tripping the test runtime's timeout.
#[tokio::test]
async fn test_bg_subagent_does_not_block_parent_run_drain() {
    use alms_gateway::runs::tools::route_bg_event;
    use alms_runtime::RuntimeEvent;
    use std::sync::Arc;

    #[derive(Debug)]
    struct ChannelForwarder {
        tx: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
    }
    impl alms_tools::EventForwarder for ChannelForwarder {
        fn forward_tool_start(
            &self,
            invocation_id: uuid::Uuid,
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
            invocation_id: uuid::Uuid,
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
        fn forward_stream_reset(&self) {}
        fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
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
            tool_invocation_id: uuid::Uuid,
            subagent_name: Option<String>,
            subagent_session_id: uuid::Uuid,
            background: bool,
        ) {
            let _ = self.tx.send(RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id: SessionId(subagent_session_id),
                background,
            });
        }
    }

    // The parent's runtime channel — drained by `forward_runtime_events` in
    // production. `runtime_rx` standing in for that drain.
    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();

    // `invoke_agent_fwd`: the STRONG forwarder held by `InvokeAgentTool`
    // (which lives inside the runtime's tool registry). Dropped at
    // `drop(runtime)` in `execute_run`.
    let invoke_agent_fwd: Arc<dyn alms_tools::EventForwarder> = Arc::new(ChannelForwarder {
        tx: runtime_tx.clone(),
    });

    // The bg subagent event channel. `bg_event_tx` is cloned into the
    // coordinator's relay task; we model that lingering clone with
    // `relay_holds_bg_tx` and keep it alive past the parent teardown.
    let (bg_event_tx, bg_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();
    let relay_holds_bg_tx = bg_event_tx.clone();
    let bg_run_id = RunId::new();

    // The bg event-forwarder task — a faithful copy of the loop in
    // `runs/lifecycle.rs`: holds `invoke_agent_fwd` only as a `Weak`, upgrades
    // per-event, and routes via the production `route_bg_event`. After the
    // parent teardown the upgrade returns `None`, which `route_bg_event`
    // handles internally (the dead-parent `Some(parent_fwd)` argument).
    let bg_session_id = SessionId::new();
    let bg_runtime_fwd = Arc::downgrade(&invoke_agent_fwd);
    let routed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let routed_in_task = routed.clone();
    let bg_task = tokio::spawn(async move {
        let mut rx = bg_event_rx;
        while let Some(event) = rx.recv().await {
            let upgraded = bg_runtime_fwd.upgrade();
            let _sse = route_bg_event(event, upgraded.as_deref(), bg_run_id, bg_session_id);
            routed_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });

    // Step 1: the bg subagent emits a `SubagentStarted` (in production this is
    // queued by `spawn_subagent` BEFORE `dispatch_background` returns, i.e.
    // while `invoke_agent_fwd` is alive). It must reroute onto `runtime_tx`.
    let inv_id = uuid::Uuid::new_v4();
    let sub_session = SessionId::new();
    bg_event_tx
        .send(RuntimeEvent::SubagentStarted {
            tool_invocation_id: inv_id,
            subagent_name: Some("worker".to_string()),
            subagent_session_id: sub_session,
            background: true,
        })
        .unwrap();

    // The reroute lands on the parent's runtime channel.
    let rerouted = runtime_rx
        .recv()
        .await
        .expect("SubagentStarted must reroute onto the parent's runtime channel");
    match rerouted {
        RuntimeEvent::SubagentStarted {
            tool_invocation_id, ..
        } => assert_eq!(tool_invocation_id, inv_id),
        _ => panic!("expected rerouted SubagentStarted on the parent's runtime channel"),
    }

    // Step 2: parent run finishes. `execute_run` drops `runtime`, which drops
    // both the runtime's own `runtime_tx` sender AND `invoke_agent_fwd` (the
    // strong forwarder inside `InvokeAgentTool`). Model both drops here.
    drop(invoke_agent_fwd);
    drop(runtime_tx);

    // CRUCIAL: the background subagent is STILL RUNNING — its relay task holds
    // a live `bg_event_tx` clone. Pre-fix, this kept a strong `runtime_tx`
    // sender alive inside the bg task and the drain below would hang.
    //
    // The parent's `forward_runtime_events`-equivalent drain must now complete
    // promptly: with the Weak fix, the only remaining `runtime_tx` senders
    // (the runtime + `invoke_agent_fwd`) are gone, so `recv()` returns `None`.
    let drain = tokio::time::timeout(std::time::Duration::from_secs(5), runtime_rx.recv()).await;
    match drain {
        Ok(None) => {}
        Ok(Some(_)) => panic!("unexpected extra event on the parent's runtime channel"),
        Err(_) => panic!(
            "parent run drain must complete (recv -> None) once the runtime and \
             invoke_agent_fwd senders drop, even though a background subagent's \
             bg_event_tx clone is still alive. A strong bg_runtime_fwd clone would \
             keep a runtime_tx sender alive and hang this recv (the #1115 deadlock)."
        ),
    }

    // The bg subagent then continues to emit its status signals, which still
    // route to SSE even after the parent is gone (the Weak upgrade fails, so
    // `route_bg_event` is called with `parent_fwd == None` and converts the
    // `SubagentActivity` directly — the status bar keeps updating after the
    // parent turn ends).
    relay_holds_bg_tx
        .send(RuntimeEvent::SubagentActivity {
            kind: "tool_end".to_string(),
            tool: None,
            tool_invocation_id: Some(uuid::Uuid::new_v4()),
            parent_tool_invocation_id: Some(uuid::Uuid::new_v4()),
            source_agent: "worker".to_string(),
        })
        .unwrap();

    // Close the bg channel (subagent finished) and let the bg task drain.
    drop(relay_holds_bg_tx);
    drop(bg_event_tx);
    bg_task.await.unwrap();
    assert_eq!(
        routed.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "bg task must process both the SubagentStarted reroute and the \
         post-parent-teardown SubagentActivity"
    );
}

/// Regression test for #1125 A1-3: a background subagent's `SubagentStarted`
/// emitted **after** the parent run has finished must still reach the parent's
/// session stream (via the `send_session_event` fallback) instead of being
/// silently dropped — while the parent-alive case must NOT double-deliver.
///
/// Failure scenario (pre-fix): the bg `SubagentStarted` is rerouted onto the
/// parent's `runtime_tx` via `forward_subagent_started`. Once the parent run
/// finishes and drops `invoke_agent_fwd`, the bg task's `Weak::upgrade` returns
/// `None`, so `route_bg_event` was called with a no-op forwarder and the
/// reroute became a silent drop — the #1105 "View session during the run"
/// surface was lost for a bg subagent spawned near the end of a parent turn.
///
/// Post-fix `route_bg_event` takes `parent_fwd: Option<&dyn EventForwarder>`:
/// - `Some(fwd)` (parent alive): reroute through the parent channel, return
///   `None` so the caller does NOT also emit on the session stream (one
///   delivery, FIFO after the parent's `tool_start`).
/// - `None` (parent dead): return `Some(Persist(subagent_started))` so the
///   caller delivers it via `send_session_event` (the fallback), carrying the
///   same `tool_invocation_id` + `subagent_session_id` the live event would.
///
/// Both invariants are asserted here, falsifiably:
///   - Parent-alive leg: `route_bg_event` returns `None` AND exactly one
///     `SubagentStarted` lands on the parent's runtime channel (no fallback
///     SSE). A revert that also emitted on the session stream would make the
///     no-double-delivery assertion fail.
///   - Parent-dead leg: `route_bg_event` returns `Some(Persist(..))` of type
///     `subagent_started`, carrying `tool_invocation_id`, `subagent_session_id`
///     and the parent `session_id`; AND nothing lands on the (dead) parent
///     channel. A revert to the silent-drop shape returns `None` here and the
///     fallback-delivery assertion fails. `Persist` (not `Transient`) matters:
///     the fallback is a durable lifecycle marker, so it must go through
///     `send_session_event` and land in the parent's session event log.
#[tokio::test]
async fn test_bg_subagent_started_falls_back_to_session_stream_when_parent_dead() {
    use alms_gateway::runs::tools::route_bg_event;
    use alms_runtime::RuntimeEvent;
    use std::sync::Arc;
    use uuid::Uuid;

    // Minimal forwarder mirroring `RuntimeEventForwarder`: pushes the
    // rerouted `SubagentStarted` onto the parent's runtime channel. Only
    // `forward_subagent_started` is exercised; the rest are no-ops because
    // this test only routes `SubagentStarted` events.
    #[derive(Debug)]
    struct ChannelForwarder {
        tx: tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
    }
    impl alms_tools::EventForwarder for ChannelForwarder {
        fn forward_tool_start(
            &self,
            _: Uuid,
            _: String,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
        }
        fn forward_tool_end(
            &self,
            _: Uuid,
            _: bool,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
        }
        fn forward_token_delta(&self, _: String, _: Option<String>) {}
        fn forward_stream_reset(&self) {}
        fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
        fn forward_status(&self, _: String, _: Option<String>) {}
        fn forward_warning(&self, _: String, _: String, _: Option<String>) {}
        fn forward_subagent_started(
            &self,
            tool_invocation_id: Uuid,
            subagent_name: Option<String>,
            subagent_session_id: Uuid,
            background: bool,
        ) {
            let _ = self.tx.send(RuntimeEvent::SubagentStarted {
                tool_invocation_id,
                subagent_name,
                subagent_session_id: SessionId(subagent_session_id),
                background,
            });
        }
    }

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let parent_fwd: Arc<dyn alms_tools::EventForwarder> = Arc::new(ChannelForwarder {
        tx: runtime_tx.clone(),
    });
    let bg_run_id = RunId::new();
    let parent_session = SessionId::new();
    let tool_invocation_id = Uuid::new_v4();
    let subagent_session = SessionId::new();

    // ---- Leg 1: PARENT ALIVE -> reroute through parent channel, no fallback. ----
    let alive = route_bg_event(
        RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name: Some("worker".to_string()),
            subagent_session_id: subagent_session,
            background: true,
        },
        Some(&*parent_fwd),
        bg_run_id,
        parent_session,
    );
    assert!(
        alive.is_none(),
        "parent-alive: route_bg_event must reroute via the parent channel and \
         return None (no session-stream fallback) -- got {alive:?}"
    );
    // Exactly one SubagentStarted on the parent channel, matching the
    // invocation. (`RuntimeEvent` is not `Debug`, so match explicitly rather
    // than `{:?}`-formatting the received value.)
    match runtime_rx.try_recv() {
        Ok(RuntimeEvent::SubagentStarted {
            tool_invocation_id: tid,
            subagent_session_id: sid,
            ..
        }) => {
            assert_eq!(tid, tool_invocation_id);
            assert_eq!(sid, subagent_session);
        }
        Ok(_) => {
            panic!("expected a rerouted SubagentStarted on the parent channel, got another variant")
        }
        Err(_) => {
            panic!("parent-alive leg must reroute the SubagentStarted onto the parent channel")
        }
    }
    assert!(
        runtime_rx.try_recv().is_err(),
        "parent-alive leg must deliver the SubagentStarted exactly once"
    );

    // ---- Leg 2: PARENT DEAD -> fallback SseEventData for the session stream. ----
    // Model the parent teardown: drop the forwarder and the channel just like
    // `drop(runtime)` does in `execute_run`. In production the bg task observes
    // this as `Weak::upgrade() -> None` and passes `parent_fwd = None`.
    drop(parent_fwd);
    drop(runtime_tx);

    let dead = route_bg_event(
        RuntimeEvent::SubagentStarted {
            tool_invocation_id,
            subagent_name: Some("worker".to_string()),
            subagent_session_id: subagent_session,
            background: true,
        },
        None,
        bg_run_id,
        parent_session,
    );

    // The dead-parent leg must produce a `subagent_started` SSE for the caller
    // to deliver on the session stream — NOT a silent drop. It must be the
    // Persist route: this fallback is a durable lifecycle marker.
    let sse = match dead.expect(
        "parent-dead: route_bg_event must FALL BACK to a subagent_started \
         SseEventData (delivered via send_session_event) instead of dropping \
         the event (#1125 A1-3)",
    ) {
        alms_gateway::runs::tools::RoutedBgEvent::Persist(sse) => sse,
        alms_gateway::runs::tools::RoutedBgEvent::Transient(sse) => panic!(
            "parent-dead subagent_started fallback must be Persist (durable \
             lifecycle marker), got Transient({sse:?})"
        ),
    };
    assert_eq!(
        sse.event_type, "subagent_started",
        "fallback event must be a subagent_started event"
    );
    assert_eq!(
        sse.data["session_id"].as_str(),
        Some(parent_session.0.to_string().as_str()),
        "fallback must carry the PARENT session id (the session stream it lands on)"
    );
    assert_eq!(
        sse.data["tool_invocation_id"].as_str(),
        Some(tool_invocation_id.to_string().as_str()),
        "fallback must carry the parent invoke_agent tool_invocation_id so the \
         frontend resolver attaches the session id to the right chip"
    );
    assert_eq!(
        sse.data["subagent_session_id"].as_str(),
        Some(subagent_session.0.to_string().as_str()),
        "fallback must carry the subagent's session id for the View-session link"
    );
    assert_eq!(
        sse.data["subagent_name"].as_str(),
        Some("worker"),
        "fallback must preserve the subagent name when present"
    );
}

/// Contract test for the Subagent status bar (#1180 follow-up, subsumes
/// #1186): the parent's stream receives a background subagent's coarse
/// `subagent_activity` STATUS signal — and never the subagent's content.
///
/// The coordinator relay reduces the subagent's reasoning/token deltas and
/// tool events to deduplicated `RuntimeEvent::SubagentActivity` signals;
/// `route_bg_event` must convert those to a **Transient** `subagent_activity`
/// SSE (live fan-out only, never persisted to the parent's session event
/// log). The subagent's raw content events must NOT be forwarded at all —
/// pre-status-bar, every `reasoning_delta` and tool start/end (with full
/// params/results) was persisted to the parent's session log for content the
/// UI never displayed.
///
/// Asserts falsifiably:
///   - `SubagentActivity` returns `Some(Transient(subagent_activity))` with
///     the `source_agent` tag, the activity kind, the tool name and the bg
///     run id — identically on BOTH parent-liveness legs (`Some(parent_fwd)`
///     and `None`): the signal is never rerouted through the parent's
///     `runtime_tx` (#1124 constraint; the bar must keep updating after the
///     parent turn ends). A revert to `Persist` fails the match; a revert to
///     the `_ => None` drop fails the `.expect`.
///   - Raw `ReasoningDelta` / `TokenDelta` / `ToolStart` / `ToolEnd` return
///     `None` — a revert that reintroduces content forwarding onto the parent
///     stream fails these.
#[tokio::test]
async fn test_bg_subagent_activity_signal_forwarded_transient_and_content_dropped() {
    use alms_gateway::runs::tools::{RoutedBgEvent, route_bg_event};
    use alms_runtime::RuntimeEvent;
    use std::sync::Arc;

    let bg_run_id = RunId::new();
    let parent_session = SessionId::new();

    // No-op parent forwarder for the parent-alive leg. Status signals must
    // NOT be rerouted through it (that path dies with the parent run); they
    // go straight to the session-stream return value regardless of liveness.
    #[derive(Debug)]
    struct NoopForwarder;
    impl alms_tools::EventForwarder for NoopForwarder {
        fn forward_tool_start(
            &self,
            _: uuid::Uuid,
            _: String,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
        }
        fn forward_tool_end(
            &self,
            _: uuid::Uuid,
            _: bool,
            _: serde_json::Value,
            _: Option<String>,
            _: Option<String>,
        ) {
        }
        fn forward_token_delta(&self, _: String, _: Option<String>) {}
        fn forward_status(&self, _: String, _: Option<String>) {}
        fn forward_warning(&self, _: String, _: String, _: Option<String>) {}
        fn forward_stream_reset(&self) {}
        fn forward_run_terminal(&self, _: alms_tools::SubagentRunOutcome) {}
    }
    let parent_fwd: Arc<dyn alms_tools::EventForwarder> = Arc::new(NoopForwarder);

    let activity_inv = uuid::Uuid::new_v4();
    let parent_inv = uuid::Uuid::new_v4();
    let mk_activity = || RuntimeEvent::SubagentActivity {
        kind: "tool_start".to_string(),
        tool: Some("shell".to_string()),
        tool_invocation_id: Some(activity_inv),
        parent_tool_invocation_id: Some(parent_inv),
        source_agent: "reviewer".to_string(),
    };

    let assert_activity = |routed: Option<RoutedBgEvent>| {
        let routed = routed.expect(
            "route_bg_event must forward a background subagent's \
             SubagentActivity as a session-stream SSE instead of dropping it \
             via the `_ => None` catch-all",
        );
        let sse = match routed {
            RoutedBgEvent::Transient(sse) => sse,
            RoutedBgEvent::Persist(sse) => panic!(
                "subagent_activity must be Transient (live fan-out only, \
                 never persisted to the parent's session event log), got \
                 Persist({sse:?})"
            ),
        };
        assert_eq!(sse.event_type, "subagent_activity");
        assert_eq!(
            sse.data["source_agent"].as_str(),
            Some("reviewer"),
            "the signal must keep the subagent's source_agent tag so the \
             frontend routes it to the matching status-bar chip"
        );
        assert_eq!(sse.data["kind"].as_str(), Some("tool_start"));
        assert_eq!(sse.data["tool"].as_str(), Some("shell"));
        assert_eq!(
            sse.data["tool_invocation_id"].as_str(),
            Some(activity_inv.to_string().as_str()),
            "the tool invocation id must survive onto the wire — it is the \
             UI's toolsUsed idempotency key (#1190)"
        );
        assert_eq!(
            sse.data["parent_tool_invocation_id"].as_str(),
            Some(parent_inv.to_string().as_str()),
            "the parent invoke_agent correlator must survive onto the wire — \
             it is the UI's identity-exact chip resolver, without which \
             concurrent unnamed subagents cross-attach status (#1190)"
        );
        assert_eq!(
            sse.data["run_id"].as_str(),
            Some(bg_run_id.0.to_string().as_str())
        );
    };

    // Parent-alive leg.
    assert_activity(route_bg_event(
        mk_activity(),
        Some(&*parent_fwd),
        bg_run_id,
        parent_session,
    ));

    // Parent-dead leg — same result (bar must keep updating after the parent
    // run ends, so the signal is never gated on parent liveness).
    assert_activity(route_bg_event(
        mk_activity(),
        None,
        bg_run_id,
        parent_session,
    ));

    // The subagent's raw content events must be DROPPED on the parent path —
    // the parent stream carries subagent status, never subagent content.
    // (In production the coordinator relay no longer emits these onto the bg
    // channel at all; this pins the belt-and-braces drop in route_bg_event.)
    let content_events = [
        RuntimeEvent::ReasoningDelta {
            text: "thinking hard".to_string(),
            source_agent: Some("reviewer".to_string()),
        },
        RuntimeEvent::TokenDelta {
            delta: "output text".to_string(),
            source_agent: Some("reviewer".to_string()),
        },
        RuntimeEvent::ToolStart {
            invocation_id: uuid::Uuid::new_v4(),
            tool: "shell".to_string(),
            params: serde_json::json!({"cmd": "ls"}),
            source_agent: Some("reviewer".to_string()),
            task_id: None,
        },
        RuntimeEvent::ToolEnd {
            invocation_id: uuid::Uuid::new_v4(),
            ok: true,
            result: serde_json::json!({"output": "big payload"}),
            source_agent: Some("reviewer".to_string()),
            task_id: None,
        },
    ];
    for event in content_events {
        assert!(
            route_bg_event(event, None, bg_run_id, parent_session).is_none(),
            "raw subagent content events must never be forwarded onto the \
             parent's session stream (status-only contract)"
        );
    }
}

/// The `subagent_activity` SSE must be EPHEMERAL end-to-end on the foreground
/// path too: `send_event` (the chokepoint `forward_runtime_events` uses) must
/// fan it out to live subscribers WITHOUT logging it to the run or session
/// event logs — persisted status signals would bloat the parent's session log
/// once per activity transition for a value that is worthless on replay.
#[tokio::test]
async fn test_subagent_activity_sse_is_ephemeral_not_persisted() {
    let run_manager = alms_gateway::RunManager::new();
    let run_id = RunId::new();
    let session_id = SessionId::new();

    // Live session subscriber.
    let mut rx = run_manager.subscribe_session(session_id);

    let hwm_before = run_manager.latest_session_event_id(session_id).await;

    run_manager
        .send_event(
            run_id,
            session_id,
            SseEventData::subagent_activity(
                run_id,
                "reasoning",
                None,
                None,
                None,
                "reviewer".to_string(),
            ),
        )
        .await;

    // Live subscriber receives it, with no persisted event id (the ephemeral
    // fast-path contract that lets it through the replay dedup filter).
    let received = rx.recv().await.expect("live subscriber must receive it");
    assert_eq!(received.event_type, "subagent_activity");
    assert_eq!(received.data["kind"].as_str(), Some("reasoning"));
    assert!(
        received.event_id.is_none(),
        "ephemeral subagent_activity must not carry a persisted event id"
    );

    // Nothing was persisted to the session event log (HWM unchanged)...
    let hwm_after = run_manager.latest_session_event_id(session_id).await;
    assert_eq!(
        hwm_before, hwm_after,
        "subagent_activity must NOT be persisted to the session event log"
    );
    // ...nor to the per-run event log.
    assert!(
        run_manager.events_from(run_id, 0).await.is_empty(),
        "subagent_activity must NOT be persisted to the run event log"
    );

    // Contrast: the transient session path used by the background drain also
    // fans out without persisting.
    run_manager.send_transient_session_event(
        session_id,
        SseEventData::subagent_activity(
            run_id,
            "writing",
            None,
            None,
            None,
            "reviewer".to_string(),
        ),
    );
    let received = rx.recv().await.expect("transient fan-out must deliver");
    assert_eq!(received.event_type, "subagent_activity");
    assert!(received.event_id.is_none());
    assert_eq!(
        run_manager.latest_session_event_id(session_id).await,
        hwm_before,
        "send_transient_session_event must never write the session event log"
    );
}
