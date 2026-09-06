// SPDX-License-Identifier: Apache-2.0

//! Cancellation: during tool execution, during shutdown, before execution, and the #1046 authoritative HTTP cancel.

use super::{
    create_recurring_job, drain_events, subscribe_session, test_app_state,
    test_app_state_with_hanging_llm, test_app_state_with_mock_llm,
};
use crate::sse::SseEventData;
use alms_core::{AgentId, Run, RunId, RunStatus, SessionId, TokenUsage};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 1. Cancelled-during-tool execution (tokio::select! path)
// ---------------------------------------------------------------------------

/// Test that cancelling a run before it starts (queued-then-cancelled) results
/// in a `run_cancelled` SSE event and the run record transitioning to
/// `Cancelled` status.
///
/// This exercises the early-exit path at the top of `execute_run()` where
/// `cancel_token.is_cancelled()` is checked before the runtime is created.
/// This is the tokio::select! cancellation path that was identified as
/// under-tested in #629.
#[tokio::test]
async fn cancelled_before_execution_emits_cancelled_event() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state.session_manager.get_or_create(agent_id, "test-cancel");
    let session_id = session.id;

    // Create a run and insert it.
    let run = Run::new(session_id, agent_id, "test input".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .session_manager
        .append_message(
            session_id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text(run.input.clone()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "pending_input": true,
                    "run_id": run_id.0.to_string(),
                })),
            },
        )
        .unwrap();

    // Register a cancellation token and cancel it BEFORE execution.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    // Subscribe to session events to capture the cancelled event.
    let mut rx = subscribe_session(&state, session_id);

    // Execute the run -- it should detect the pre-cancelled token and
    // short-circuit without creating a runtime.
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "test-cancel".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: true,
            dm_ended_peer: None,
        },
    )
    .await;

    // Verify the run status is Cancelled.
    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status(),
        RunStatus::Cancelled,
        "pre-cancelled run should transition to Cancelled status"
    );

    // Verify that a run_cancelled SSE event was emitted.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected a run_cancelled SSE event; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    let history = state.session_manager.get_history(session_id).unwrap();
    let run_id_string = run_id.0.to_string();
    let claimed_input = history
        .iter()
        .find(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("run_id"))
                .and_then(serde_json::Value::as_str)
                == Some(run_id_string.as_str())
        })
        .expect("pre-cancelled accepted prompt must remain durable");
    let metadata = claimed_input.metadata.as_ref().unwrap();
    assert_eq!(metadata["pending_input"], false);
    assert!(metadata["input_claimed_at"].is_string());
    assert!(
        state
            .session_manager
            .get_context_history(session_id)
            .unwrap()
            .iter()
            .any(|message| matches!(
                &message.content,
                alms_session::Content::Text(text) if text == "test input"
            )),
        "a pre-cancelled accepted prompt must remain context-visible"
    );

    shutdown_token.cancel();
}

#[tokio::test]
async fn cancellation_between_precheck_and_start_transition_cleans_up_without_starting() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancel-during-start");
    let session_id = session.id;
    let job_id = alms_core::JobId::new();
    let run = Run::for_job(
        session_id,
        agent_id,
        "must never reach the agent loop".into(),
        job_id,
    );
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .job_episodes
        .open(job_id, session_id, agent_id, run_id);

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);

    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, _decision_rx) = tokio::sync::oneshot::channel();
    state
        .approval_store
        .insert(crate::approvals::PendingApproval {
            approval_id,
            run_id,
            tool: "test".to_string(),
            params: serde_json::json!({}),
            requested_at: chrono::Utc::now(),
            decision_tx,
        });

    let barrier = crate::runs::lifecycle::install_start_transition_barrier(run_id);
    let execute_state = state.clone();
    let execute_cancel_token = cancel_token.clone();
    let handle = tokio::spawn(async move {
        crate::runs::lifecycle::execute_run(
            execute_state,
            crate::runs::RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id: "test-cancel-during-start".to_string(),
                cancel_token: execute_cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    barrier.wait().await;
    cancel_token.cancel();
    assert!(state.run_manager.mark_run_as_cancelled(run_id));
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("execute_run should leave the rejected-start branch")
        .expect("execute_run task should not panic");

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Cancelled);
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(!event_types.iter().any(|kind| kind == "run_started"));
    assert!(!event_types.iter().any(|kind| kind == "run_finished"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));
    assert!(
        state
            .approval_store
            .list_pending()
            .iter()
            .all(|approval| approval.run_id != run_id)
    );
    assert!(state.job_episodes.snapshot(job_id).is_none());
    shutdown_token.cancel();
}

#[tokio::test]
async fn pre_persisted_input_survives_start_persistence_failure() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-start-persistence-failure");
    let run = Run::new(session.id, agent_id, "must not start".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    state
        .session_manager
        .append_message(
            session.id,
            alms_session::Message {
                id: uuid::Uuid::new_v4().to_string(),
                role: alms_session::Role::User,
                content: alms_session::Content::Text(run.input.clone()),
                timestamp: alms_core::Timestamp::now(),
                metadata: Some(serde_json::json!({
                    "pending_input": true,
                    "run_id": run_id.0.to_string(),
                })),
            },
        )
        .unwrap();
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);
    state.run_manager.inject_next_persistence_failure();

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id: session.id,
            agent_id,
            input: run.input,
            context_id: "test-start-persistence-failure".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: true,
            dm_ended_peer: None,
        },
    )
    .await;

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(run.terminal_reason(), Some("persistence_failed"));
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(event_types.iter().any(|kind| kind == "run_error"));
    assert!(!event_types.iter().any(|kind| kind == "run_started"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));

    let history = state.session_manager.get_history(session.id).unwrap();
    let run_id_string = run_id.0.to_string();
    let claimed_input = history
        .iter()
        .find(|message| {
            message
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("run_id"))
                .and_then(serde_json::Value::as_str)
                == Some(run_id_string.as_str())
        })
        .expect("accepted prompt must remain in session history");
    let metadata = claimed_input.metadata.as_ref().unwrap();
    assert_eq!(metadata["pending_input"], false);
    assert!(metadata["input_claimed_at"].as_str().is_some());
    assert!(
        state
            .session_manager
            .get_context_history(session.id)
            .unwrap()
            .iter()
            .any(|message| matches!(
                &message.content,
                alms_session::Content::Text(text) if text == "must not start"
            ))
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn terminal_persistence_failure_is_reported_and_quarantined() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-terminal-persistence-failure");
    let run = Run::new(session.id, agent_id, "complete in mock mode".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let _run_subscription = state.run_manager.subscribe_run(run_id);
    let barrier = crate::runs::lifecycle::install_terminal_transition_barrier(run_id);
    let execute_state = state.clone();
    let handle = tokio::spawn(async move {
        crate::runs::lifecycle::execute_run(
            execute_state,
            crate::runs::RunParams {
                run_id,
                session_id: session.id,
                agent_id,
                input: run.input,
                context_id: "test-terminal-persistence-failure".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    barrier.wait().await;
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Running
    );
    state.run_manager.inject_next_persistence_failure();
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("execute_run should leave the terminal transition")
        .expect("execute_run task should not panic");

    let run = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(run.status(), RunStatus::Failed);
    assert_eq!(run.terminal_reason(), Some("persistence_failed"));
    let event_types: Vec<_> = state
        .run_manager
        .events_from(run_id, 0)
        .await
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert!(event_types.iter().any(|kind| kind == "run_started"));
    assert!(event_types.iter().any(|kind| kind == "run_error"));
    assert!(!event_types.iter().any(|kind| kind == "run_finished"));
    assert!(!state.run_manager.cancel_run(run_id));
    assert!(!state.run_manager.event_senders.contains_key(&run_id));
    shutdown_token.cancel();
}

#[tokio::test]
async fn create_run_registration_failure_aborts_before_message_or_queue_side_effects() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State, http::StatusCode};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-registration-persistence-failure");
    state.run_manager.inject_next_persistence_failure();

    let request = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: "must not be admitted".to_string(),
        },
    };
    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(request)).await
    else {
        panic!("registration persistence failure must reject POST /runs");
    };

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"]["code"], "LIFECYCLE_PERSISTENCE_FAILED");
    assert!(state.run_manager.runs.is_empty());
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "the user message must not be persisted after registration fails"
    );
    assert_eq!(state.agent_queue.pending_count(&agent_id), 0);
    shutdown_token.cancel();
}

#[tokio::test]
async fn concurrent_same_session_admissions_preserve_durable_and_live_order() {
    use alms_core::{CreateRunRequest, RunInput};
    use alms_session::{Content, Role};
    use axum::{Json, extract::State, http::StatusCode};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-concurrent-admission-order");
    let barrier = crate::runs::lifecycle::install_admission_persistence_barrier(session.id);

    let first_state = state.clone();
    let first_session_id = session.id;
    let first = tokio::spawn(async move {
        crate::runs::lifecycle::create_run(
            State(first_state),
            Json(CreateRunRequest {
                session_id: first_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "first admission".to_string(),
                },
            }),
        )
        .await
    });

    barrier.wait().await;
    let sqlite = state
        .session_manager
        .store()
        .expect("SQLite-backed test state");
    assert_eq!(sqlite.load_messages(session.id).unwrap().len(), 1);

    let second_state = state.clone();
    let second_session_id = session.id;
    let mut second = tokio::spawn(async move {
        crate::runs::lifecycle::create_run(
            State(second_state),
            Json(CreateRunRequest {
                session_id: second_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "second admission".to_string(),
                },
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "the second same-session admission must wait for the first projection"
    );
    assert_eq!(
        sqlite.load_messages(session.id).unwrap().len(),
        1,
        "the second durable commit must wait behind the session gate"
    );

    barrier.wait().await;
    let (first_status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), first)
        .await
        .expect("first admission timed out")
        .expect("first admission task panicked")
        .expect("first admission failed");
    let (second_status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("second admission timed out")
        .expect("second admission task panicked")
        .expect("second admission failed");
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);

    let user_texts = |messages: Vec<alms_session::Message>| {
        messages
            .into_iter()
            .filter_map(|message| match (message.role, message.content) {
                (Role::User, Content::Text(text))
                    if message
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("run_id"))
                        .and_then(serde_json::Value::as_str)
                        .is_some() =>
                {
                    Some(text)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![
        "first admission".to_string(),
        "second admission".to_string(),
    ];
    assert_eq!(
        user_texts(sqlite.load_messages(session.id).unwrap()),
        expected
    );
    assert_eq!(
        user_texts(state.session_manager.get_history(session.id).unwrap()),
        expected
    );
    assert!(
        state.run_admission_gates.is_empty(),
        "idle admission gates must remove their weak registry entry"
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn queued_later_prompt_is_not_visible_to_the_first_run() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-run-context-boundary");
    state.agent_config.write().debug_mode = true;
    let mut session_events = subscribe_session(&state, session.id);
    let execution_barrier = crate::runs::lifecycle::install_admission_execution_barrier(session.id);

    let create = |text: &str| CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text {
            text: text.to_string(),
        },
    };
    let (_, first) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(create("first prompt")))
            .await
            .expect("first admission failed");
    let (_, second) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(create("second prompt")))
            .await
            .expect("second admission failed");

    let before_claim = state
        .session_manager
        .get_context_history(session.id)
        .unwrap();
    assert!(
        before_claim.is_empty(),
        "neither queued prompt is eligible before the first run claims its input"
    );
    execution_barrier.wait().await;
    execution_barrier.wait().await;

    for _ in 0..500 {
        let both_terminal = [first.0.run_id, second.0.run_id].into_iter().all(|run_id| {
            state.run_manager.get_run(run_id).is_some_and(|run| {
                matches!(
                    run.status(),
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                )
            })
        });
        if both_terminal {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let first_run = state
        .run_manager
        .get_run(first.0.run_id)
        .expect("first run disappeared");
    let second_run = state
        .run_manager
        .get_run(second.0.run_id)
        .expect("second run disappeared");
    assert_eq!(first_run.status(), RunStatus::Completed);
    assert_eq!(second_run.status(), RunStatus::Completed);
    assert_eq!(first_run.output.as_deref(), Some("[mock] first prompt"));
    assert_eq!(second_run.output.as_deref(), Some("[mock] second prompt"));

    let events = drain_events(&mut session_events);
    let context_messages = |run_id: RunId| {
        events
            .iter()
            .find(|event| {
                event.event_type == "context_debug" && event.data["run_id"] == run_id.0.to_string()
            })
            .and_then(|event| event.data["messages"].as_array())
            .cloned()
            .unwrap_or_else(|| panic!("missing context_debug for run {run_id:?}"))
    };
    let text_turns = |messages: Vec<serde_json::Value>| {
        messages
            .into_iter()
            .filter_map(|message| {
                let role = message["role"].as_str()?;
                if role == "system" {
                    return None;
                }
                Some((role.to_string(), message["content"].as_str()?.to_string()))
            })
            .collect::<Vec<_>>()
    };
    let first_turns = text_turns(context_messages(first.0.run_id));
    assert_eq!(
        first_turns,
        vec![("user".to_string(), "first prompt".to_string())]
    );
    assert!(
        first_turns
            .iter()
            .all(|(_, content)| !content.contains("second prompt")),
        "the actual first LLM context must not contain the later prompt"
    );
    assert_eq!(
        text_turns(context_messages(second.0.run_id)),
        vec![
            ("user".to_string(), "first prompt".to_string()),
            ("assistant".to_string(), "[mock] first prompt".to_string()),
            ("user".to_string(), "second prompt".to_string()),
        ]
    );
    shutdown_token.cancel();
}

#[tokio::test]
async fn cancelled_create_request_keeps_event_order_and_gate_ownership() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::{Json, extract::State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancelled-admission-publication");
    let event_barrier = crate::runs::lifecycle::install_admission_event_barrier(session.id);

    let first_state = state.clone();
    let first_session_id = session.id;
    let first = tokio::spawn(async move {
        crate::runs::lifecycle::create_run(
            State(first_state),
            Json(CreateRunRequest {
                session_id: first_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "first publication".to_string(),
                },
            }),
        )
        .await
    });
    event_barrier.wait().await;
    let first_run_id = state.run_manager.list_by_session(session.id, 10)[0].run_id;
    first.abort();
    let _ = first.await;

    let second_state = state.clone();
    let second_session_id = session.id;
    let mut second = tokio::spawn(async move {
        crate::runs::lifecycle::create_run(
            State(second_state),
            Json(CreateRunRequest {
                session_id: second_session_id,
                agent_id: Some(agent_id),
                input: RunInput::Text {
                    text: "second publication".to_string(),
                },
            }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "the second admission must remain behind the detached first publisher"
    );
    assert!(
        state
            .run_manager
            .session_events_from(session.id, 0)
            .await
            .iter()
            .all(|event| event.event_type != "run_created")
    );

    event_barrier.wait().await;
    let (_, second_response) = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("second admission timed out")
        .expect("second admission task panicked")
        .expect("second admission failed");
    let created_run_ids = state
        .run_manager
        .session_events_from(session.id, 0)
        .await
        .into_iter()
        .filter(|event| event.event_type == "run_created")
        .filter_map(|event| {
            event
                .data
                .get("run_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .map(RunId)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        created_run_ids,
        vec![first_run_id, second_response.0.run_id]
    );
    assert!(state.run_admission_gates.is_empty());
    shutdown_token.cancel();
}

#[tokio::test]
async fn admission_gate_registry_does_not_retain_idle_sessions() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    for _ in 0..100 {
        let guard = crate::runs::lifecycle::acquire_run_admission_guard(
            &state.run_admission_gates,
            SessionId::new(),
        )
        .await;
        drop(guard);
    }
    assert!(state.run_admission_gates.is_empty());
    shutdown_token.cancel();
}

#[tokio::test]
async fn simultaneous_final_admission_lease_drops_remove_registry_entry() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    for _ in 0..100 {
        let session_id = SessionId::new();
        let owner = crate::runs::lifecycle::acquire_run_admission_guard(
            &state.run_admission_gates,
            session_id,
        )
        .await;
        let acquire_barrier = crate::runs::lifecycle::install_admission_acquire_barrier(session_id);
        let gates = state.run_admission_gates.clone();
        let waiter = tokio::spawn(async move {
            crate::runs::lifecycle::acquire_run_admission_guard(&gates, session_id).await
        });

        acquire_barrier.wait().await;
        let release = Arc::new(tokio::sync::Barrier::new(3));
        let drop_release = release.clone();
        let drop_task = tokio::spawn(async move {
            drop_release.wait().await;
            drop(owner);
        });
        let abort_release = release.clone();
        let abort_waiter = waiter.abort_handle();
        let abort_task = tokio::spawn(async move {
            abort_release.wait().await;
            abort_waiter.abort();
        });

        release.wait().await;
        let _ = tokio::join!(drop_task, abort_task);
        let _ = waiter.await;
        assert!(
            state.run_admission_gates.is_empty(),
            "simultaneous final lease drops must remove the registry entry"
        );
    }
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_job_session_cannot_gain_an_orphan_scheduled_run() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job = state
        .job_store
        .create(alms_core::CreateJobRequest {
            agent_id,
            prompt: "scheduled work".to_string(),
            schedule: alms_core::JobSchedule::Once {
                run_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        })
        .unwrap();
    let context_id = format!("job_{}", job.id.0);
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let owner =
        crate::runs::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session.id)
            .await;
    let acquire_barrier = crate::runs::lifecycle::install_admission_acquire_barrier(session.id);
    let fire_state = state.clone();
    let fire =
        tokio::spawn(
            async move { crate::runs::notifications::fire_job_run(fire_state, job.id).await },
        );

    acquire_barrier.wait().await;
    state.session_manager.delete(agent_id, &context_id).unwrap();
    acquire_barrier.wait().await;
    drop(owner);

    let result = fire.await.expect("scheduled producer panicked");
    assert!(result.is_err(), "deleted target must reject registration");
    assert!(state.run_manager.list_by_session(session.id, 10).is_empty());
    assert!(!state.session_manager.has_session(&(agent_id, context_id)));
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_session_cannot_gain_an_orphan_triggered_run() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let context_id = "deleted-trigger-target".to_string();
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let session_id = session.id;
    let owner =
        crate::runs::lifecycle::acquire_run_admission_guard(&state.run_admission_gates, session_id)
            .await;
    let acquire_barrier = crate::runs::lifecycle::install_admission_acquire_barrier(session_id);
    let trigger_state = state.clone();
    let trigger_context = context_id.clone();
    let trigger = tokio::spawn(async move {
        crate::runs::notifications::enqueue_triggered_run(
            &trigger_state,
            agent_id,
            session_id,
            "late notification".to_string(),
            trigger_context,
            "test".to_string(),
            false,
            None,
            None,
        )
        .await
    });

    acquire_barrier.wait().await;
    state.session_manager.delete(agent_id, &context_id).unwrap();
    acquire_barrier.wait().await;
    drop(owner);

    assert_eq!(
        trigger.await.expect("triggered producer panicked"),
        None,
        "deleted target must suppress registration"
    );
    assert!(state.run_manager.list_by_session(session_id, 10).is_empty());
    assert!(!state.session_manager.has_session(&(agent_id, context_id)));
    shutdown_token.cancel();
}

#[tokio::test]
async fn deleted_episode_target_releases_reserved_continuation() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let job_id = create_recurring_job(&state, agent_id, "episode cleanup");
    let context_id = format!("job_{}", job_id.0);
    let session = state.session_manager.get_or_create(agent_id, &context_id);
    let turn1 = RunId::new();
    let task_id = uuid::Uuid::new_v4();
    state.job_episodes.open(job_id, session.id, agent_id, turn1);
    assert!(matches!(
        state.job_episodes.on_run_complete(
            job_id,
            turn1,
            vec![],
            vec![(task_id, SessionId::new())]
        ),
        crate::runs::job_episode::RunCompletion::Open
    ));
    let route = state
        .job_episodes
        .resolve_subagent(task_id)
        .expect("terminal signal must reserve its continuation");
    state.session_manager.delete(agent_id, &context_id).unwrap();

    let result = crate::runs::notifications::enqueue_triggered_run(
        &state,
        agent_id,
        route.job_session_id,
        "late continuation".to_string(),
        route.context_id,
        "subagent".to_string(),
        false,
        Some(route.job_id),
        None,
    )
    .await;

    assert_eq!(result, None);
    assert!(
        state.job_episodes.snapshot(job_id).is_none(),
        "failed admission must release the final continuation reservation"
    );
    assert!(state.run_manager.list_by_session(session.id, 10).is_empty());
    shutdown_token.cancel();
}

/// Test that cancelling during shutdown also produces a `run_cancelled`
/// event, exercising the `state.shutdown_token.is_cancelled()` branch
/// in `execute_run()`.
#[tokio::test]
async fn cancelled_during_shutdown_emits_cancelled_event() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-shutdown");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "shutdown test".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Cancel the shutdown token (not the per-run token) to simulate
    // graceful shutdown in progress.
    shutdown_token.cancel();

    let mut rx = subscribe_session(&state, session_id);

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id,
            input: run.input,
            context_id: "test-shutdown".to_string(),
            cancel_token,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    let run = state.run_manager.get_run(run_id).expect("run should exist");
    assert_eq!(
        run.status(),
        RunStatus::Cancelled,
        "run during shutdown should be cancelled"
    );

    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected a run_cancelled SSE event during shutdown"
    );
}

/// Regression test for #1109: denying a tool approval must cancel runs
/// still queued on the same session, BEFORE the decision reaches the
/// runtime — they must NOT auto-start when the per-agent queue advances.
///
/// Drives the deny through the real `resolve_approval` handler. The
/// denied run's own token must stay uncancelled (the runtime's deny
/// branch owns it — see the #816 race note in `resolve_approval`), and
/// queued runs on other sessions must be untouched.
#[tokio::test]
async fn deny_cancels_queued_runs_in_same_session() {
    use crate::approvals::{PendingApproval, ResolveApprovalRequest};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-queue");
    let session_id = session.id;

    // Run A: Running, with a pending approval (the run being denied).
    let mut run_a = Run::new(session_id, agent_id, "guarded run".into());
    run_a.mark_running();
    let run_a_id = run_a.run_id;
    let _ = state.run_manager.insert_run(run_a);
    let token_a = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_a_id, token_a.clone());

    // Run B: Queued behind A on the SAME session.
    let run_b = Run::new(session_id, agent_id, "queued behind".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b.clone());
    let token_b = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_b_id, token_b.clone());

    // Run C: Queued on a DIFFERENT session of the same agent.
    let other_session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-queue-other");
    let run_c = Run::new(other_session.id, agent_id, "other session".into());
    let run_c_id = run_c.run_id;
    let _ = state.run_manager.insert_run(run_c);
    let token_c = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_c_id, token_c.clone());

    // Pending approval on run A, as the runtime's approval gate would
    // have registered it.
    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state.approval_store.insert(PendingApproval {
        approval_id,
        run_id: run_a_id,
        tool: "math".to_string(),
        params: serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        requested_at: chrono::Utc::now(),
        decision_tx,
    });

    let mut rx = subscribe_session(&state, session_id);

    // Deny through the real HTTP handler.
    let response = crate::approvals::resolve_approval(
        axum::extract::State(state.clone()),
        axum::extract::Path(approval_id),
        axum::Json(ResolveApprovalRequest {
            decision: "deny".to_string(),
        }),
    )
    .await;
    assert!(response.is_ok(), "deny resolution must succeed");

    // The runtime received `false` — and only after the queued-run
    // cancellation above had already been applied.
    assert!(
        !decision_rx.await.expect("decision channel must resolve"),
        "runtime must receive the deny decision"
    );

    // (1) Queued same-session run: token cancelled + state flipped.
    assert!(
        token_b.is_cancelled(),
        "queued same-session run's token must be cancelled on deny"
    );
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued same-session run must flip to Cancelled on deny"
    );

    // (2) The denied run is left to the runtime's deny branch.
    assert!(
        !token_a.is_cancelled(),
        "handler must NOT cancel the denied run's token — the runtime's \
         deny branch owns that after emitting the user_denied result"
    );

    // (3) Different-session queued run: untouched.
    assert!(
        !token_c.is_cancelled(),
        "queued run on another session must not be cancelled"
    );
    assert_eq!(
        state.run_manager.get_run(run_c_id).unwrap().status(),
        RunStatus::Queued,
        "queued run on another session must stay Queued"
    );

    // run_cancelled (for B) and approval_resolved (for A) on the session feed.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| e.event_type == "run_cancelled"),
        "expected run_cancelled SSE for the queued run; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert!(
        events.iter().any(|e| e.event_type == "approval_resolved"),
        "expected approval_resolved SSE; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    // (4) When the per-agent queue later dequeues B's work item, the
    // early-exit at the top of `execute_run` fires — B never runs.
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id: run_b_id,
            session_id,
            agent_id,
            input: run_b.input,
            context_id: "test-deny-queue".to_string(),
            cancel_token: token_b,
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued run must never auto-start after a deny"
    );

    shutdown_token.cancel();
}

/// Regression pin for the #1139 review's C1 race: a same-session `Queued`
/// run whose cancel token is NOT yet registered (the `create_run`
/// insert-to-register window, lifecycle.rs ~:879-961) must be SKIPPED by
/// the deny sweep — left `Queued`, never marked `Cancelled`, no
/// `run_cancelled` broadcast. Marking it would let `create_run` register
/// a fresh token and the unconditional `mark_running` resurrect a run
/// that already announced `run_cancelled`. The structural fix (register
/// token before `insert_run`) is tracked in #1142.
#[tokio::test]
async fn deny_skips_queued_run_without_registered_cancel_token() {
    use crate::approvals::{PendingApproval, ResolveApprovalRequest};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-deny-no-token");
    let session_id = session.id;

    // Run A: Running, with a pending approval (the run being denied).
    let mut run_a = Run::new(session_id, agent_id, "guarded run".into());
    run_a.mark_running();
    let run_a_id = run_a.run_id;
    let _ = state.run_manager.insert_run(run_a);
    let token_a = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_a_id, token_a.clone());

    // Run B: Queued on the SAME session, but with NO registered cancel
    // token — visible to `list_queued_for_session` exactly as it is
    // inside `create_run`'s insert-to-register window.
    let run_b = Run::new(session_id, agent_id, "queued, token pending".into());
    let run_b_id = run_b.run_id;
    let _ = state.run_manager.insert_run(run_b);

    let approval_id = uuid::Uuid::new_v4();
    let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
    state.approval_store.insert(PendingApproval {
        approval_id,
        run_id: run_a_id,
        tool: "math".to_string(),
        params: serde_json::json!({"operation": "add", "a": 1, "b": 2}),
        requested_at: chrono::Utc::now(),
        decision_tx,
    });

    let mut rx = subscribe_session(&state, session_id);

    // Deny through the real HTTP handler.
    let response = crate::approvals::resolve_approval(
        axum::extract::State(state.clone()),
        axum::extract::Path(approval_id),
        axum::Json(ResolveApprovalRequest {
            decision: "deny".to_string(),
        }),
    )
    .await;
    assert!(response.is_ok(), "deny resolution must succeed");
    assert!(
        !decision_rx.await.expect("decision channel must resolve"),
        "runtime must receive the deny decision"
    );

    // The token-less queued run escaped the sweep: still Queued, never
    // marked Cancelled (degraded pre-#1109 behavior, not corruption).
    assert_eq!(
        state.run_manager.get_run(run_b_id).unwrap().status(),
        RunStatus::Queued,
        "queued run without a registered cancel token must be left Queued \
         by the deny sweep — marking it Cancelled would race create_run's \
         token registration and resurrect via mark_running"
    );

    // No run_cancelled was broadcast for it either.
    let events = drain_events(&mut rx);
    assert!(
        !events.iter().any(|e| e.event_type == "run_cancelled"),
        "no run_cancelled SSE may be emitted for a skipped token-less run; \
         got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert!(
        events.iter().any(|e| e.event_type == "approval_resolved"),
        "approval_resolved must still fire; got: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1046 — HTTP cancel must flip state authoritatively
// ---------------------------------------------------------------------------

/// #1046 regression — `POST /runs/{id}/cancel` flips the run state to
/// `Cancelled` SYNCHRONOUSLY in the HTTP handler and broadcasts exactly
/// one `run_cancelled` SSE event before the response returns.
///
/// Pre-#1046 the state flip + SSE broadcast only happened inside
/// `execute_run`'s terminal arm, AFTER `drop(runtime)` and
/// `forwarder_handle.await` had completed. With an in-flight LLM HTTP
/// request being aborted (Windows + TLS connection drop), that cleanup
/// window was observed to stretch to ~8 seconds during which
/// `GET /runs/{id}` still reported `Running` and the SSE feed had
/// emitted no terminal event. The user-visible symptom matched the
/// issue report ("cancel doesn't work — agent keeps running").
///
/// This test pins the synchronous flip on the HTTP boundary: after
/// `cancel_run` returns OK, the run's status MUST be `Cancelled` and
/// the session SSE feed MUST have received exactly one `run_cancelled`
/// event.
#[tokio::test]
async fn http_cancel_flips_state_synchronously() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-http-cancel-flip");
    let session_id = session.id;

    // Set up a run in the Running state (the common case for cancel).
    let run = Run::new(session_id, agent_id, "test".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Register a cancel token so `cancel_run` finds it.
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    // Invoke the HTTP cancel handler directly.
    let cancel_state = state.clone();
    let response = crate::runs::lifecycle::cancel_run(State(cancel_state), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // The response shape matches the existing contract.
    assert_eq!(response.0["status"], "cancelled");

    // SYNCHRONOUS state flip: the moment the HTTP handler returns OK,
    // `GET /runs/{id}` must report `Cancelled`. No await between the
    // handler returning and this read, so any pre-#1046 deferred-flip
    // regression surfaces here.
    let run_after = state
        .run_manager
        .get_run(run_id)
        .expect("run must still exist after cancel");
    assert_eq!(
        run_after.status(),
        RunStatus::Cancelled,
        "run.status() MUST be Cancelled immediately after `cancel_run` \
         returns (#1046). Pre-fix: status stayed `Running` until \
         `execute_run`'s terminal arm completed."
    );

    // The cancel token was also cancelled (existing behaviour).
    assert!(
        cancel_token.is_cancelled(),
        "cancel token must be cancelled after `cancel_run`"
    );

    // The session feed must have received exactly one `run_cancelled`
    // event for this run.
    let events = drain_events(&mut session_rx);
    let cancelled_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "run_cancelled")
        .collect();
    assert_eq!(
        cancelled_events.len(),
        1,
        "HTTP cancel must emit exactly one `run_cancelled` event; got \
         {} events: {:?}",
        cancelled_events.len(),
        events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

#[tokio::test]
async fn http_cancel_persists_before_firing_the_cancellation_token() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-cancel-persistence-order");
    let run = Run::new(session.id, agent_id, "test".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    assert!(state.run_manager.mark_run_as_running(run_id));
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    let mut session_rx = subscribe_session(&state, session.id);
    state.run_manager.inject_next_persistence_failure();

    let Err((status, body)) =
        crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id)).await
    else {
        panic!("durable cancellation failure must fail the HTTP request");
    };

    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"]["code"], "LIFECYCLE_PERSISTENCE_FAILED");
    assert!(
        cancel_token.is_cancelled(),
        "the quarantined worker must be stopped after the durable attempt fails"
    );
    let quarantined = state.run_manager.get_run(run_id).unwrap();
    assert_eq!(quarantined.status(), RunStatus::Failed);
    assert_eq!(quarantined.terminal_reason(), Some("persistence_failed"));
    let peer_error =
        crate::runs::lifecycle::lifecycle_persistence_error_for_peer(&state, run_id, None)
            .expect("the quarantined terminal reason must retain persistence provenance");
    assert_eq!(peer_error, "Runtime error");
    assert!(!peer_error.contains("injected run persistence failure"));
    assert!(
        drain_events(&mut session_rx)
            .iter()
            .any(|event| event.event_type == "run_error"),
        "the persistence failure must be published immediately"
    );
    assert!(
        state
            .run_manager
            .activity_events_from(0)
            .await
            .iter()
            .any(|event| event.event_type == "session_activity_ended"),
        "the activity boundary must close immediately"
    );
    shutdown_token.cancel();
}

/// #1254 regression — when the HTTP `cancel_run` handler wins the race and
/// `execute_run`'s terminal arm consequently takes its "already terminal"
/// skip branch, the session stream must STILL receive the terminal event.
///
/// The skip line in the reported log reads like a dropped event, which is
/// what made this look like a defect. It is not: `cancel_run` OWNS the
/// broadcast when it wins, and the terminal arm stays silent precisely so
/// the event is not duplicated. This test pins that ownership so the two
/// sides can never both defer to each other.
///
/// Which skip branch, precisely: the terminal-transition barrier sits
/// before `match result`, so the mock-LLM loop has already returned
/// `Ok(_)` by the time the cancel lands and the token can no longer steer
/// it into a `Cancelled` arm. What runs is the **completed arm's**
/// already-terminal gate ("Run {} was already terminal when its loop
/// returned Ok"), which is structurally identical to the four
/// `Cancelled`/`Failed`/`Err` gates below it: same `marked_*` transition
/// bool, same silence, same reliance on the winner having broadcast. That
/// the run finishes with `run_cancelled` and no `run_finished` is exactly
/// this gate suppressing itself. The reported production line came from
/// the `CancelledWithToolCalls` sibling; that arm needs a hanging LLM to
/// reach and is not what this test drives.
///
/// Deterministic by construction — the barrier parks `execute_run`
/// immediately before its terminal transition, so the cancel is guaranteed
/// to land first rather than relying on timing.
///
/// Asserts the BROADCAST, not the persisted status: the status was already
/// correct in production and is exactly what hid the reported symptom.
#[tokio::test]
async fn http_cancel_emits_terminal_sse_even_when_the_terminal_arm_skips() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_mock_llm();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1254-cancel-broadcast");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "complete in mock mode".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    // Subscribe before anything runs so this is a live-delivery assertion.
    let mut session_rx = subscribe_session(&state, session_id);
    let barrier = crate::runs::lifecycle::install_terminal_transition_barrier(run_id);

    let exec_state = state.clone();
    let handle = tokio::spawn(async move {
        crate::runs::lifecycle::execute_run(
            exec_state,
            crate::runs::RunParams {
                run_id,
                session_id,
                agent_id,
                input: run.input,
                context_id: "test-1254-cancel-broadcast".to_string(),
                cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await;
    });

    // `execute_run` is now parked immediately before its terminal transition.
    barrier.wait().await;

    // The HTTP cancel wins the race: it flips the run terminal and owns the
    // terminal broadcast.
    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // Release `execute_run`. Its completed arm now finds the state already
    // terminal and takes the already-terminal skip branch.
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("execute_run must leave the terminal transition")
        .expect("execute_run task should not panic");

    let events = drain_events(&mut session_rx);
    let terminal: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .filter(|kind| matches!(*kind, "run_cancelled" | "run_finished" | "run_error"))
        .collect();

    assert_eq!(
        terminal,
        vec!["run_cancelled"],
        "cancelling must put exactly one terminal event on the session \
         stream even though `execute_run`'s terminal arm skipped its own \
         broadcast (#1254); saw terminal events {terminal:?} out of {:?}",
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>()
    );

    shutdown_token.cancel();
}

/// #1046 regression — race between the HTTP `cancel_run` handler and
/// `execute_run`'s terminal `Cancelled` arm produces EXACTLY ONE
/// `run_cancelled` SSE event for the same run.
///
/// The HTTP handler now flips state + broadcasts. When `execute_run`
/// later reaches its `Err(CancelledWithToolCalls)` arm (after the agent
/// loop unwound and the runtime was dropped), it calls
/// `mark_run_as_cancelled` again. The bool-returning contract makes
/// that second call a no-op for the broadcast: the state is already
/// `Cancelled` so `mark_run_as_cancelled` returns `false` and the SSE
/// branch is skipped. Without the gate, the UI's session feed would
/// see two `run_cancelled` events for the same run and append two
/// `(run cancelled)` system bubbles to the transcript.
///
/// Uses the hanging-LLM helper so `execute_run` gets far enough to
/// reach the terminal arm via a real `Err(Cancelled)` propagation
/// through `agent_loop`'s LLM-call `select!` rather than the pre-cancel
/// early-exit branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_cancel_and_execute_run_emit_single_event() {
    use axum::extract::{Path, State};

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_hanging_llm().await;
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-race-single-event");
    let session_id = session.id;

    let run = Run::new(session_id, agent_id, "trigger LLM hang".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());

    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());

    let mut session_rx = subscribe_session(&state, session_id);

    // Spawn `execute_run` so we can interleave the HTTP cancel with
    // its in-flight loop.
    let exec_state = state.clone();
    let exec_input = run.input.clone();
    let exec_cancel_token = cancel_token.clone();
    let exec_handle = tokio::spawn(async move {
        crate::runs::lifecycle::execute_run(
            exec_state,
            crate::runs::RunParams {
                run_id,
                session_id,
                agent_id,
                input: exec_input,
                context_id: "test-1046-race-single-event".to_string(),
                cancel_token: exec_cancel_token,
                is_peer_message: false,
                is_system_triggered: false,
                input_pre_persisted: false,
                dm_ended_peer: None,
            },
        )
        .await
    });

    // Wait for `run_started` so we know the loop has crossed
    // `mark_run_as_running` and is parked on the LLM HTTP request
    // (the hanging-LLM helper).
    let started_deadline = tokio::time::sleep(std::time::Duration::from_secs(3));
    tokio::pin!(started_deadline);
    let mut saw_started = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut started_deadline => break,
            event = session_rx.recv() => {
                match event {
                    Some(e) if e.event_type == "run_started" => {
                        saw_started = true;
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }
    assert!(
        saw_started,
        "expected `run_started` SSE event before HTTP cancel"
    );

    // Fire the HTTP cancel handler. With the #1046 fix this both
    // cancels the token AND broadcasts `run_cancelled` synchronously.
    let _ = crate::runs::lifecycle::cancel_run(State(state.clone()), Path(run_id))
        .await
        .expect("cancel_run should succeed for a Running run");

    // Let `execute_run` finish unwinding — agent_loop's `select!` will
    // wake on the cancelled token, unwind through `finish_run`'s
    // `Err(Cancelled)` arm, and reach the terminal
    // `CancelledWithToolCalls` arm in `execute_run`. With the #1046
    // gate, that arm sees `mark_run_as_cancelled` return `false` and
    // skips the duplicate broadcast.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), exec_handle)
        .await
        .expect("execute_run must complete within 15s after cancel");

    // Drain all remaining session events and count `run_cancelled`s.
    let events = drain_events(&mut session_rx);
    let cancelled_count = events
        .iter()
        .filter(|e| e.event_type == "run_cancelled")
        .count();
    assert_eq!(
        cancelled_count,
        1,
        "exactly one `run_cancelled` event must be emitted per run \
         even when the HTTP handler and `execute_run`'s terminal arm \
         both call `mark_run_as_cancelled` (#1046); got {cancelled_count} \
         events: {:?}",
        events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>()
    );

    // Final state is still Cancelled (idempotent re-mark in the
    // terminal arm does not regress the run to some other status).
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after execute_run");
    assert_eq!(
        final_run.status(),
        RunStatus::Cancelled,
        "run must remain `Cancelled` after `execute_run` completes its cleanup"
    );

    shutdown_token.cancel();
}

/// #1046 regression — `Run::mark_cancelled` returns `false` and does
/// NOT mutate `ended_at` when the run is already in a terminal state.
///
/// The idempotency contract is what makes the gateway-side
/// "first-writer-wins" broadcast gate work. Without it the cleanup
/// pass in `execute_run` would re-stamp `ended_at` to a later
/// (cleanup-time) timestamp, masking when the cancel actually
/// happened, and the bool return would always be `true` so the
/// duplicate-broadcast guard would not fire.
#[test]
fn run_mark_cancelled_is_idempotent_and_returns_transition_bool() {
    let session_id = SessionId::new();
    let agent_id = AgentId::new();
    let mut run = Run::new(session_id, agent_id, "test".into());

    // Queued → Cancelled transitions and reports true.
    assert!(matches!(run.status(), RunStatus::Queued));
    assert!(
        run.mark_cancelled(),
        "Queued → Cancelled must report transition=true"
    );
    assert!(matches!(run.status(), RunStatus::Cancelled));
    let first_ended_at = run.ended_at.expect("ended_at must be stamped");

    // Second call on an already-Cancelled run reports false and does
    // NOT update ended_at.
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(
        !run.mark_cancelled(),
        "second mark_cancelled on already-Cancelled run must report false"
    );
    assert_eq!(
        run.ended_at,
        Some(first_ended_at),
        "ended_at must NOT be updated by a no-op second mark_cancelled — \
         it should reflect when the cancel actually happened, not when \
         cleanup re-marked the run"
    );

    // Failed → mark_cancelled must NOT overwrite the terminal Failed
    // status (defense-in-depth; not a path the gateway exercises today,
    // but the idempotency invariant should hold for all terminal
    // states).
    let mut failed_run = Run::new(session_id, agent_id, "test".into());
    failed_run.mark_running();
    let _ = failed_run.mark_failed("boom".into());
    assert!(matches!(failed_run.status(), RunStatus::Failed));
    assert!(
        !failed_run.mark_cancelled(),
        "mark_cancelled on a Failed run must report false (no transition)"
    );
    assert!(
        matches!(failed_run.status(), RunStatus::Failed),
        "mark_cancelled on a Failed run must NOT overwrite the terminal status"
    );
}

/// #1046 — Codex P1 — `http_cancel_wins_against_natural_completion`.
///
/// **Bug shape (symmetric to the original #1046 fix):** the first
/// #1046 PR closed "cancel doesn't flip until `execute_run` finishes"
/// by making the HTTP handler flip state + broadcast `run_cancelled`
/// synchronously. That introduced a symmetric hole in the opposite
/// direction: `execute_run`'s terminal arms (`Ok` / `FailedWithToolCalls`
/// / generic `Err`) called `mark_run_as_completed` / `mark_run_as_failed`
/// UNCONDITIONALLY and emitted `run_finished` / `run_error`. If a near-
/// complete run had its state flipped to `Cancelled` by the HTTP handler
/// in the narrow window between `agent_loop` returning a non-cancel
/// outcome and `execute_run`'s terminal arm running, the terminal arm
/// would silently regress the state from `Cancelled` back to
/// `Completed` / `Failed` and emit a duplicate terminal SSE event on
/// top of the already-delivered `run_cancelled`. From the user's
/// perspective: they clicked cancel, saw it land, then the agent
/// "finished" anyway.
///
/// **Why a RunManager-boundary test rather than an end-to-end
/// interposer:** the bug requires `agent_loop` to return a NON-cancel
/// terminal outcome (`Ok` or non-cancel `Err`) BEFORE the HTTP cancel
/// arrives, then for the HTTP cancel to flip state to `Cancelled`,
/// then for `execute_run`'s terminal arm to run `mark_run_as_completed`
/// / `mark_run_as_failed`. Driving this end-to-end requires either (a)
/// a custom LLM helper that signals back through a channel before
/// returning `Ok` (so the test can fire HTTP cancel in the gap between
/// the signal and `execute_run`'s terminal arm dispatching), or (b) a
/// DashMap-barrier interposer that blocks the producer at
/// `mark_run_as_failed` — but the HTTP `cancel_run` handler ALSO
/// contends on the same DashMap shard via `mark_run_as_cancelled`, so
/// the barrier would deadlock the cancel path. Neither approach is
/// clean without adding test-only hooks into production code, and the
/// existing hanging-LLM helper drives the cancel through `agent_loop`'s
/// `select!` (producing `Err(CancelledWithToolCalls)` rather than a
/// non-cancel `Err`), so the failing-arm race path is unreachable
/// via that fixture.
///
/// Instead, this test pins the contract at the `RunManager` boundary
/// directly. The terminal-arm broadcast gates in `execute_run` are
/// trivial bool checks on the return value of `mark_run_as_*`; the
/// load-bearing invariant is the `RunManager`-side idempotency
/// contract that produces `false` when a cancelled run is later
/// re-marked. If that contract holds, the gates in `lifecycle.rs`
/// trivially follow. The pre-cancel-state simulation here is exactly
/// what the HTTP handler does in production
/// (`mark_run_as_cancelled` + `send_event(run_cancelled)`); the
/// subsequent `mark_run_as_completed` / `mark_run_as_failed` calls
/// are exactly what `execute_run`'s Ok / FailedWithToolCalls / generic
/// Err arms do.
///
/// **Contract pinned by this test:**
/// 1. `mark_run_as_completed` / `mark_run_as_failed` on a run whose
///    state is already `Cancelled` return `false`.
/// 2. The run's status remains `Cancelled` (no regression to
///    `Completed` / `Failed`).
/// 3. The `output` / `error` fields are NOT mutated by the no-op
///    flip — preserving the run's actual end-state for triage via
///    `GET /runs/{id}`.
/// 4. Using the bool gate, the gateway broadcasts EXACTLY ONE
///    terminal SSE event (`run_cancelled`) — no `run_finished` or
///    `run_error` follows.
#[tokio::test]
async fn http_cancel_wins_against_natural_completion() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "test-1046-cancel-wins-vs-completion");
    let session_id = session.id;

    // Set up a run that has crossed `mark_run_as_running` — i.e. the
    // production state at the moment the LLM call is in flight.
    let run = Run::new(session_id, agent_id, "race fixture".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    let mut session_rx = subscribe_session(&state, session_id);

    // Step 1 — simulate the HTTP `cancel_run` handler winning the
    // race: it flips the state to `Cancelled` and broadcasts
    // `run_cancelled` synchronously. This is exactly what the
    // post-original-#1046 handler does (see `cancel_run` in
    // lifecycle.rs).
    let http_cancel_transitioned = state.run_manager.mark_run_as_cancelled(run_id);
    assert!(
        http_cancel_transitioned,
        "Running → Cancelled must transition (sanity check on the test fixture)"
    );
    state
        .run_manager
        .send_event(run_id, session_id, SseEventData::run_cancelled(run_id))
        .await;

    // Step 2 — simulate `execute_run`'s Ok arm running AFTER the
    // HTTP cancel won the race: `agent_loop` returned `Ok(_)`, the
    // terminal arm calls `mark_run_as_completed`. Post-fix this
    // returns false (Codex P1 gate: `mark_completed` requires Running,
    // run is Cancelled). The `lifecycle.rs` broadcast gate uses this
    // bool to skip the duplicate `run_finished` event.
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        "the agent finished its work".to_string(),
        TokenUsage {
            prompt_tokens: 42,
            completion_tokens: 7,
            ..TokenUsage::default()
        },
    );
    assert!(
        !completed_transitioned,
        "mark_run_as_completed on a Cancelled run MUST return false. \
         Pre-#1046 symmetric-fix, this would return true (or have no \
         return at all) and the lifecycle would silently regress the \
         state to Completed and emit a duplicate `run_finished` SSE \
         event after `run_cancelled`."
    );
    // Production gate (mirrors the lifecycle.rs Ok arm post-fix). If
    // the gate were missing or inverted, this would fire a duplicate
    // `run_finished` event after `run_cancelled`.
    if completed_transitioned {
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_finished(run_id, true, TokenUsage::default()),
            )
            .await;
    }

    // Step 3 — simulate `execute_run`'s FailedWithToolCalls / generic
    // Err arm. Same shape on the failure side: `mark_run_as_failed`
    // on a Cancelled run must return false and not regress state.
    let failed_transitioned = state
        .run_manager
        .mark_run_as_failed(run_id, "post-cancel LLM 500".to_string());
    assert!(
        !failed_transitioned,
        "mark_run_as_failed on a Cancelled run MUST return false. \
         Pre-#1046 symmetric-fix, this would have overwritten the \
         state to Failed."
    );
    if failed_transitioned {
        state
            .run_manager
            .send_event(
                run_id,
                session_id,
                SseEventData::run_error(run_id, "post-cancel LLM 500"),
            )
            .await;
    }

    // Contract assertion: the run's stored state remains Cancelled,
    // and neither `output` nor `error` was mutated by the no-op
    // terminal flips.
    let final_run = state
        .run_manager
        .get_run(run_id)
        .expect("run must exist after the race");
    assert_eq!(
        final_run.status(),
        RunStatus::Cancelled,
        "run state must remain Cancelled — `mark_completed` / \
         `mark_failed` must NOT regress an already-Cancelled run. \
         Pre-#1046 symmetric-fix (Codex P1), the terminal arm \
         overwrote the state."
    );
    assert!(
        final_run.output.is_none(),
        "output field must NOT be populated by a no-op \
         mark_completed — got {:?}",
        final_run.output
    );
    assert!(
        final_run.error.is_none(),
        "error field must NOT be populated by a no-op mark_failed — \
         got {:?}",
        final_run.error
    );

    // Wire-contract assertion: exactly ONE terminal SSE event fired
    // on the session feed, and it is `run_cancelled`. Pre-fix code
    // emits two: `run_cancelled` followed by `run_finished` (or
    // `run_error`), breaking the single-terminal-event contract that
    // clients rely on.
    let events = drain_events(&mut session_rx);
    let terminal: Vec<&str> = events
        .iter()
        .map(|e| e.event_type.as_str())
        .filter(|t| matches!(*t, "run_cancelled" | "run_finished" | "run_error"))
        .collect();
    assert_eq!(
        terminal,
        vec!["run_cancelled"],
        "exactly one terminal SSE event must fire per run, and it \
         must be `run_cancelled` (cancel won the race against natural \
         completion); got {terminal:?}. Pre-fix code emits \
         `run_cancelled` followed by a regression event."
    );

    shutdown_token.cancel();
}
