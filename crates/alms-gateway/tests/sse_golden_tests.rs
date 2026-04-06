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
    let session_id = SessionId::new();

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
