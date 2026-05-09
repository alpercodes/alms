//! Golden tests for SSE event sequencing
//!
//! Tests event ordering and field alignment with docs/api.md

use alms_core::{RunId, TokenUsage};
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
