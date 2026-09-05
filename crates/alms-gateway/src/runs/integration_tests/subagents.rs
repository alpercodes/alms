// SPDX-License-Identifier: Apache-2.0

//! Subagent completion propagation, transcript reads (#1045), session-keyed cancel, and the status snapshot on reattach (#1189).

use super::{drain_events, subscribe_session, test_app_state, test_app_state_with_mock_llm};
use crate::test_support::{AppStateWithChannels, TestAppState};
use alms_coordinator::{SubagentCompletion, TaskId, TaskStatus};
use alms_core::{AgentId, RunId, SessionId, TokenUsage};
use alms_tools::SubagentDispatcher;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 6. Subagent completion with session ID propagation
// ---------------------------------------------------------------------------

/// Test that `completion_notification_loop` creates a run on the correct
/// parent session and emits a `subagent_completed` SSE event with the
/// subagent's session ID for frontend navigation.
///
/// This verifies the full subagent completion -> notification flow including
/// session ID propagation (#629 item).
#[tokio::test]
async fn subagent_completion_propagates_session_id() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "parent-chat");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    // Subscribe to the parent session to capture SSE events.
    let mut rx = subscribe_session(&state, parent_session_id);

    // Send a subagent completion event.
    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("researcher".to_string()),
            status: TaskStatus::Completed,
            summary: "Found 3 relevant papers on the topic.".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Research quantum computing advances".to_string()),
            tool_count: Some(5),
            duration_ms: Some(12000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 3000,
                completion_tokens: 800,
                ..TokenUsage::default()
            }),
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);

    // Run the completion notification loop.
    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // Verify a run was created on the parent session.
    let runs = state.run_manager.list_by_session(parent_session_id, 10);
    assert!(
        !runs.is_empty(),
        "completion notification should create a run on the parent session"
    );
    assert_eq!(runs[0].agent_id, parent_agent_id);

    // Verify the subagent_completed SSE event contains the subagent's session ID.
    let events = drain_events(&mut rx);
    let completion_event = events.iter().find(|e| e.event_type == "subagent_completed");
    assert!(
        completion_event.is_some(),
        "expected a subagent_completed SSE event on the parent session; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    if let Some(event) = completion_event {
        let data = &event.data;

        // Verify subagent session ID is propagated (for frontend navigation).
        assert_eq!(
            data.get("subagent_session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            Some(subagent_session_id.0.to_string()),
            "subagent_completed event must include the subagent's session_id \
             in the subagent_session_id field"
        );

        // Verify subagent name is included.
        assert_eq!(
            data.get("subagent_name").and_then(|v| v.as_str()),
            Some("researcher"),
            "subagent_completed event must include the subagent name"
        );

        // Verify status is included.
        assert_eq!(
            data.get("status").and_then(|v| v.as_str()),
            Some("done"),
            "subagent_completed event must include the status"
        );
    }

    // Verify a subagent_completion marker was persisted to session history.
    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let completion_markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(
        !completion_markers.is_empty(),
        "expected a subagent_completion marker persisted to session history"
    );

    // Verify the marker contains the subagent's session ID for navigation.
    let marker_meta = completion_markers[0].metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        Some(subagent_session_id.0.to_string()),
        "subagent_completion marker must include the subagent's session_id"
    );
    assert_eq!(
        marker_meta.get("subagent_name").and_then(|v| v.as_str()),
        Some("researcher"),
    );
    assert_eq!(
        marker_meta.get("status").and_then(|v| v.as_str()),
        Some("done"),
    );

    shutdown_token.cancel();
}

/// Drive `forward_runtime_events` with a single foreground
/// `RuntimeEvent::SubagentStarted` and assert it persists a durable
/// `subagent_started` lifecycle marker to the parent's session history
/// (#1125, A1-1).
///
/// Helper: pre-seeds an `invoke_agent` `Content::ToolCall` row into history
/// (mirroring the runtime agent loop, which appends the call row before
/// `tool.execute()` runs), runs `forward_runtime_events` on the supplied
/// event to completion, and returns the resulting parent-session history.
///
/// `subagent_name` is threaded into the `SubagentStarted` event so callers can
/// exercise both the named (`Some(..)`) and ephemeral/unnamed (`None`) paths —
/// the latter pins the conditional key-omission in the marker arm of
/// `forward_runtime_events`.
async fn run_subagent_started_marker_case(
    context_id: &str,
    background: bool,
    subagent_name: Option<&str>,
) -> (Vec<alms_session::Message>, uuid::Uuid, SessionId) {
    use alms_runtime::RuntimeEvent;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, context_id);
    let parent_session_id = parent_session.id;

    let tool_invocation_id = uuid::Uuid::new_v4();
    let subagent_session_id = SessionId::new();

    // Seed the parent's `invoke_agent` tool-call row BEFORE the marker, just
    // like the runtime agent loop (loop_impl.rs ~343) appends the call row
    // before `tool.execute()` runs. The marker must land AFTER this row so
    // the rehydrator sees the chip's tool row first.
    state
        .session_manager
        .append_message(
            parent_session_id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::Assistant,
                content: alms_session::Content::ToolCall {
                    name: "invoke_agent".to_string(),
                    params: serde_json::json!({ "agent_name": "researcher" }),
                },
                timestamp: alms_core::Timestamp::now(),
                metadata: None,
            },
        )
        .unwrap();

    // Drive `forward_runtime_events` with one foreground SubagentStarted.
    let (tx, rx) = mpsc::unbounded_channel::<RuntimeEvent>();
    tx.send(RuntimeEvent::SubagentStarted {
        tool_invocation_id,
        subagent_name: subagent_name.map(str::to_string),
        subagent_session_id,
        background,
    })
    .unwrap();
    drop(tx); // close the channel so the forwarder loop terminates

    crate::runs::tools::forward_runtime_events(
        rx,
        RunId::new(),
        parent_session_id,
        state.run_manager.clone(),
        state.approval_store.clone(),
        state.session_manager.clone(),
        context_id.to_string(),
        None,
    )
    .await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();

    shutdown_token.cancel();
    (history, tool_invocation_id, subagent_session_id)
}

/// #1125 (A1-1): a FOREGROUND `subagent_started` event on a user-facing
/// session must persist a durable lifecycle marker carrying
/// `tool_invocation_id` + `subagent_session_id` + `subagent_name`,
/// positioned AFTER the parent's `invoke_agent` tool-call row.
///
/// Falsifiable: removing the `persist_lifecycle_marker` call in the
/// `SubagentStarted` arm of `forward_runtime_events` drops the marker and
/// fails the "exactly one marker" assertion.
#[tokio::test]
async fn foreground_subagent_started_persists_marker_after_invoke_row() {
    let (history, tool_invocation_id, subagent_session_id) =
        run_subagent_started_marker_case("web", false, Some("researcher")).await;

    // Locate the marker and the parent's invoke_agent tool-call row.
    let marker_pos = history.iter().position(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    let invoke_pos = history.iter().position(|m| {
        matches!(
            &m.content,
            alms_session::Content::ToolCall { name, .. } if name == "invoke_agent"
        )
    });

    let marker_pos = marker_pos.expect("expected a subagent_started marker in parent history");
    let invoke_pos = invoke_pos.expect("expected the seeded invoke_agent tool-call row");

    // Exactly one marker (no duplicate persistence).
    let marker_count = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
            })
        })
        .count();
    assert_eq!(
        marker_count, 1,
        "expected exactly one subagent_started marker"
    );

    // ORDERING INVARIANT: marker lands AFTER the invoke_agent tool row so the
    // rehydrator resolves the chip's tool row first, then fills its session id.
    assert!(
        marker_pos > invoke_pos,
        "subagent_started marker (idx {marker_pos}) must come AFTER the \
         invoke_agent tool-call row (idx {invoke_pos})"
    );

    // Metadata shape — the keys Iris's history.js + subagents.js consume.
    let marker = &history[marker_pos];
    assert_eq!(marker.role, alms_session::Role::System);
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["synthetic"], true);
    assert_eq!(meta["type"], "subagent_started");
    assert_eq!(
        meta["tool_invocation_id"].as_str(),
        Some(tool_invocation_id.to_string().as_str()),
        "marker must carry the parent's invoke_agent tool_invocation_id (#1127 disambiguator)"
    );
    assert_eq!(
        meta["subagent_session_id"].as_str(),
        Some(subagent_session_id.0.to_string().as_str()),
        "marker must carry the subagent's session id for chip rehydration"
    );
    assert_eq!(
        meta["subagent_name"].as_str(),
        Some("researcher"),
        "marker must carry the subagent name when present"
    );

    // The marker is DM-hygiene-filtered like every other lifecycle marker.
    assert!(
        alms_tools::dm_filter::is_synthetic_marker(marker),
        "subagent_started marker must be filtered by is_synthetic_marker"
    );
}

/// #1125 (A1-1): an UNNAMED (ephemeral) foreground `subagent_started` event
/// must persist a marker that OMITS the `subagent_name` key entirely —
/// matching the live SSE event's `skip_serializing_if` so the frontend
/// rehydrator (which reads `md.subagent_name` defensively as possibly
/// undefined) never sees a `null`/empty name.
///
/// This pins the conditional `if let Some(name) = subagent_name` key insert in
/// the `SubagentStarted` arm of `forward_runtime_events` at the PERSISTED
/// MARKER level — the named case (above) exercises the `Some(..)` branch, this
/// exercises the `None` branch. Falsifiable: unconditionally writing
/// `subagent_name` (e.g. `json!(null)`) fails the `is_none()` assertion.
#[tokio::test]
async fn unnamed_foreground_subagent_started_omits_subagent_name() {
    let (history, tool_invocation_id, subagent_session_id) =
        run_subagent_started_marker_case("web", false, None).await;

    // The marker is still persisted (foreground, non-internal) — only the
    // `subagent_name` key differs from the named case.
    let marker = history
        .iter()
        .find(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
            })
        })
        .expect("expected a subagent_started marker for the unnamed foreground case");

    let meta = marker.metadata.as_ref().unwrap();

    // The id keys the frontend joins on are still present and correct — the
    // rehydrator resolves the chip by `tool_invocation_id`, not by name, so the
    // omitted name must not cost it the session id.
    assert_eq!(
        meta["tool_invocation_id"].as_str(),
        Some(tool_invocation_id.to_string().as_str()),
        "unnamed marker must still carry the tool_invocation_id join key"
    );
    assert_eq!(
        meta["subagent_session_id"].as_str(),
        Some(subagent_session_id.0.to_string().as_str()),
        "unnamed marker must still carry the subagent session id"
    );

    // CONTRACT: the `subagent_name` key is ABSENT (not present-as-null) for an
    // ephemeral/unnamed subagent — the wire shape Iris's frontend depends on.
    assert!(
        meta.get("subagent_name").is_none(),
        "subagent_name must be OMITTED from the marker for an unnamed subagent, \
         got {:?}",
        meta.get("subagent_name")
    );

    // The unnamed `display_text` branch renders without a name suffix.
    let alms_session::Content::Text(ref text) = marker.content else {
        panic!("expected text content on the subagent_started marker");
    };
    assert_eq!(
        text, "Subagent started.",
        "unnamed marker display text must omit the ' '<name>'' suffix"
    );
}

/// #1125 (A1-1): the BACKGROUND path must NOT persist a `subagent_started`
/// marker — background subagents are already reload-safe via their persisted
/// `{task_id, session_id}` tool result, so a start marker would be redundant.
#[tokio::test]
async fn background_subagent_started_persists_no_marker() {
    let (history, _inv, _sub) =
        run_subagent_started_marker_case("web", true, Some("researcher")).await;

    let has_marker = history.iter().any(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    assert!(
        !has_marker,
        "background subagent_started must NOT persist a marker (already reload-safe)"
    );
}

/// #1125 (A1-1): internal / DM contexts must NOT accrue a `subagent_started`
/// marker — gated by `!is_internal_context_id`, exactly like the
/// `run_warning` marker. A `subagent_` context id is internal.
#[tokio::test]
async fn internal_context_subagent_started_persists_no_marker() {
    // `subagent_…` is classified internal by `is_internal_context_id`.
    let (history, _inv, _sub) =
        run_subagent_started_marker_case("subagent_task-internal", false, Some("researcher")).await;

    let has_marker = history.iter().any(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("type").and_then(|v| v.as_str()) == Some("subagent_started")
        })
    });
    assert!(
        !has_marker,
        "internal/DM-context subagent_started must NOT persist a marker"
    );
}

/// Test that subagent completion for a missing parent session is
/// gracefully skipped (warn + continue), not a panic.
///
/// This exercises the defensive check at the top of
/// `completion_notification_loop` where the parent session lookup fails.
#[tokio::test]
async fn subagent_completion_with_missing_parent_session_is_skipped() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    // Use a session ID that was never created.
    let missing_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("ghost".to_string()),
            status: TaskStatus::Completed,
            summary: "This should be skipped".to_string(),
            parent_session_id: missing_session_id,
            parent_agent_id,
            subagent_session_id: SessionId::new(),
            task_description: None,
            tool_count: None,
            duration_ms: None,
            token_usage: None,
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);

    // Should complete without panic.
    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

    // No run should have been created for the missing session.
    let runs = state.run_manager.list_by_session(missing_session_id, 10);
    assert!(
        runs.is_empty(),
        "no run should be created when parent session is missing"
    );

    shutdown_token.cancel();
}

/// Test that subagent completion with rich metadata (tool_count, duration_ms,
/// token_usage) is correctly propagated into the persisted marker.
#[tokio::test]
async fn subagent_completion_marker_includes_rich_metadata() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "rich-metadata-test");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("analyzer".to_string()),
            status: TaskStatus::Failed,
            summary: "OOM after processing large dataset".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Analyze the 10GB dataset".to_string()),
            tool_count: Some(42),
            duration_ms: Some(300_000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 50_000,
                completion_tokens: 15_000,
                ..TokenUsage::default()
            }),
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(!markers.is_empty());

    let meta = markers[0].metadata.as_ref().unwrap();
    assert_eq!(meta.get("status").and_then(|v| v.as_str()), Some("fail"));
    assert_eq!(
        meta.get("task_description").and_then(|v| v.as_str()),
        Some("Analyze the 10GB dataset")
    );
    assert_eq!(meta.get("tool_count").and_then(|v| v.as_u64()), Some(42));
    assert_eq!(
        meta.get("duration_ms").and_then(|v| v.as_u64()),
        Some(300_000)
    );

    // Verify token usage is included.
    let usage = meta.get("token_usage").expect("should have token_usage");
    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(50_000)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(15_000)
    );
    // reasoning_tokens was None on the TokenUsage; the marker must omit
    // the field entirely (byte-identical to pre-#768).
    assert!(
        usage.get("reasoning_tokens").is_none(),
        "reasoning_tokens should be absent from token_usage when None"
    );

    // Verify the marker text mentions "failed".
    if let alms_session::Content::Text(ref text) = markers[0].content {
        assert!(
            text.contains("failed"),
            "marker text should mention 'failed' for a failed subagent"
        );
    }

    shutdown_token.cancel();
}

/// Test that subagent completion carries `reasoning_tokens` through to the
/// persisted marker metadata when the provider reports them separately
/// (OpenAI o-series, DeepSeek R1, xAI reasoning variants). Previously the
/// field was dropped at the marker boundary — fixed in response to Tim's
/// review on #777 (C1).
#[tokio::test]
async fn subagent_completion_marker_includes_reasoning_tokens() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "reasoning-tokens-test");
    let parent_session_id = parent_session.id;

    let subagent_session_id = SessionId::new();

    let (test_tx, test_rx) = mpsc::unbounded_channel();
    test_tx
        .send(SubagentCompletion {
            task_id: TaskId::new(),
            subagent_name: Some("reasoner".to_string()),
            status: TaskStatus::Completed,
            summary: "Deep thought complete".to_string(),
            parent_session_id,
            parent_agent_id,
            subagent_session_id,
            task_description: Some("Solve the hard problem".to_string()),
            tool_count: Some(3),
            duration_ms: Some(45_000),
            token_usage: Some(TokenUsage {
                prompt_tokens: 1_200,
                completion_tokens: 300,
                reasoning_tokens: Some(2_048),
                ..TokenUsage::default()
            }),
            parent_tool_invocation_id: None,
        })
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::completion_notification_loop(test_rx, state.clone()).await;

    let history = state
        .session_manager
        .get_history(parent_session_id)
        .unwrap();
    let markers: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("subagent_completion")
            })
        })
        .collect();
    assert!(!markers.is_empty());

    let meta = markers[0].metadata.as_ref().unwrap();
    let usage = meta.get("token_usage").expect("should have token_usage");
    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(1_200)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(300)
    );
    assert_eq!(
        usage.get("reasoning_tokens").and_then(|v| v.as_u64()),
        Some(2_048),
        "reasoning_tokens must be propagated into the subagent completion marker"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1045 — GET /sessions/{id}/messages must return the subagent's transcript
// ---------------------------------------------------------------------------

/// Regression test for #1045.
///
/// The UI's "View session" drill-down (SubagentBar / SubagentCompletionCard)
/// loads the subagent session by id via `loadSession` → `getSessionMessages`,
/// which hits this exact HTTP endpoint. If the response body's `messages`
/// array is empty or shaped wrong, the chat pane renders blank — the
/// #1045 symptom.
///
/// Coverage: dispatches a named subagent through the real `Coordinator`
/// path in an `AppState` whose LLM is mock-backed (so the agent loop
/// runs to completion without network), then invokes
/// `GET /sessions/{sub_session_id}/messages` over the production router
/// via `tower::ServiceExt::oneshot` and asserts the JSON wire shape.
///
/// What is exercised end-to-end against the mock LLM:
///   - `Role::User`     → `"user"`      with `Content::Text` → `"type": "text"`
///   - `Role::Assistant`→ `"assistant"` with `Content::Text` → `"type": "text"`
///   - the trailing `timestamp` field on each text message
///
/// What is intentionally *not* covered here (because the mock LLM emits
/// plain-text only and the dispatch path produces no synthetic markers):
///   - `Content::ToolCall` / `Content::ToolResult` shape mapping
///   - the `Role::System` synthetic-vs-internal filter
///   - the `notification_input` metadata filter
///   - the UI's `mapHistoryMessages` (in `static/ui/utils/history.js`) is
///     *not* loaded by this test; field-additive regressions there are
///     not caught — only wire-level regressions that drop or rename the
///     `role` / `type` / `content` / `timestamp` fields on text messages.
///
/// Covering the tool-call and synthetic-system paths would require an
/// integration harness that can drive a real `invoke_agent` tool call
/// (or emit a synthetic `Role::System` message into the session). For
/// the #1045 regression specifically, the user/assistant text path is
/// the load-bearing surface — that is what the UI's chat pane reads.
#[tokio::test]
async fn subagent_session_messages_endpoint_returns_transcript() {
    use alms_tools::subagent::SubagentDispatcher;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // Drive a real subagent dispatch through the Coordinator that lives
    // inside AppState. The parent_session_id and parent_agent_id are
    // synthetic — `dispatch` doesn't require either to exist anywhere;
    // they are only used as keys to derive the subagent's deterministic
    // identity (#1051 / #1068: keyed on `(parent_agent_id, name)`).
    let parent_session = SessionId::new();
    let parent_agent_id = AgentId::new();
    let task: &str = "Investigate topic X";
    let (response, sub_session_id) = state
        .coordinator
        .dispatch(
            task.to_string(),
            parent_session,
            parent_agent_id,
            None,
            None,
            Some("researcher".to_string()),
            None,
            None,
        )
        .await
        .expect("subagent dispatch should succeed against mock LLM");
    // The mock LLM at crates/alms-runtime/src/llm_client/mod.rs:577 always
    // prepends `[mock] ` to the assistant reply. If `dispatch` returns Ok
    // with an empty / garbled string, the wire-shape assertions below would
    // still pass (an empty assistant message would just not match) but the
    // failure mode would be opaque. This guards the upstream contract.
    assert!(
        response.contains("mock"),
        "dispatch returned a response that does not contain the mock marker; \
         got {response:?}"
    );

    // Build the production router (with state) and issue the exact GET
    // the UI fires when `loadSession` hits `getSessionMessages`.
    // Note: `protected_router()` here omits the `require_auth` middleware
    // that `server::mod::serve_with_gateway` layers on top in production
    // (crates/alms-gateway/src/server/mod.rs:171-178) — consistent with
    // the rest of this test file, which doesn't carry bearer tokens.
    let router = crate::server::routes::protected_router().with_state(state);
    let uri = format!("/sessions/{}/messages", sub_session_id.0);
    let http_response = router
        .oneshot(HttpRequest::get(&uri).body(Body::empty()).unwrap())
        .await
        .expect("router oneshot should not fail");
    assert_eq!(
        http_response.status(),
        axum::http::StatusCode::OK,
        "expected 200 OK from {uri}, got {}",
        http_response.status()
    );

    let body_bytes = axum::body::to_bytes(http_response.into_body(), 1024 * 1024)
        .await
        .expect("read response body");
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("response body should be JSON");

    let messages = json["messages"]
        .as_array()
        .expect("response should carry a `messages` array");
    assert!(
        !messages.is_empty(),
        "GET /sessions/{sub_session_id}/messages must return a non-empty \
         `messages` array for a subagent session that has run — \
         empty array is the #1045 symptom. Body: {json}",
        sub_session_id = sub_session_id.0
    );

    // Shape assertions match the UI's `mapHistoryMessages` expectations
    // (crates/alms-gateway/static/ui/utils/history.js): user task as a
    // text message under `role: "user"`, assistant reply as a text
    // message under `role: "assistant"`. Both must carry non-empty
    // `content` so the chat pane has something to render.
    let user_msg = messages
        .iter()
        .find(|m| {
            m["role"] == "user"
                && m["type"] == "text"
                && m["content"].as_str().is_some_and(|c| c.contains(task))
        })
        .unwrap_or_else(|| {
            panic!("expected a user text message containing the task; got {messages:?}")
        });

    let assistant_msg = messages
        .iter()
        .find(|m| {
            m["role"] == "assistant"
                && m["type"] == "text"
                && m["content"].as_str().is_some_and(|c| !c.is_empty())
        })
        .unwrap_or_else(|| panic!("expected a non-empty assistant text reply; got {messages:?}"));

    // Each text message must carry a `timestamp` field. This is what
    // `mapHistoryMessages` (and the chat-pane ordering logic) consume —
    // dropping or renaming it would resurface a #1045-flavored bug.
    for (label, msg) in [("user", user_msg), ("assistant", assistant_msg)] {
        assert!(
            msg["timestamp"].is_string(),
            "{label} message must carry a string `timestamp` field; got {msg:?}"
        );
    }

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// POST /sessions/{id}/subagent/cancel — session-keyed subagent cancel
// ---------------------------------------------------------------------------

/// The 404 leg over the PRODUCTION router: a session with no live subagent
/// must return 404 with the `NO_LIVE_SUBAGENT` error code. Exercising this
/// through `protected_router()` (rather than calling the handler directly)
/// pins the route registration — `/sessions/{session_id}/subagent/cancel`
/// wired to `cancel_subagent` — so the frontend's `cancelSubagent(sessionId)`
/// helper can't silently 404-on-every-request due to a routing drift.
#[tokio::test]
async fn cancel_subagent_endpoint_404_when_no_live_subagent() {
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    let router = crate::server::routes::protected_router().with_state(state);
    let uri = format!("/sessions/{}/subagent/cancel", SessionId::new().0);
    let response = router
        .oneshot(HttpRequest::post(&uri).body(Body::empty()).unwrap())
        .await
        .expect("router oneshot should not fail");

    assert_eq!(
        response.status(),
        axum::http::StatusCode::NOT_FOUND,
        "a session with no live subagent must 404"
    );
    let body_bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read response body");
    let json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("error body should be JSON");
    assert_eq!(
        json["error"]["code"], "NO_LIVE_SUBAGENT",
        "error code must be NO_LIVE_SUBAGENT; got {json}"
    );

    shutdown_token.cancel();
}

/// The 200 leg: cancelling a LIVE subagent through the handler fires its
/// cancellation token and the subagent actually terminates `Cancelled`.
///
/// Determinism note: `#[tokio::test]` uses the current-thread runtime, and
/// the handler body has no await points before the (synchronous)
/// `cancel_subagent_by_session` call — so the `tokio::spawn`ed
/// `run_subagent` task cannot run to completion between `spawn_subagent`
/// returning and the cancel firing. The handle is still live (Pending) at
/// cancel time, every time. The subagent then observes the already-fired
/// token at its first cancellation checkpoint and lands `Cancelled`, which
/// the awaited `TaskResult` asserts end-to-end.
#[tokio::test]
async fn cancel_subagent_endpoint_cancels_live_subagent() {
    use axum::extract::{Path as AxumPath, State as AxumState};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // Spawn a background subagent directly on the coordinator (the same
    // object the endpoint reaches through `state.coordinator`). Unnamed, so
    // no registry entry is needed.
    let request = alms_coordinator::SubagentRequest {
        task: "long-running background research".to_string(),
        parent_session: SessionId::new(),
        parent_agent_id: AgentId::new(),
        parent_run_id: None,
        subagent_name: None,
        parent_tool_invocation_id: None,
    };
    let (task_id, sub_session_id) = state
        .coordinator
        .spawn_subagent(request, None, true, None)
        .await
        .expect("spawn_subagent should succeed");
    let result_rx = state
        .coordinator
        .take_result_rx(task_id)
        .expect("result receiver should be available");

    // Cancel through the HTTP handler (session-keyed).
    let response =
        crate::runs::lifecycle::cancel_subagent(AxumState(state.clone()), AxumPath(sub_session_id))
            .await
            .expect("cancel of a live subagent must return 200");
    assert_eq!(
        response.0["status"], "cancelling",
        "the 200 body must report status=cancelling"
    );

    // The subagent must actually terminate Cancelled (not Completed): the
    // token was fired before its task ever ran, so its first checkpoint
    // takes the cancellation arm.
    let task_result = result_rx.await.expect("should receive a task result");
    assert_eq!(
        task_result.status,
        TaskStatus::Cancelled,
        "the cancelled subagent must land TaskStatus::Cancelled; got {:?}",
        task_result.status
    );

    // Idempotence / double-click: the subagent is now terminal, so a second
    // cancel must report 404 (no live subagent), not 200.
    let second =
        crate::runs::lifecycle::cancel_subagent(AxumState(state.clone()), AxumPath(sub_session_id))
            .await;
    assert!(
        second.is_err(),
        "a second cancel after termination must be an error (404)"
    );
    assert_eq!(
        second.unwrap_err().0,
        axum::http::StatusCode::NOT_FOUND,
        "the second cancel must be a 404"
    );

    shutdown_token.cancel();
}

/// #1254 regression — cancelling a parent must put a terminal SSE event on
/// the in-flight subagent's OWN session, not merely a `cancelled` row in the
/// database.
///
/// This is the acceptance case the issue flagged as uncovered: the parent's
/// cancellation token propagates to a FOREGROUND subagent (in the reported
/// session, subagent `d19b6d62` went terminal 1ms after its parent), and the
/// fullscreen subagent-session view has to learn about it from the SSE feed.
///
/// The assertion is deliberately on the BROADCAST, not on `TaskStatus` —
/// the persisted status was already correct in production, and that is
/// precisely what hid the reported symptom for so long.
#[tokio::test]
async fn cancelled_subagent_emits_terminal_sse_on_its_own_session() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();

    // The parent's cancellation token — the same object the HTTP
    // `cancel_run` handler fires via `RunManager::cancel_run`.
    let parent_cancel_token = CancellationToken::new();

    let request = alms_coordinator::SubagentRequest {
        task: "long-running foreground research".to_string(),
        parent_session: SessionId::new(),
        parent_agent_id: AgentId::new(),
        parent_run_id: None,
        subagent_name: None,
        parent_tool_invocation_id: None,
    };
    // `is_background = false` — the foreground `invoke_agent` shape.
    let (task_id, sub_session_id) = state
        .coordinator
        .spawn_subagent(request, None, false, Some(parent_cancel_token.clone()))
        .await
        .expect("spawn_subagent should succeed");
    let result_rx = state
        .coordinator
        .take_result_rx(task_id)
        .expect("result receiver should be available");

    // Subscribe BEFORE the cancel so this is a live-delivery assertion and
    // not just a replay of the persisted session log.
    let mut session_subscription = state.run_manager.subscribe_session(sub_session_id);

    // Cancel the PARENT. Propagation through the child token is what drives
    // the subagent terminal — nothing cancels the subagent directly.
    parent_cancel_token.cancel();

    let task_result = result_rx.await.expect("should receive a task result");
    assert_eq!(
        task_result.status,
        TaskStatus::Cancelled,
        "the subagent must land Cancelled via parent-token propagation; got {:?}",
        task_result.status
    );

    // Await the terminal ON THE SUBSCRIPTION, not on `session_events_from`.
    // The self-sink's drain task orders the terminal event after all of the
    // subagent's buffered content, so it lands asynchronously — `result_rx`
    // resolving does not mean it has already been broadcast.
    //
    // Reading the persisted log instead would be the wrong surface: #1254's
    // symptom was "the client never received it", and `send_event` writes
    // the log and fans out in the same call, so a log-based assertion still
    // passes with `fan_out_to` broken or with the terminal rerouted through
    // the log-skipping `send_transient_session_event`. Draining the channel
    // is what actually pins delivery — and it needs no sleep, because
    // `recv()` wakes on the send.
    let mut delivered: Vec<String> = Vec::new();
    let saw_terminal = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = session_subscription.recv().await {
            let event_type = event.event_type.clone();
            delivered.push(event_type.clone());
            if event_type == "run_cancelled" {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_terminal,
        "the cancelled subagent's own session must receive `run_cancelled` \
         on its live SSE stream (#1254); delivered: {delivered:?}"
    );

    // Whatever is already queued behind the terminal — a second broadcast
    // from a racing path would be sitting right here.
    delivered.extend(
        drain_events(&mut session_subscription)
            .into_iter()
            .map(|event| event.event_type),
    );
    let cancelled_count = delivered
        .iter()
        .filter(|kind| kind.as_str() == "run_cancelled")
        .count();
    assert_eq!(
        cancelled_count, 1,
        "the cancelled subagent's own session must receive EXACTLY one \
         `run_cancelled` SSE event (#1254); delivered: {delivered:?}"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// 12. Subagent status snapshot on session-stream reattach (#1189 follow-up)
// ---------------------------------------------------------------------------

/// Build an `AppState` whose LLM streams ONE OpenAI-format content chunk and
/// then holds the connection open without finishing the response.
///
/// This freezes a real subagent run in its "actively writing" state: the
/// runtime received a `token_delta` (so the coordinator relay emitted — and
/// recorded — a `writing` activity signal) but the run cannot complete until
/// the generous `stream_chunk_timeout_secs` fires, long after the test ends.
/// That is exactly the live condition under which the "chip stuck on
/// Starting…" bug was reproduced.
async fn test_app_state_with_streaming_then_stalling_llm() -> AppStateWithChannels {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                // One OpenAI-compatible SSE chunk with visible content, then
                // hold the socket (no finish_reason, no EOF): the client's
                // stream stays "in flight" for the rest of the test.
                let chunk = concat!(
                    "data: {\"id\":\"stall\",\"object\":\"chat.completion.chunk\",",
                    "\"created\":0,\"model\":\"m\",\"choices\":[{\"index\":0,",
                    "\"delta\":{\"role\":\"assistant\",\"content\":\"partial answer \"},",
                    "\"finish_reason\":null}]}\n\n"
                );
                let response =
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{chunk}");
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
                // Park until the client goes away.
                std::future::pending::<()>().await;
            });
        }
    });

    let llm_config = alms_runtime::LlmConfig {
        base_url,
        api_key: "fake-key-for-test".to_string(),
        // Generous timeouts: the run must still be mid-stream ("writing")
        // when the test reattaches — nothing here should fire during the
        // test window.
        timeout_secs: 60,
        stream_chunk_timeout_secs: 60,
        ..alms_runtime::LlmConfig::default()
    };
    TestAppState::new()
        .llm_config(llm_config)
        .build_with_channels()
}

/// REGRESSION (#1189 follow-up): a session-stream subscriber that attaches
/// while a background subagent is mid-phase must immediately learn the
/// subagent's CURRENT activity.
///
/// The live `subagent_activity` signal is ephemeral (never persisted or
/// replayed) and deduplicated at the source to ONE emission per activity
/// transition, so it only reaches the subscribers attached at that instant.
/// Before the fix, any client that (re)attached afterwards — page reload,
/// session switch back from the subagent view, a second tab, an SSE
/// reconnect — rendered the chip as "Starting…" until the subagent's NEXT
/// kind transition, even while it was actively writing. This test drives the
/// REAL end-to-end path: a real background subagent (spawned through the
/// coordinator) makes a real streaming LLM call that delivers one token and
/// stalls, the REAL relay emits + records the `writing` signal through the
/// production bg-event forwarder/drain, and then a NEW subscriber attaches
/// through the same `attach_session_stream` path the
/// `GET /sessions/{id}/events` endpoint uses.
///
/// With the snapshot replay removed (the pre-fix behaviour), the reattached
/// subscriber receives nothing and the final assertions fail.
#[tokio::test]
async fn reattached_session_stream_receives_subagent_activity_snapshot() {
    let (state, shutdown_token, _cr, _tr, _dr) =
        test_app_state_with_streaming_then_stalling_llm().await;

    let parent_agent_id = AgentId::new();
    let parent_session = state
        .session_manager
        .get_or_create(parent_agent_id, "parent-chat");
    let parent_session_id = parent_session.id;

    // Subscriber attached BEFORE the subagent starts — the control group.
    // Uses the same attach path; the snapshot is empty at this point (no
    // subagents yet), so it receives only genuinely live events.
    let mut early_rx = crate::runs::streaming::attach_session_stream(&state, parent_session_id);

    // Production-shaped background event leg: the coordinator relay forwards
    // status signals into a `RuntimeEventForwarder`, and a drain task routes
    // them via `route_bg_event` onto the parent's session stream — mirroring
    // the wiring `execute_run` installs for `invoke_agent` (parent-dead leg,
    // which is the steady state for a long-lived background subagent).
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<alms_runtime::RuntimeEvent>();
    let bg_fwd: Arc<dyn alms_tools::EventForwarder> =
        Arc::new(crate::runs::tools::RuntimeEventForwarder::new(bg_tx));
    let bg_run_id = RunId::new();
    let drain_state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = bg_rx.recv().await {
            match crate::runs::tools::route_bg_event(event, None, bg_run_id, parent_session_id) {
                Some(crate::runs::tools::RoutedBgEvent::Persist(sse)) => {
                    drain_state
                        .run_manager
                        .send_session_event(parent_session_id, bg_run_id, sse)
                        .await;
                }
                Some(crate::runs::tools::RoutedBgEvent::Transient(sse)) => {
                    drain_state
                        .run_manager
                        .send_transient_session_event(parent_session_id, sse);
                }
                None => {}
            }
        }
    });

    // REAL background dispatch: real subagent runtime, real (stalling)
    // streaming LLM call, real relay. `parent_inv` is the parent's
    // invoke_agent tool-invocation-id — the chip-resolution correlator the
    // UI matches identity-exactly (#1190 Codex P2).
    let parent_inv = uuid::Uuid::new_v4();
    let (task_uuid, _sub_session_id) = state
        .coordinator
        .dispatch_background(
            "Investigate the flaky test".to_string(),
            parent_session_id,
            parent_agent_id,
            None,
            Some(bg_fwd),
            None,
            None,
            Some(parent_inv),
        )
        .await
        .expect("dispatch_background should succeed");

    // Wait until the subagent is observably WRITING: the stub delivered its
    // one token, the relay emitted the (single, deduplicated) live signal,
    // and the recording landed on the handle.
    let mut attempts = 0;
    while state
        .coordinator
        .subagent_activity_snapshot(parent_session_id)
        .is_empty()
    {
        attempts += 1;
        assert!(
            attempts < 200,
            "subagent never reached the 'writing' state — streaming stub broken?"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // The attached-at-emission subscriber got the live signal (this leg
    // always worked — it is what the pre-fix tests modelled).
    let early_events = drain_events(&mut early_rx);
    let expected_label = format!("subagent-{}", &task_uuid.to_string()[..8]);
    assert!(
        early_events
            .iter()
            .any(|e| e.event_type == "subagent_activity"
                && e.data.get("kind").and_then(|v| v.as_str()) == Some("writing")
                && e.data.get("source_agent").and_then(|v| v.as_str())
                    == Some(expected_label.as_str())),
        "subscriber attached before the transition must receive the live \
         `writing` signal, got: {:?}",
        early_events
            .iter()
            .map(|e| (&e.event_type, &e.data))
            .collect::<Vec<_>>()
    );

    let hwm_before = state
        .run_manager
        .latest_session_event_id(parent_session_id)
        .await;

    // THE REGRESSION: a subscriber attaching AFTER the (once-per-transition,
    // ephemeral) signal fired. The subagent is still actively writing — the
    // reattached client must be told so instead of showing "Starting…".
    let mut reattached_rx =
        crate::runs::streaming::attach_session_stream(&state, parent_session_id);
    let snapshot_event = reattached_rx
        .try_recv()
        .expect("reattached session stream must immediately receive the subagent status snapshot");
    assert_eq!(snapshot_event.event_type, "subagent_activity");
    assert_eq!(
        snapshot_event.data.get("kind").and_then(|v| v.as_str()),
        Some("writing"),
        "the snapshot must carry the subagent's CURRENT activity kind"
    );
    assert_eq!(
        snapshot_event
            .data
            .get("source_agent")
            .and_then(|v| v.as_str()),
        Some(expected_label.as_str()),
        "the snapshot must be tagged with the same label as the live signals \
         so it resolves to the same status-bar chip"
    );
    assert_eq!(
        snapshot_event
            .data
            .get("parent_tool_invocation_id")
            .and_then(|v| v.as_str()),
        Some(parent_inv.to_string().as_str()),
        "the snapshot must carry the parent invoke_agent correlator so the \
         UI resolves the chip identity-exactly — with concurrent unnamed \
         subagents, the label alone first-matches onto the WRONG chip \
         (#1190 Codex P2)"
    );

    // Snapshot events keep the live signal's ephemerality contract: no
    // event id (passes the replay dedup filter) and nothing persisted.
    assert!(
        snapshot_event.event_id.is_none(),
        "snapshot subagent_activity must not carry a persisted event id"
    );
    assert_eq!(
        state
            .run_manager
            .latest_session_event_id(parent_session_id)
            .await,
        hwm_before,
        "replaying the snapshot must not write the session event log"
    );

    shutdown_token.cancel();
}
