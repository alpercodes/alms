// SPDX-License-Identifier: Apache-2.0

//! DM endings: ignore -> end_conversation -> notification, interrupted ends (#1258), `handle_dm_run_failure`, the completion gate (#1154), and the post-end fold (#1299).

use super::{
    drain_events, seed_agent, seed_alice_bob, subscribe_session, test_app_state,
    test_app_state_with_mock_llm, test_app_state_with_sqlite,
};
use crate::server::AppState;
use crate::sse::SseEventData;
use alms_coordinator::message_bus::{MessageSource, RunTrigger};
use alms_core::{AgentId, Run, RunId, RunStatus, SessionId};
use alms_tools::MessageSender;
use alms_tools::message_sender::ConversationEndReason;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 2. DM with ignore_message -> end_conversation -> notification flow
// ---------------------------------------------------------------------------

/// Test that a `ConversationEnded` trigger (with `Ignored` reason) flowing
/// through `run_trigger_loop` creates a notification run on the correct
/// session, emits `dm_conversation_ended` SSE, and persists a DM-ended
/// marker to the web-chat session.
///
/// This exercises the full DM lifecycle: ignore_message -> end_conversation
/// -> ConversationEnded trigger -> notification run + web-chat forwarding.
#[tokio::test]
async fn dm_conversation_ended_trigger_creates_notification_and_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // Create a notifications session for the target agent.
    let notif_session = state
        .session_manager
        .get_or_create(agent_id, "notifications:bob");
    let notif_session_id = notif_session.id;
    let notif_context_id = notif_session.context_id.clone();

    // Create a user-facing web-chat session so that
    // `notify_dm_ended_to_webchat` has a target.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;

    // Subscribe to web-chat session to capture the forwarded DM-ended event.
    let mut web_rx = subscribe_session(&state, web_session_id);

    // Build a ConversationEnded trigger with Ignored reason.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    // Run the trigger loop to completion.
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Verify a notification run was created on the notifications session.
    let runs = state.run_manager.list_by_session(notif_session_id, 10);
    assert!(
        !runs.is_empty(),
        "expected at least one run on the notifications session"
    );
    assert_eq!(
        runs[0].session_id, notif_session_id,
        "notification run must be on the notifications: session"
    );
    assert_eq!(runs[0].agent_id, agent_id);

    // Verify that the web-chat session received a DM-ended marker
    // (persisted by `notify_dm_ended_to_webchat`).
    let web_history = state.session_manager.get_history(web_session_id).unwrap();
    let dm_ended_markers: Vec<_> = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .collect();
    assert!(
        !dm_ended_markers.is_empty(),
        "expected a dm_ended_notification marker on the web-chat session"
    );

    // Verify the marker contains the peer name.
    let marker_meta = dm_ended_markers[0].metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta.get("peer").and_then(|v| v.as_str()),
        Some("alice"),
        "DM-ended marker should reference the peer who ended the conversation"
    );
    assert_eq!(
        marker_meta.get("reason").and_then(|v| v.as_str()),
        Some("ignored"),
        "DM-ended marker should record the reason as 'ignored'"
    );

    // Verify SSE events were forwarded to the web-chat session.
    let web_events = drain_events(&mut web_rx);
    let ended = web_events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "expected a dm_conversation_ended SSE event on the web-chat session; got: {:?}",
                web_events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    // A pure-recipient forward persists the marker (the run is on the invisible
    // notifications: session), so it must NOT suppress the live banner either.
    assert_ne!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "pure-recipient forward must not set suppress_banner (the banner is the \
         one visible indicator)"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1258 — an interrupted DM end is delivered as a marker, not as a run
// ---------------------------------------------------------------------------

/// What the operator's session ended up with after ONE `ConversationEnded`
/// trigger was driven through `run_trigger_loop`.
struct DmEndDelivery {
    /// Runs created on the operator's web-chat session.
    runs: Vec<Run>,
    /// Persisted `dm_ended_notification` markers on that session.
    markers: Vec<alms_session::Message>,
    /// `dm_conversation_ended` SSE frames delivered to that session.
    events: Vec<SseEventData>,
}

/// Drive one `ConversationEnded` trigger whose target IS the agent's
/// user-facing web-chat — the shape of the #1258 incident, where the
/// notification landed on the very session the operator had just cancelled a
/// run on (`source_session_id = Some(web-chat)`, the #556 initiator-ends
/// routing).
///
/// Every arm of the delivery is captured, so a caller asserting "no run" is
/// also forced to say what the operator DOES get instead — the assertion
/// cannot pass by the trigger having been silently dropped.
async fn drive_dm_end_on_operator_session(reason: ConversationEndReason) -> DmEndDelivery {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let peer_agent_id = AgentId::new();

    // The operator's web-chat: both the trigger's target and the answer
    // `find_user_facing_session` gives the marker forward.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;
    let mut web_rx = subscribe_session(&state, web_session_id);

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: web_session_id,
            input: "DM ended".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: peer_agent_id,
                from_name: "scout".to_string(),
                reason,
                self_notification: true,
                source_session_id: Some(web_session_id),
            },
            context_id: web_session.context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let runs = state.run_manager.list_by_session(web_session_id, 10);
    let markers: Vec<alms_session::Message> = state
        .session_manager
        .get_history(web_session_id)
        .expect("web-chat session must exist")
        .into_iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .collect();
    let events: Vec<SseEventData> = drain_events(&mut web_rx)
        .into_iter()
        .filter(|e| e.event_type == "dm_conversation_ended")
        .collect();

    shutdown_token.cancel();

    DmEndDelivery {
        runs,
        markers,
        events,
    }
}

/// #1258, the reported incident: a DM run dies on an upstream 429, and 470ms
/// after the operator cancelled a run on their web-chat the DM-ended
/// notification puts a NEW run on that same session — work they did not ask
/// for, about a conversation they were not watching, indistinguishable from
/// the cancel having been ignored.
///
/// The run that was going to produce this DM's outcome died mid-turn, so it
/// must cost no further turn: the operator learns about it from the persisted
/// banner marker instead, which also has to carry the failure text now that no
/// prose turn explains it.
#[tokio::test]
async fn errored_dm_end_delivers_marker_and_starts_no_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Errored {
        message: "LLM rate limit exceeded".to_string(),
        interrupted: true,
    })
    .await;

    assert!(
        delivered.runs.is_empty(),
        "an errored DM end whose run died must not start a run on the \
         operator's session; got {:?}",
        delivered.runs
    );

    assert_eq!(
        delivered.markers.len(),
        1,
        "the operator must still learn the DM ended — exactly one reloadable \
         marker; got {:?}",
        delivered.markers
    );
    let meta = delivered.markers[0]
        .metadata
        .as_ref()
        .expect("marker carries metadata");
    assert_eq!(meta.get("reason").and_then(|v| v.as_str()), Some("errored"));
    assert_eq!(
        meta.get("detail").and_then(|v| v.as_str()),
        Some("LLM rate limit exceeded"),
        "the marker must carry the failure text — with no run to narrate it, \
         this is the only place the operator can read WHY"
    );
    let alms_session::Content::Text(ref content) = delivered.markers[0].content else {
        panic!("marker content must be text");
    };
    assert!(
        content.contains("run failed") && content.contains("LLM rate limit exceeded"),
        "marker text must read as an explanation, not a raw reason code; got {content:?}"
    );

    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_ne!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "with no run standing in for it, the live banner must render"
    );
    assert_eq!(
        ended.data.get("detail").and_then(|v| v.as_str()),
        Some("LLM rate limit exceeded"),
        "the live banner must carry the same failure text as the marker"
    );
}

/// #1258, the other half of "interrupted": the operator cancelled. A cancel is
/// the strongest possible *stop doing things here* signal, so the end that
/// follows it must not reappear as a fresh run on the same session.
#[tokio::test]
async fn user_cancelled_dm_end_delivers_marker_and_starts_no_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::UserCancelled).await;

    assert!(
        delivered.runs.is_empty(),
        "a user-cancelled DM end must not start a run on the operator's \
         session; got {:?}",
        delivered.runs
    );
    assert_eq!(
        delivered.markers.len(),
        1,
        "the cancel must still be recorded for reload; got {:?}",
        delivered.markers
    );
    let meta = delivered.markers[0]
        .metadata
        .as_ref()
        .expect("marker carries metadata");
    assert_eq!(
        meta.get("reason").and_then(|v| v.as_str()),
        Some("user_cancelled")
    );
    assert!(
        meta.get("detail").is_none(),
        "a cancel carries no failure text — `detail` must stay off non-errored \
         markers so their shape is unchanged"
    );
    let alms_session::Content::Text(ref content) = delivered.markers[0].content else {
        panic!("marker content must be text");
    };
    assert!(
        content.contains("cancelled by user"),
        "marker text must read as an explanation, not a raw reason code; got {content:?}"
    );
    assert!(
        !delivered.events.is_empty(),
        "the phase-clear SSE is unconditional — otherwise the web-chat is \
         stuck showing 'Chatting with scout'"
    );
}

/// The control for the two tests above, on byte-identical wiring: a
/// *concluded* end still gets its notification run. Only the reason differs,
/// so "no run was created" above cannot be an artefact of the harness — this
/// same setup does create one.
///
/// It also pins the thing the #1258 suppression must not break: a DM that ran
/// its course carries a transcript the agent has to relay (the #429 history
/// embedding), and that relaying is the run.
#[tokio::test]
async fn concluded_dm_end_still_starts_its_notification_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Ignored).await;

    assert_eq!(
        delivered.runs.len(),
        1,
        "a concluded DM end must still relay its outcome as a run; got {:?}",
        delivered.runs
    );
    assert!(
        delivered.markers.is_empty(),
        "the run IS the visible notification here, so the marker stays \
         suppressed (#1215 'initiator gets both'); got {:?}",
        delivered.markers
    );
    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "the run is the notification, so the live banner is suppressed"
    );
}

/// #1258 / Tim's review of PR #1267 — the edge that keeps the suppression
/// from becoming the very bug it was written to avoid.
///
/// `Errored` covers two materially different things. `dm_lifecycle`'s Exit 3
/// (#1154, "agent run completed without producing a reply") and its
/// delivery-failure sibling both describe a run that **completed** — it just
/// had nothing usable on its LAST turn, possibly after several delivered
/// ones. Those earlier turns exist only in the `dm:` session; the
/// notification run and its #429 transcript are what carry them to the
/// operator's chat.
///
/// So this end must keep its run even though the reason string is `errored`.
/// Suppressing it would trade a spurious spinner for a silently dropped
/// answer — which is exactly why blanket "no run for a DM-ended
/// notification" was rejected in the first place.
///
/// Same harness as the two suppression tests above and as the concluded
/// control: only `interrupted` differs.
#[tokio::test]
async fn errored_dm_end_from_a_completed_run_still_starts_its_notification_run() {
    let delivered = drive_dm_end_on_operator_session(ConversationEndReason::Errored {
        message: "agent run completed without producing a reply".to_string(),
        interrupted: false,
    })
    .await;

    assert_eq!(
        delivered.runs.len(),
        1,
        "an `errored` end whose run COMPLETED still has a transcript to \
         relay, so it must keep its notification run; got {:?}",
        delivered.runs
    );
    assert!(
        delivered.markers.is_empty(),
        "the run IS the visible notification here, so the marker stays \
         suppressed (#1215 'initiator gets both'); got {:?}",
        delivered.markers
    );
    let ended = delivered
        .events
        .first()
        .expect("the phase-clear SSE is unconditional");
    assert_eq!(
        ended.data.get("reason").and_then(|v| v.as_str()),
        Some("errored"),
        "the `interrupted` split is a routing input, not a wire change — both \
         `Errored` shapes stay `errored` on the SSE"
    );
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "the run is the notification, so the live banner is suppressed"
    );
}

/// #1258 / Tim's review of PR #1267 — why the suppression keys on "was the
/// turn cut short", not on "is the transcript empty".
///
/// The tempting predicate is the transcript: a notification run is
/// load-bearing precisely when it carries DM content the operator's web-chat
/// has never seen. It does not work as a predicate, because a DM that is
/// eligible to end always has content. `MessageBus::end_conversation`
/// refuses to run unless the DM session exists, and the session exists only
/// because a `send_message` persisted a message into it — so the initiating
/// message alone makes the history non-empty.
///
/// That is not a corner case, it is the reported incident: the ended run was
/// the *recipient's*, so the peer's opening message was already in the
/// transcript. A transcript-gated suppression would have let #1258's run
/// through.
#[tokio::test]
async fn an_interrupted_dm_end_still_has_a_transcript() {
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice opens the DM. This is the ONLY message in it — the shape of the
    // #1258 incident at the moment bob's run died.
    let receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "please review X", None)
        .await
        .expect("send must succeed");
    let _ = trigger_rx.try_recv(); // drain bob's DM-delivery trigger

    // Bob's DM run dies on an upstream 429 before he says anything.
    state
        .message_bus
        .end_conversation(
            "bob",
            bob_id,
            "alice",
            alice_id,
            ConversationEndReason::Errored {
                message: "LLM rate limit exceeded".to_string(),
                interrupted: true,
            },
        )
        .await
        .expect("end must succeed");

    let messages = state
        .session_manager
        .get_history(receipt.session_id)
        .expect("DM session must exist");
    let transcript = crate::runs::notifications::format_dm_conversation_history(&messages);
    assert!(
        !transcript.is_empty(),
        "an interrupted DM still has a transcript, so `conversation_history` \
         cannot be the suppression predicate; got {transcript:?}"
    );
    assert!(
        transcript.contains("please review X"),
        "the initiating message is what makes it non-empty; got {transcript:?}"
    );

    shutdown_token.cancel();
}

/// #1215: when a `ConversationEnded` trigger carries a `source_session_id`
/// (the agent initiated/ended from a user-facing session), the notification
/// RUN is routed to that source session and is itself the visible
/// notification there. The gateway must uphold BOTH:
///
/// 1. The redundant persisted `dm_ended_notification` banner marker is
///    SKIPPED — otherwise the DM-end renders twice ("initiator gets both").
/// 2. The lightweight `dm_conversation_ended` SSE is STILL forwarded to the
///    web-chat so the "Chatting with {peer}" status clears (Tim's C1 on
///    #1218). It is the ONLY path that reaches the web-chat, and the
///    notification run re-asserts the DM phase (#688) rather than clearing
///    it — so dropping the SSE with the marker would strand the status bar.
#[tokio::test]
async fn dm_conversation_ended_with_source_session_clears_phase_but_skips_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The agent has a user-facing source session (its web-chat). The
    // MessageBus routes the notification run here and sets source_session_id.
    let source_session = state.session_manager.get_or_create(agent_id, "web");
    let source_session_id = source_session.id;
    let source_context_id = source_session.context_id.clone();

    // Subscribe to the source session so we can observe the phase-clear SSE.
    let mut src_rx = subscribe_session(&state, source_session_id);

    // Build a ConversationEnded trigger WITH a source session, mirroring how
    // the MessageBus emits the initiator/self notification.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: source_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(source_session_id),
            },
            context_id: source_context_id,
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The notification run still fires on the source session (#556 preserved).
    let runs = state.run_manager.list_by_session(source_session_id, 10);
    assert!(
        !runs.is_empty(),
        "the #556 self-notification run must still be created on the source session"
    );
    assert_eq!(runs[0].agent_id, agent_id);

    // (1) NO redundant dm_ended_notification banner marker is persisted to
    // the source session — the run is the single visible notification there.
    let history = state
        .session_manager
        .get_history(source_session_id)
        .unwrap_or_default();
    let banner_markers = history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 0,
        "no dm_ended_notification banner should be persisted when the run \
         already lands in the user-facing source session (#1215 initiator-gets-both)"
    );

    // (2) But the dm_conversation_ended SSE IS forwarded to the source session
    // so the web-chat clears the "Chatting with {peer}" phase (Tim's C1). The
    // reason here is `Ignored` and there is no agent registry, so the only
    // possible source of this event on the source session is the phase-clear
    // forward inside notify_dm_ended_to_webchat.
    let src_events = drain_events(&mut src_rx);
    let ended = src_events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "the phase-clear dm_conversation_ended SSE must still reach the source \
                 session even when the marker is suppressed; got: {:?}",
                src_events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    // ...and it carries suppress_banner=true: suppressing the marker must also
    // suppress the LIVE banner, so a live viewer sees only the notification run
    // (the live half of "initiator gets both", #1215).
    assert_eq!(
        ended.data.get("suppress_banner").and_then(|v| v.as_bool()),
        Some(true),
        "suppressing the reloadable marker must also suppress the live banner"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (the #1202 job-as-DM-source interaction): when a scheduled job is
/// the DM source, `source_session_id = Some(job_session_id)` but the job
/// session is INTERNAL (`job_*`), so the notification run is routed to the job
/// session, NOT the agent's web-chat. The operator watching the web-chat must
/// therefore STILL get the persisted `dm_ended_notification` banner — there is
/// no visible run there to stand in for it. The marker is suppressed ONLY when
/// the source session IS the marker target; a `job_*` source is never the
/// user-facing target, so the banner persists.
#[tokio::test]
async fn dm_conversation_ended_job_source_persists_webchat_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The DM source is the job's (internal) session — where the notification
    // run is routed, NOT the web-chat.
    let job_session = state
        .session_manager
        .get_or_create(agent_id, "job_550e8400-e29b-41d4-a716-446655440000");
    let job_session_id = job_session.id;
    let job_context_id = job_session.context_id.clone();

    // The agent also has a user-facing web-chat that the operator is watching.
    let web_session = state.session_manager.get_or_create(agent_id, "web");
    let web_session_id = web_session.id;

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: job_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(job_session_id),
            },
            context_id: job_context_id,
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The DM-ended banner MUST be persisted to the web-chat: the notification
    // run went to the internal job session, so the web-chat has no visible run
    // to replace the banner. Pre-fix (persist_marker = source.is_none()) this
    // was suppressed -> operator saw a transient phase-clear but nothing
    // reloadable and no run (#1218 P2).
    let web_history = state
        .session_manager
        .get_history(web_session_id)
        .unwrap_or_default();
    let banner_markers = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "a job-as-DM-source DM-end must persist the web-chat banner (the run is \
         routed to the internal job session, not the web-chat) — #1218 P2"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (second): `notify_dm_ended_to_webchat` targets the agent's
/// MOST-RECENT user-facing session, which can differ from the source session
/// the run is routed to when the agent has more than one user-facing chat.
/// The marker must still be persisted to the target chat — otherwise that
/// chat gets neither the run (it is in the source chat) nor a reloadable
/// banner, only a transient SSE. The marker is suppressed ONLY when the
/// source session IS the exact marker target (`source_session_id == target`),
/// which is the identity check that subsumes the earlier internal-source case.
#[tokio::test]
async fn dm_conversation_ended_source_differs_from_marker_target_persists_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // The agent has TWO user-facing sessions.
    let web_a = state.session_manager.get_or_create(agent_id, "web-a");
    let web_b = state.session_manager.get_or_create(agent_id, "web-b");

    // Discover the actual marker target (the most-recent user-facing session)
    // and pick a DIFFERENT user-facing session as the DM source, so the check
    // is deterministic regardless of last-activity ordering.
    let target = crate::runs::find_user_facing_session(&state.session_manager, agent_id)
        .expect("agent has user-facing sessions");
    let target_id = target.id;
    let (source_id, source_ctx) = if target_id == web_a.id {
        (web_b.id, "web-b")
    } else {
        (web_a.id, "web-a")
    };
    assert_ne!(
        source_id, target_id,
        "source must differ from the marker target"
    );

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: source_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(source_id),
            },
            context_id: source_ctx.to_string(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The banner MUST land on the marker target: the run is in the source
    // chat (a different session), so the target chat has no visible run to
    // replace the banner. Pre-fix (marker suppressed whenever the source was
    // user-facing) the target chat got only a transient SSE (#1218 P2).
    let target_history = state
        .session_manager
        .get_history(target_id)
        .unwrap_or_default();
    let banner_markers = target_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "marker must persist to the most-recent user-facing session when the DM \
         source is a DIFFERENT user-facing chat than the target (#1218 P2)"
    );

    shutdown_token.cancel();
}

/// #1218 P2 (#3, the ordering / #1205 episode-routing case): when an open job
/// episode awaits the SAME DM that a user-facing web-chat also sourced, the
/// #1205 episode override REROUTES the notification run to the job session.
/// The web-chat is then not a run target, so its reloadable banner must
/// persist — otherwise the operator watching the web-chat gets neither the run
/// (it went to the job session) nor a marker, only a transient SSE. This is
/// why the web-chat forward is deferred until the FINAL `run_targets` are
/// known: keying persistence on `source_session_id` alone (pre-fix) suppressed
/// the marker here because source == the web-chat target.
#[tokio::test]
async fn dm_conversation_ended_episode_rerouted_run_persists_webchat_marker() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state_with_sqlite();

    // Register alice + bob so peer-name resolution (bob) works and resolve_dm
    // can compute the deterministic DM session id.
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // bob has a user-facing web-chat (the DM source AND the marker target) ...
    let web_session = state.session_manager.get_or_create(bob_id, "web");
    let web_session_id = web_session.id;

    // ... and an open job episode awaiting the alice<->bob DM.
    let job_id = alms_core::JobId::new();
    let job_ctx = format!("job_{}", job_id.0);
    let job_session = state.session_manager.get_or_create(bob_id, &job_ctx);
    let job_session_id = job_session.id;
    let turn1 = alms_core::RunId::new();
    state
        .job_episodes
        .open(job_id, job_session_id, bob_id, turn1);
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let _ = state
        .job_episodes
        .on_run_complete(job_id, turn1, vec![dm_session_id], vec![]);

    // ConversationEnded for bob, sourced from bob's web-chat. Episode routing
    // supersedes the web-chat target and sends the run to the job session.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id: bob_id,
            session_id: web_session_id,
            input: "DM ended by alice".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: alice_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::Ignored,
                self_notification: false,
                source_session_id: Some(web_session_id),
            },
            context_id: "web".to_string(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // The continuation run went to the JOB session (episode routing fired) ...
    let job_runs = state.run_manager.list_by_session(job_session_id, 10);
    assert!(
        !job_runs.is_empty(),
        "episode routing must send the continuation run to the job session"
    );
    // ... and NOT to the web-chat.
    let web_runs = state.run_manager.list_by_session(web_session_id, 10);
    assert!(
        web_runs.is_empty(),
        "the run must NOT land on the web-chat when episode routing reroutes it"
    );

    // So the web-chat still needs the reloadable banner: it must be persisted
    // even though the DM source == the web-chat target (#1218 P2 ordering).
    let web_history = state
        .session_manager
        .get_history(web_session_id)
        .unwrap_or_default();
    let banner_markers = web_history
        .iter()
        .filter(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("type").and_then(|v| v.as_str()) == Some("dm_ended_notification")
            })
        })
        .count();
    assert_eq!(
        banner_markers, 1,
        "marker must persist to the web-chat when the run is rerouted to a job \
         session by #1205 episode routing (#1218 P2 ordering)"
    );

    shutdown_token.cancel();
}

/// Test that a `ConversationEnded` trigger with `DepthExceeded` reason
/// handles a missing agent registry gracefully (no panic) and still creates
/// the notification run.
///
/// Without a SQLite agent registry, peer name resolution fails and the
/// depth-exceeded SSE emission path is skipped. This verifies the code
/// degrades gracefully rather than panicking.
#[tokio::test]
async fn dm_depth_exceeded_graceful_without_agent_registry() {
    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let sender_agent_id = AgentId::new();

    // Register the target agent in the session manager so that the
    // peer name resolution (`load_agent_by_id`) works. Without SQLite
    // there is no agent registry, so this path will skip the SSE emission.
    // We need to verify the code path doesn't panic and gracefully
    // handles the missing agent record.
    let notif_session = state
        .session_manager
        .get_or_create(agent_id, "notifications:bob");
    let notif_session_id = notif_session.id;
    let notif_context_id = notif_session.context_id.clone();

    // Create the DM session between alice and bob so we can subscribe.
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let _dm_session = state
        .session_manager
        .get_or_create_with_id(dm_session_id, agent_id, "dm:alice:bob")
        .unwrap();
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx
        .send(RunTrigger {
            agent_id,
            session_id: notif_session_id,
            input: "DM depth exceeded".to_string(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: "alice".to_string(),
                reason: ConversationEndReason::DepthExceeded,
                self_notification: false,
                source_session_id: None,
            },
            context_id: notif_context_id.clone(),
        })
        .await
        .unwrap();
    drop(test_tx);

    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    // Without a SQLite agent registry, the peer name resolution will fail
    // and the depth-exceeded SSE emission path will be skipped. Verify
    // the run was still created (the notification run is independent of
    // the SSE emission).
    let runs = state.run_manager.list_by_session(notif_session_id, 10);
    assert!(
        !runs.is_empty(),
        "notification run should be created even when peer resolution fails"
    );

    // The DM session SSE events may or may not contain dm_conversation_ended
    // depending on whether peer name resolution succeeded. In our test setup
    // (no SQLite), it won't. Verify no panic occurred.
    let _dm_events = drain_events(&mut dm_rx);

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// handle_dm_run_failure tests
//
// Regression coverage for the gap surfaced during v0.2.2 sign-off: the four
// error arms in `execute_run` (Cancelled, CancelledWithToolCalls,
// FailedWithToolCalls, generic Err) skipped DM peer-state notification, so
// a peer-triggered DM run that failed left the depth counter, `dm_ended`
// marker, and `ConversationEnded` peer notification unset until the 1800s
// `DEPTH_EXPIRY_SECS` sweep eventually cleared it.
// ---------------------------------------------------------------------------

/// Test that `handle_dm_run_failure` is a no-op for non-peer runs.
///
/// The helper must NOT call `MessageBus::end_conversation` or emit any SSE
/// when `is_peer_message` is false — only peer-triggered DM runs participate
/// in the DM lifecycle handshake.
#[tokio::test]
async fn handle_dm_run_failure_skips_when_not_peer_message() {
    let (state, _shutdown, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session_id = SessionId::deterministic_dm("alice", "bob");
    state
        .session_manager
        .get_or_create_with_id(session_id, bob_id, "dm:alice:bob")
        .unwrap();
    let mut dm_rx = subscribe_session(&state, session_id);

    let run_id = alms_core::RunId::new();
    let res = crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &session_id,
        bob_id,
        Some("bob"),
        "dm:alice:bob",
        false, // <-- is_peer_message = false
        ConversationEndReason::UserCancelled,
    )
    .await;
    assert!(res.is_ok(), "no-op path should succeed");

    // No RunTrigger should have been emitted.
    assert!(
        tr.try_recv().is_err(),
        "no ConversationEnded RunTrigger should fire for non-peer runs"
    );

    // No SSE event should land on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        events.is_empty(),
        "no SSE should be emitted; got {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
}

/// Test that `handle_dm_run_failure` is a no-op for non-DM context IDs.
///
/// The helper must only act on `dm:` context IDs — a peer-triggered run on
/// any other context (e.g. `notifications:bob`, `web`, `subagent_…`) is not
/// a DM and must not trigger the DM end handshake.
#[tokio::test]
async fn handle_dm_run_failure_skips_when_not_dm_context() {
    let (state, _shutdown, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session = state.session_manager.get_or_create(bob_id, "web");
    let session_id = session.id;
    let mut rx = subscribe_session(&state, session_id);

    let run_id = alms_core::RunId::new();
    let res = crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &session_id,
        bob_id,
        Some("bob"),
        "web", // <-- not a dm: context
        true,
        ConversationEndReason::UserCancelled,
    )
    .await;
    assert!(res.is_ok());

    assert!(tr.try_recv().is_err(), "no RunTrigger for non-DM context");
    tokio::task::yield_now().await;
    let events = drain_events(&mut rx);
    assert!(
        events.is_empty(),
        "no SSE for non-DM context; got {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
}

/// Happy path: a peer-triggered DM run that errored should end the
/// conversation, reset the depth counter, emit a `ConversationEnded`
/// `RunTrigger` carrying `Errored { message }` for the peer, and emit a
/// `dm_conversation_ended` SSE on the DM session stream.
#[tokio::test]
async fn handle_dm_run_failure_errored_resets_depth_and_emits_sse() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Bootstrap the depth counter as if alice had just sent a DM to bob.
    // This routes through the real `MessageBus::send`, which creates the
    // shared DM session and bumps the depth.
    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    // Drain the trigger emitted by `send` so subsequent assertions are
    // scoped to the failure-driven trigger only.
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Subscribe to the DM session SSE stream so we can capture the
    // `dm_conversation_ended` event.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Simulate bob's peer-triggered DM run failing mid-flight. This is the
    // generic `Err(e)` arm in `execute_run` — the message field is the
    // truncated error display.
    let run_id = alms_core::RunId::new();
    let result = crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "LLM provider error".to_string(),
            interrupted: true,
        },
    )
    .await;
    assert!(
        result.is_ok(),
        "happy path should return Ok; got {result:?}"
    );

    // 1. The DM session must contain a `dm_ended` marker — this proves
    //    `MessageBus::end_conversation` ran (the marker write happens after
    //    the depth counter has been removed). The depth-counter map is a
    //    `pub(super)` field of MessageBus so we cannot inspect it directly
    //    from the gateway tests; the marker is the next-best public proof.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    let dm_ended_marker = history.iter().find(|m| {
        m.metadata
            .as_ref()
            .and_then(|meta| meta.get("message_type"))
            .and_then(|v| v.as_str())
            == Some("dm_ended")
    });
    assert!(
        dm_ended_marker.is_some(),
        "expected a `dm_ended` marker to be persisted to the DM session"
    );
    let marker_meta = dm_ended_marker.unwrap().metadata.as_ref().unwrap();
    assert_eq!(
        marker_meta.get("reason").and_then(|v| v.as_str()),
        Some("errored"),
        "dm_ended marker must record the Errored reason"
    );

    // 2. `ConversationEnded` RunTrigger should have been emitted for alice
    //    (the peer of the failed run) carrying the `Errored` reason.
    let trigger = tr.try_recv().expect("expected ConversationEnded trigger");
    match trigger.source {
        MessageSource::ConversationEnded {
            from_name, reason, ..
        } => {
            assert_eq!(from_name, "bob", "from_name must be the failed run's agent");
            match reason {
                ConversationEndReason::Errored {
                    message,
                    interrupted,
                } => {
                    assert_eq!(message, "LLM provider error");
                    assert!(
                        interrupted,
                        "the caller's `interrupted` classification must reach \
                         the trigger unchanged — it is what decides whether \
                         the peer's end costs a turn (#1258)"
                    );
                }
                other => panic!("expected Errored reason, got {other:?}"),
            }
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // 3. `dm_conversation_ended` SSE event with reason "errored" should
    //    have landed on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "expected dm_conversation_ended SSE; got {:?}",
                events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            )
        });
    let reason_str = dm_ended
        .data
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        reason_str, "errored",
        "SSE reason must be 'errored' for the Errored variant"
    );

    shutdown_token.cancel();
}

/// Cancel arm: a peer-triggered DM run that was cancelled mid-flight should
/// emit `dm_conversation_ended` with reason `user_cancelled` and reset the
/// depth counter, mirroring the `Errored` happy-path test.
#[tokio::test]
async fn handle_dm_run_failure_user_cancelled_emits_sse() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    let run_id = alms_core::RunId::new();
    crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::UserCancelled,
    )
    .await
    .unwrap();

    // dm_ended marker is the public proof that `MessageBus::end_conversation`
    // ran end-to-end (depth counter removal happens immediately before the
    // marker write).
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("reason"))
                .and_then(|v| v.as_str())
                == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled must be persisted"
    );

    let trigger = tr.try_recv().expect("expected ConversationEnded trigger");
    match trigger.source {
        MessageSource::ConversationEnded { reason, .. } => {
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "trigger reason must be UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .expect("expected dm_conversation_ended SSE");
    assert_eq!(
        dm_ended
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "user_cancelled",
    );

    shutdown_token.cancel();
}

/// Atomicity guard: when both peers race to end the conversation (e.g. the
/// peer's happy-path `ignore_message` runs at the same time as the local
/// run's failure arm), only the first caller should win. The atomicity
/// guard at `bus.rs:359` (`depths.remove()`) ensures the second
/// `end_conversation` returns `Ok(())` without re-emitting a trigger.
#[tokio::test]
async fn handle_dm_run_failure_double_end_is_idempotent() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    let run_id = alms_core::RunId::new();

    // First call: should emit the ConversationEnded trigger.
    crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "first".into(),
            interrupted: true,
        },
    )
    .await
    .unwrap();

    // Second call: depth has been removed already; bus.rs's atomicity guard
    // returns Ok(()) without re-emitting a trigger.
    crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::Errored {
            message: "second".into(),
            interrupted: true,
        },
    )
    .await
    .unwrap();

    // Exactly one ConversationEnded trigger should have been emitted across
    // the two calls (the second call hits the atomicity guard).
    let mut conversation_ended_triggers = 0;
    while let Ok(t) = tr.try_recv() {
        if matches!(t.source, MessageSource::ConversationEnded { .. }) {
            conversation_ended_triggers += 1;
        }
    }
    assert_eq!(
        conversation_ended_triggers, 1,
        "atomicity guard must suppress duplicate ConversationEnded triggers"
    );

    shutdown_token.cancel();
}

/// #1052 regression: when a DM run is cancelled mid-`ignore_message`-unwind,
/// the post-`Ok`-arm bookkeeping in `execute_run` must NOT fire — the peer
/// already received `run_cancelled` (or will receive it from the cancel
/// arm), and emitting `dm_conversation_ended` with reason `"ignored"`
/// (the `Display`-format of `ConversationEndReason::Ignored`) here would
/// conflate an operator-driven cancel with an agent-driven ignore,
/// corrupting the peer's conversation-state model. (The issue body
/// paraphrased the reason as `"ignore_message"`; the wire-format reason
/// is `"ignored"` per `ConversationEndReason::Display`. The conflation
/// concern is identical regardless of the spelling.)
///
/// The contract this test pins:
///
/// 1. `mark_run_as_completed` is now bool-returning. When the run is
///    already `Cancelled` (because a racing cancel path drove it there),
///    the call returns `false` and the existing `Cancelled` status is
///    preserved (the transition is idempotent — see
///    `test_mark_run_terminal_is_idempotent` in `run_manager.rs` for the
///    data-layer contract).
///
/// 2. The lifecycle layer's `Ok` arm in `execute_run` reads that bool
///    and gates `handle_dm_run_completion` on it. This test exercises
///    the gate at the call site: we drive the exact ordering the racing
///    `Ok` arm sees (`mark_run_as_cancelled` already won, then
///    `mark_run_as_completed` runs), and verifies that:
///
///    - `mark_run_as_completed` returns `false`.
///    - When the caller honours the gate (skips
///      `handle_dm_run_completion`), no `dm_conversation_ended` SSE
///      event is emitted on the DM session feed.
///    - For contrast, if we ignored the gate and called
///      `handle_dm_run_completion` anyway with `ignore_message` in the
///      tool-call record set, it WOULD emit `dm_conversation_ended` —
///      proving the gate is what prevents the double-emit, not some
///      other coincidence in the test setup.
///
/// Driving the full `execute_run` race deterministically would require an
/// LLM stub that returns `Ok` carrying an `ignore_message` tool-call
/// record while a sibling task races a cancel in between the loop's
/// return and `execute_run`'s `Ok` arm — feasible but flaky. The
/// gate-at-the-call-site test pins the contract just as tightly with
/// zero scheduling sensitivity. The full-flow scenario is covered
/// indirectly by code review of the `Ok` arm: the only path to
/// `handle_dm_run_completion` is gated on `completed_transitioned`,
/// which is `false` in this test.
#[tokio::test]
async fn handle_dm_run_completion_gated_when_cancel_wins_race() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open a DM conversation: alice → bob. This creates the shared DM
    // session and bumps the depth counter, so the subsequent
    // `end_conversation` path has something to remove (and so the
    // `dm_ended` marker write is reachable — `end_conversation` aborts
    // early if the session doesn't exist).
    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    // Drain the trigger emitted by `send` so subsequent assertions are
    // scoped to the post-cancel side effects only.
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Insert bob's peer-triggered run (the run that races the cancel).
    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Simulate the race: a concurrent path (cancel handler, shutdown
    // drain, `cancel_runs_for_session`, ...) drives the run to
    // `Cancelled` BEFORE the Ok arm processes the loop result.
    assert!(
        state.run_manager.mark_run_as_cancelled(run_id),
        "first mark_run_as_cancelled must transition the run"
    );

    // Subscribe to the DM session AFTER the cancel transition so we can
    // pin "no `dm_conversation_ended` SSE on the gated path." A
    // pre-fix run would emit one here.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Now the racing Ok arm tries to mark Completed — the bool gate must
    // return `false`, the status must stay `Cancelled`, and the caller
    // must skip `handle_dm_run_completion` (the lifecycle layer in
    // `lifecycle.rs` does this; we replicate the contract here).
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        String::new(),
        alms_core::TokenUsage::default(),
    );
    assert!(
        !completed_transitioned,
        "mark_run_as_completed must return false when state is already terminal"
    );
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled,
        "Cancelled status must NOT be clobbered to Completed"
    );

    // Build the same tool-call record set the cancelled `ignore_message`
    // run would have produced. `should_signal_dm_end` is satisfied by
    // this shape — if we were to call `handle_dm_run_completion` here,
    // it would emit `dm_conversation_ended` with reason
    // `"ignore_message"`. The gate must prevent the call.
    let ignore_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];
    // Pin the contrast: these records WOULD satisfy
    // `should_signal_dm_end` if the gate weren't there. The
    // `should_signal_dm_end` function is the gating logic inside
    // `handle_dm_run_completion` itself — and it returns `true` for
    // this shape, which is exactly why the call-site gate is necessary.
    assert!(
        crate::runs::dm_lifecycle::should_signal_dm_end(true, &ignore_records, dm_context),
        "tool-call record set must trigger `dm_conversation_ended` if \
         `handle_dm_run_completion` is called — proving the gate is what \
         prevents the double-emit, not some other accident in this setup"
    );

    // The Ok arm honours the gate: when `completed_transitioned` is
    // false, `handle_dm_run_completion` is NOT called. We do not call
    // it here either — this is the contract being verified.

    // Assert: no `dm_conversation_ended` SSE landed on the DM session
    // feed. Any other events that flowed through (e.g. from the depth
    // bookkeeping triggered by the `send` above) are allowed; only the
    // post-Ok-arm `dm_conversation_ended` is forbidden.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == "dm_conversation_ended"),
        "no `dm_conversation_ended` SSE event must be emitted when the cancel \
         path wins the race against an `ignore_message`-emitting `Ok` arm; \
         got events: {:?}",
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
    );

    shutdown_token.cancel();
}

/// Companion test to `handle_dm_run_completion_gated_when_cancel_wins_race`:
/// when the `Ok` arm transitions cleanly (no racing cancel), the gate
/// passes and `handle_dm_run_completion` DOES emit
/// `dm_conversation_ended`. Pins the happy-path side of the gate so a
/// future refactor that "simplifies" the gate by inverting the
/// condition gets caught by both tests.
#[tokio::test]
async fn handle_dm_run_completion_fires_when_completed_transition_wins() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _receipt = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _initial_trigger = tr.try_recv().expect("send should emit a trigger");

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // No racing cancel — the Ok arm wins.
    let completed_transitioned = state.run_manager.mark_run_as_completed(
        run_id,
        String::new(),
        alms_core::TokenUsage::default(),
    );
    assert!(
        completed_transitioned,
        "mark_run_as_completed must return true on the first call from Running"
    );

    let ignore_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("ignore_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: Some(r#"{"ok":true}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];

    // Mirror the lifecycle layer's behaviour: gate passes, so call
    // `handle_dm_run_completion`.
    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &ignore_records,
            response: "",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        crate::runs::dm_lifecycle::DmRunExit::Ended,
        "handle_dm_run_completion must take the Ended exit for a peer-DM \
         run that called ignore_message"
    );

    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .unwrap_or_else(|| {
            panic!(
                "happy path must emit dm_conversation_ended; got events: {:?}",
                events.iter().map(|e| &e.event_type).collect::<Vec<_>>(),
            )
        });
    // `ConversationEndReason::Ignored` serialises to `"ignored"` (see
    // `alms-tools/src/message_sender.rs`); the SSE event renders the
    // `Display`-format string.
    assert_eq!(
        dm_ended
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "ignored",
        "reason must be `ignored` (Display of ConversationEndReason::Ignored) \
         on the happy path"
    );

    shutdown_token.cancel();
}

/// #1052 review (Tim): pin the contract that `handle_dm_run_failure`
/// fires from the terminal Err arm REGARDLESS of whether
/// `mark_run_as_cancelled` returned `true` or `false`.
///
/// Per #1050's design, `handle_dm_run_failure` is an independent side
/// effect: the synchronous HTTP cancel handler from #1050 flips state
/// and broadcasts `run_cancelled` itself, but it does NOT call
/// `handle_dm_run_failure` — it relies on the terminal Err(Cancelled)
/// arm in `execute_run` to do that. If the terminal arm were to gate
/// the call on `cancelled_transitioned`, then when the HTTP cancel
/// won the race the bool would be `false` and the DM peer would
/// never receive the `ConversationEnded` peer notification, the depth
/// counter would never be reset, and no `dm_ended` marker would land.
///
/// This test simulates the race: a concurrent path drives the run to
/// `Cancelled` first (so the subsequent `mark_run_as_cancelled` call
/// returns `false`), then calls `handle_dm_run_failure` directly (as
/// the terminal arm does, post-fix). The peer-notification side
/// effects (`ConversationEnded` `RunTrigger`, `dm_ended` marker,
/// `dm_conversation_ended` SSE) MUST all land.
#[tokio::test]
async fn handle_dm_run_failure_fires_when_cancel_transition_already_lost() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open the DM and drain the initial trigger from `send`.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Insert bob's peer-triggered run.
    let run = Run::new(dm_session_id, bob_id, "received a DM".into());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run);
    state.run_manager.mark_run_as_running(run_id);

    // Simulate the race: an external path (the synchronous HTTP cancel
    // handler from #1050, a sibling shutdown drain, etc.) has already
    // driven the run to `Cancelled` before the terminal Err arm runs.
    assert!(
        state.run_manager.mark_run_as_cancelled(run_id),
        "first mark_run_as_cancelled must transition"
    );

    // Subscribe AFTER the racing cancel so the test only observes the
    // post-terminal-arm side effects.
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // The terminal Err(Cancelled) arm now runs. Its `mark_run_as_cancelled`
    // call returns `false` — the SSE broadcast is correctly skipped (the
    // external path already emitted `run_cancelled`).
    let cancelled_transitioned = state.run_manager.mark_run_as_cancelled(run_id);
    assert!(
        !cancelled_transitioned,
        "second mark_run_as_cancelled must return false when state is already terminal"
    );

    // CONTRACT: `handle_dm_run_failure` MUST still fire. The terminal arm
    // calls it unconditionally — it is an independent side effect per
    // #1050, NOT something to skip when the state-flip lost the race.
    crate::runs::dm_lifecycle::handle_dm_run_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        Some("bob"),
        dm_context,
        true,
        ConversationEndReason::UserCancelled,
    )
    .await
    .expect("handle_dm_run_failure must succeed even when cancel transition was lost");

    // Assert: dm_ended marker landed. Without this, Alice's session UI
    // shows the DM as still open until the 1800s `DEPTH_EXPIRY_SECS`
    // sweep clears the depth counter.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("reason"))
                .and_then(|v| v.as_str())
                == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled MUST be persisted even when \
         the state-flip race was lost — this is the contract that prevents \
         stranded DM peers when the HTTP cancel handler wins"
    );

    // Assert: ConversationEnded RunTrigger fired so Alice's notifications
    // session learns about the end.
    let trigger = tr
        .try_recv()
        .expect("ConversationEnded RunTrigger MUST fire even on lost cancel race");
    match trigger.source {
        MessageSource::ConversationEnded { reason, .. } => {
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "trigger reason must be UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // Assert: dm_conversation_ended SSE landed on the DM session feed.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    let dm_ended = events
        .iter()
        .find(|e| e.event_type == "dm_conversation_ended")
        .expect("dm_conversation_ended SSE MUST fire even when the state-flip race was lost");
    assert_eq!(
        dm_ended
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "user_cancelled",
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// DM completion gate tests (#1154 implicit replies)
// ---------------------------------------------------------------------------

/// Implicit reply happy path: a peer-triggered DM run that completes with
/// plain final text — and NO `send_message` call — must have that text
/// delivered to the peer: persisted to the shared DM session with
/// `message_type: "dm"` metadata and a `RunTrigger` emitted for the peer.
#[tokio::test]
async fn dm_completion_gate_delivers_final_text() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice opens the DM; drain her trigger.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // Bob's run completed with plain text and no tool calls.
    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: "Hello Alice, pong!",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        crate::runs::dm_lifecycle::DmRunExit::Delivered,
        "plain final text must take the Delivered exit"
    );

    // The reply is persisted to the shared DM session as a real DM message.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    let delivered = history.iter().find(|m| {
        m.metadata.as_ref().is_some_and(|meta| {
            meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
        })
    });
    let delivered = delivered.expect("bob's implicit reply must be persisted as a DM message");
    assert!(
        matches!(&delivered.content, alms_session::Content::Text(t) if t == "Hello Alice, pong!"),
        "persisted DM message must carry the final assistant text"
    );

    // The peer is triggered with the reply text.
    let trigger = tr
        .try_recv()
        .expect("peer must be triggered by the delivery");
    assert_eq!(trigger.agent_id, alice_id);
    assert_eq!(trigger.input, "Hello Alice, pong!");
    assert!(
        matches!(trigger.source, MessageSource::Agent { ref from_name, .. } if from_name == "bob"),
        "trigger source must attribute the reply to bob"
    );

    shutdown_token.cancel();
}

/// B1 regression: a run whose only action was a FAILED `send_message`
/// (bad recipient → soft-error tool result) and no final text must take
/// the **errored** exit — the peer gets a `dm_ended` notification instead
/// of waiting forever. Pre-#1154, the presence-only termination check
/// completed the run as if the reply had been delivered.
#[tokio::test]
async fn dm_completion_gate_failed_send_no_longer_silently_completes() {
    use alms_core::ToolCallRole;

    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // Bob's run called send_message with a bad recipient (soft error
    // result) and produced no final text.
    let failed_send_records = vec![
        alms_core::ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: Some(r#"{"to":"nonexistent","message":"hi"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
        alms_core::ToolCallRecord {
            seq: 1,
            role: ToolCallRole::Tool,
            tool_name: Some("send_message".to_string()),
            tool_id: Some("call_1".to_string()),
            tool_invocation_id: None,
            params: None,
            result: Some(r#"{"error":"Agent not found."}"#.to_string()),
            timestamp: chrono::Utc::now(),
            from_agent: None,
        },
    ];

    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &failed_send_records,
            response: "",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(
        exit,
        crate::runs::dm_lifecycle::DmRunExit::Errored,
        "failed send_message + no final text must take the Errored exit, \
         not silently complete"
    );

    // The peer is notified via the ConversationEnded trigger...
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(
        matches!(
            trigger.source,
            MessageSource::ConversationEnded {
                reason: ConversationEndReason::Errored {
                    interrupted: false,
                    ..
                },
                ..
            }
        ),
        "trigger must carry the Errored reason, classified as NOT interrupted \
         — this run COMPLETED, it just had nothing deliverable on its last \
         turn, so its transcript still has to reach the operator's chat \
         (#1258); got {:?}",
        trigger.source
    );

    // ...and the dm_ended marker lands in the session.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        }),
        "dm_ended marker must be persisted on the errored exit"
    );

    shutdown_token.cancel();
}

/// Design default #3: an empty / whitespace-only final text with no end
/// tool takes the **errored** exit (the runtime's bounded nudge already
/// ran) — no empty message is delivered, and the peer is notified.
#[tokio::test]
async fn dm_completion_gate_empty_reply_ends_with_error() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: "   \n",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, crate::runs::dm_lifecycle::DmRunExit::Errored);

    // No DM message from bob may have been delivered.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        !history.iter().any(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                    && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
            })
        }),
        "no (empty) DM message may be delivered to the peer"
    );

    // The peer is notified via the Errored conversation end.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(matches!(
        trigger.source,
        MessageSource::ConversationEnded {
            reason: ConversationEndReason::Errored { .. },
            ..
        }
    ));

    shutdown_token.cancel();
}

/// The `[Thinking]`-only promotion fallback (`response == reasoning`,
/// #1098) is NOT a deliverable reply: the gate must take the errored exit
/// rather than deliver a reasoning trace to the peer.
#[tokio::test]
async fn dm_completion_gate_promoted_reasoning_not_delivered() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    let trace = "Let me think about whether this ping needs a response...";
    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: dm_context,
            is_peer_message: true,
            tool_calls: &[],
            response: trace,
            reasoning: Some(trace),
        },
    )
    .await;
    assert_eq!(
        exit,
        crate::runs::dm_lifecycle::DmRunExit::Errored,
        "a promoted reasoning trace must NOT be delivered as a reply"
    );

    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        !history.iter().any(|m| {
            m.metadata.as_ref().is_some_and(|meta| {
                meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                    && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
            })
        }),
        "the reasoning trace must not land in the DM session as a message"
    );

    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the errored end");
    assert!(matches!(
        trigger.source,
        MessageSource::ConversationEnded {
            reason: ConversationEndReason::Errored { .. },
            ..
        }
    ));

    shutdown_token.cancel();
}

/// Non-peer-triggered runs on a DM session (and peer runs on non-DM
/// sessions) are outside the gate: NotPeerDm, no side effects.
#[tokio::test]
async fn dm_completion_gate_not_peer_dm_is_noop() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // Peer flag off → gate does not apply even on a dm: context.
    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id: RunId::new(),
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: "dm:alice:bob",
            is_peer_message: false,
            tool_calls: &[],
            response: "hello",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, crate::runs::dm_lifecycle::DmRunExit::NotPeerDm);

    // dm: prefix missing → gate does not apply even for peer messages.
    let exit = crate::runs::dm_lifecycle::handle_dm_run_completion(
        crate::runs::dm_lifecycle::DmRunCompletionContext {
            state: &state,
            run_id: RunId::new(),
            session_id: dm_session_id,
            agent_id: bob_id,
            agent_name: Some("bob"),
            context_id: "web-chat-1234",
            is_peer_message: true,
            tool_calls: &[],
            response: "hello",
            reasoning: None,
        },
    )
    .await;
    assert_eq!(exit, crate::runs::dm_lifecycle::DmRunExit::NotPeerDm);

    assert!(
        tr.try_recv().is_err(),
        "NotPeerDm exits must produce no triggers"
    );

    shutdown_token.cancel();
}

/// Option C (#1156, Tim's review on the #1154 PR): DM sessions are
/// agent-to-agent only. `POST /runs` always enqueues with
/// `is_peer_message: false`, so a run created through it on a `dm:`
/// session would arm the implicit-reply machinery (DM recipient prompt +
/// send_message peer-fold) while the completion gate refuses delivery
/// (`NotPeerDm`) — a guaranteed silent drop. The handler must reject with
/// a structured 400 instead.
#[tokio::test]
async fn create_run_rejects_dm_session_with_structured_400() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let session = state
        .session_manager
        .get_or_create_shared(dm_session_id, dm_context);

    let req = CreateRunRequest {
        session_id: session.id,
        // Shared DM sessions require an agent_id on the request; supply
        // one so the rejection under test (and not AGENT_ID_REQUIRED) is
        // what fires.
        agent_id: Some(AgentId::new()),
        input: RunInput::Text { text: "hi".into() },
    };

    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject non-peer runs on dm: sessions (#1156)");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        body.0["error_code"], "DM_SESSION_NOT_DIRECTLY_RUNNABLE",
        "rejection must carry the structured error code; got {:?}",
        body.0
    );
    assert_eq!(body.0["context_id"], dm_context);

    // The rejection must fire BEFORE any side effects: no run recorded,
    // no user input pre-persisted to the DM session.
    assert!(
        state.run_manager.list_by_session(session.id, 10).is_empty(),
        "no run may be created on the rejected path"
    );
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "no input may be persisted on the rejected path"
    );

    shutdown_token.cancel();
}

/// #1289 item 3: `POST /runs` on a subagent session is rejected on
/// principle, not incidentally.
///
/// Subagent turns come from `invoke_agent` -> `SubagentDispatcher` ->
/// `run_subagent_loop`, which alone records the `parent_run_id` linkage
/// and returns the result to the awaiting parent. A run created here
/// would write into a coordinator-owned transcript and deliver to nobody.
///
/// The request deliberately carries **no** `agent_id`. That is the case
/// the old incidental `AGENT_SESSION_MISMATCH` never covered — with
/// `agent_id` omitted the handler took the session's own id and proceeded,
/// before #1278 as well as after — so a guard that only fired on a
/// supplied-and-differing `agent_id` would leave this test red.
#[tokio::test]
async fn create_run_rejects_subagent_session_with_structured_400() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    // #1278 keying: filed under the INVOKED agent's registry id, context
    // naming the invoking parent.
    let invoked = AgentId::new();
    let parent = AgentId::new();
    let context_id = format!("subagent_{}_reviewer", parent.0);
    let session = state.session_manager.get_or_create(invoked, &context_id);

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: None,
        input: RunInput::Text { text: "hi".into() },
    };

    let Err((status, body)) =
        crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await
    else {
        panic!("create_run must reject operator runs on subagent sessions (#1289)");
    };
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        body.0["error_code"], "SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE",
        "rejection must carry the structured error code; got {:?}",
        body.0
    );
    assert_eq!(body.0["context_id"], context_id);

    // The rejection must fire BEFORE any side effect, exactly like the DM
    // guard it sits next to: no run recorded, no user input pre-persisted
    // into a transcript a live coordinator loop may be writing.
    assert!(
        state.run_manager.list_by_session(session.id, 10).is_empty(),
        "no run may be created on the rejected path"
    );
    assert!(
        state
            .session_manager
            .get_history(session.id)
            .unwrap()
            .is_empty(),
        "no input may be persisted on the rejected path"
    );

    shutdown_token.cancel();
}

/// The guard keys on the `subagent_` prefix, not on a successful parse,
/// so every shape the coordinator has ever minted is covered: the
/// ephemeral `subagent_{parent}_{task_id}`, and the legacy pre-#1185
/// `subagent_{task_id}` that carries no parent segment and which
/// `parse_subagent_context` returns `None` for. A guard written on the
/// parse would let the legacy shape through.
#[tokio::test]
async fn create_run_rejects_every_subagent_context_shape() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();
    let parent = AgentId::new();

    for context_id in [
        format!("subagent_{}_{}", parent.0, uuid::Uuid::new_v4()),
        format!("subagent_{}", uuid::Uuid::new_v4()),
    ] {
        let session = state
            .session_manager
            .get_or_create(AgentId::new(), &context_id);
        let req = CreateRunRequest {
            session_id: session.id,
            agent_id: None,
            input: RunInput::Text { text: "hi".into() },
        };
        let Err((status, body)) =
            crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await
        else {
            panic!("create_run must reject the subagent context {context_id}");
        };
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            body.0["error_code"], "SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE",
            "{context_id} must be rejected by the subagent guard; got {:?}",
            body.0
        );
    }

    shutdown_token.cancel();
}

/// Positive control on the guard's width. `subagent_` is a prefix test,
/// not a substring one: an ordinary chat whose `context_id` merely
/// contains the word must still be runnable, or the guard has rotted into
/// "reject anything that mentions subagents".
///
/// `classify_session_type` already draws this line — the assertion is
/// here so that relaxing the guard to `contains` is a red test rather
/// than a silent outage of every session with an unlucky name.
#[tokio::test]
async fn create_run_admits_a_chat_session_whose_context_merely_mentions_subagents() {
    use alms_core::{CreateRunRequest, RunInput};
    use axum::Json;
    use axum::extract::State;

    let (state, shutdown_token, _cr, _tr, _dr) = test_app_state();

    let agent_id = AgentId::new();
    let session = state
        .session_manager
        .get_or_create(agent_id, "notes-about-subagent_design");

    let req = CreateRunRequest {
        session_id: session.id,
        agent_id: Some(agent_id),
        input: RunInput::Text { text: "hi".into() },
    };
    let result = crate::runs::lifecycle::create_run(State(state.clone()), Json(req)).await;

    match result {
        Ok((status, _)) => assert_eq!(status, axum::http::StatusCode::CREATED),
        Err((status, body)) => panic!(
            "an ordinary chat session must not hit the subagent guard; \
             got {status:?} {:?}",
            body.0
        ),
    }

    shutdown_token.cancel();
}

/// Companion to `create_run_rejects_dm_session_with_structured_400`: the
/// peer-trigger path (`MessageBus` -> `RunTrigger` -> `run_trigger_loop`,
/// which enqueues with `is_peer_message: true`) must be UNAFFECTED by the
/// Option C rejection — it never goes through `create_run`. End-to-end
/// against the mock LLM: the triggered run is created, executes, and the
/// DM completion gate delivers bob's implicit reply to alice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_triggered_dm_run_is_not_rejected_and_delivers() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_mock_llm();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Alice DMs bob through the real MessageBus — persists the message to
    // the shared DM session and emits the RunTrigger the gateway's
    // trigger loop would normally consume.
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let trigger = tr
        .try_recv()
        .expect("MessageBus must emit a RunTrigger for bob");
    assert_eq!(trigger.agent_id, bob_id);

    // Feed the trigger through the actual run_trigger_loop.
    let (test_tx, test_rx) = mpsc::channel(8);
    test_tx.send(trigger).await.unwrap();
    drop(test_tx);
    crate::runs::notifications::run_trigger_loop(test_rx, state.clone()).await;

    let dm_session_id = SessionId::deterministic_dm("alice", "bob");

    // The run was created — NOT rejected.
    let runs = state.run_manager.list_by_session(dm_session_id, 10);
    assert!(
        !runs.is_empty(),
        "peer-triggered DM run must be created on the trigger path"
    );

    // ...and executes to completion: the mock LLM's reply is delivered to
    // the DM session by the completion gate. Poll with a timeout — the
    // run executes on the agent queue's spawned task.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let delivered = loop {
        let history = state.session_manager.get_history(dm_session_id).unwrap();
        let found = history
            .iter()
            .find(|m| {
                m.metadata.as_ref().is_some_and(|meta| {
                    meta.get("from_agent").and_then(|v| v.as_str()) == Some("bob")
                        && meta.get("message_type").and_then(|v| v.as_str()) == Some("dm")
                })
            })
            .cloned();
        if let Some(msg) = found {
            break msg;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for bob's implicit reply to be delivered; \
             history: {history:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(
        matches!(&delivered.content, alms_session::Content::Text(t) if t.contains("[mock]")),
        "delivered DM message must carry the mock LLM's reply text; got {delivered:?}"
    );

    // The delivery re-triggers alice (normal DM ping-pong), proving the
    // full MessageBus::send shape was used — not a bare session write.
    let alice_trigger = tokio::time::timeout(std::time::Duration::from_secs(5), tr.recv())
        .await
        .expect("alice's trigger must arrive after the delivery")
        .expect("trigger channel must stay open");
    assert_eq!(alice_trigger.agent_id, alice_id);
    assert!(
        matches!(alice_trigger.source, MessageSource::Agent { ref from_name, .. } if from_name == "bob"),
        "alice's trigger must attribute the reply to bob; got {:?}",
        alice_trigger.source
    );

    shutdown_token.cancel();
}

/// B4 regression: a peer-triggered DM run that dies on a pre-loop setup
/// failure (e.g. `resolve_agent_config` error — BEFORE the agent name is
/// known) must still notify the peer. The helper re-resolves the agent
/// name from the registry by ID.
#[tokio::test]
async fn dm_setup_failure_notifies_peer_without_agent_name() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv();

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let run_id = RunId::new();

    // agent_name: None — the resolve-failure arm fires before the registry
    // record is loaded; the helper must resolve "bob" by agent_id.
    crate::runs::dm_lifecycle::notify_dm_peer_of_setup_failure(
        &state,
        &run_id,
        &dm_session_id,
        bob_id,
        None,
        dm_context,
        true,
        "failed to resolve agent config".to_string(),
    )
    .await;

    // Peer notified with the Errored reason.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the setup failure");
    assert_eq!(trigger.agent_id, alice_id);
    match trigger.source {
        MessageSource::ConversationEnded {
            ref from_name,
            ref reason,
            ..
        } => {
            assert_eq!(from_name, "bob", "helper must resolve the agent name by ID");
            assert!(
                matches!(
                    reason,
                    ConversationEndReason::Errored {
                        message,
                        interrupted: true,
                    } if message.contains("failed to resolve agent config")
                ),
                "reason must carry the setup-failure message, classified as an \
                 interrupted end — the run never started its loop, so no turn \
                 of this DM completed (#1258); got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // dm_ended marker persisted.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
        }),
        "dm_ended marker must be persisted on setup failure"
    );

    shutdown_token.cancel();
}

/// Non-DM / non-peer setup failures are outside the helper: no triggers,
/// no markers.
#[tokio::test]
async fn dm_setup_failure_noop_outside_peer_dm() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    crate::runs::dm_lifecycle::notify_dm_peer_of_setup_failure(
        &state,
        &RunId::new(),
        &SessionId::new(),
        bob_id,
        Some("bob"),
        "web-chat-1234",
        false,
        "boom".to_string(),
    )
    .await;

    assert!(
        tr.try_recv().is_err(),
        "non-peer-DM setup failures must not emit triggers"
    );

    shutdown_token.cancel();
}

/// S1 (#1154): a peer-triggered DM run cancelled while still `Queued`
/// (HTTP cancel / #1109 deny cascade / session-cancel / shutdown) reaches
/// `execute_run`'s pre-loop early-exit. That exit must notify the DM peer
/// with `UserCancelled` — otherwise the peer is stranded on "Chatting
/// with…" until the 1800s depth-expiry sweep, because neither the
/// synchronous `cancel_run` handler nor the early-exit historically
/// signalled the peer.
#[tokio::test]
async fn queued_then_cancelled_dm_run_notifies_peer() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);

    // Open the DM: alice -> bob. This creates the shared DM session and the
    // depth entry, and emits alice's RunTrigger for bob (which we discard —
    // we drive bob's run manually below).
    let _ = state
        .message_bus
        .send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    let _ = tr.try_recv(); // discard alice->bob trigger

    let dm_context = "dm:alice:bob";
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    let mut dm_rx = subscribe_session(&state, dm_session_id);

    // Bob's peer-triggered DM run, inserted as Queued, then cancelled while
    // still queued (mirrors the HTTP cancel / deny cascade landing before the
    // queue dispatches the work item).
    let run = Run::new(dm_session_id, bob_id, "ping".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    // The per-agent queue eventually dispatches the work item; `execute_run`
    // hits the pre-loop early-exit (token already cancelled).
    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id: dm_session_id,
            agent_id: bob_id,
            input: run.input,
            context_id: dm_context.to_string(),
            cancel_token,
            is_peer_message: true,
            is_system_triggered: true,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    // The run must be Cancelled (never auto-started).
    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled,
        "queued-then-cancelled run must stay Cancelled"
    );

    // The peer (alice) must be notified with UserCancelled — the S1 fix.
    let trigger = tr
        .try_recv()
        .expect("peer must be notified of the queued-cancel (S1)");
    assert_eq!(trigger.agent_id, alice_id, "notification targets the peer");
    match trigger.source {
        MessageSource::ConversationEnded {
            ref from_name,
            ref reason,
            ..
        } => {
            assert_eq!(from_name, "bob", "ended-by is the cancelled agent");
            assert!(
                matches!(reason, ConversationEndReason::UserCancelled),
                "queued-cancel must signal UserCancelled; got {reason:?}"
            );
        }
        other => panic!("expected ConversationEnded source, got {other:?}"),
    }

    // dm_ended marker with reason=user_cancelled persisted to the DM session.
    let history = state.session_manager.get_history(dm_session_id).unwrap();
    assert!(
        history.iter().any(|m| {
            let meta = m.metadata.as_ref();
            meta.and_then(|x| x.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm_ended")
                && meta.and_then(|x| x.get("reason")).and_then(|v| v.as_str())
                    == Some("user_cancelled")
        }),
        "dm_ended marker with reason=user_cancelled must be persisted"
    );

    // dm_conversation_ended SSE emitted on the DM session stream.
    tokio::task::yield_now().await;
    let events = drain_events(&mut dm_rx);
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "dm_conversation_ended"),
        "expected dm_conversation_ended SSE on the DM session stream"
    );

    shutdown_token.cancel();
}

/// S1 (#1154) negative: a NON-peer run cancelled while queued (e.g. a
/// user-initiated `POST /runs` run) must NOT emit any DM peer notification —
/// the helper is gated on `is_peer_message && dm:`.
#[tokio::test]
async fn queued_then_cancelled_non_peer_run_does_not_notify() {
    let (state, shutdown_token, _cr, mut tr, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);

    let session = state.session_manager.get_or_create(bob_id, "web-chat-1");
    let session_id = session.id;

    let run = Run::new(session_id, bob_id, "hello".to_string());
    let run_id = run.run_id;
    let _ = state.run_manager.insert_run(run.clone());
    let cancel_token = CancellationToken::new();
    state
        .run_manager
        .register_cancel_token(run_id, cancel_token.clone());
    cancel_token.cancel();

    crate::runs::lifecycle::execute_run(
        state.clone(),
        crate::runs::RunParams {
            run_id,
            session_id,
            agent_id: bob_id,
            input: run.input,
            context_id: "web-chat-1".to_string(),
            cancel_token,
            // Not a peer message — the user cancelled their own run.
            is_peer_message: false,
            is_system_triggered: false,
            input_pre_persisted: false,
            dm_ended_peer: None,
        },
    )
    .await;

    assert_eq!(
        state.run_manager.get_run(run_id).unwrap().status(),
        RunStatus::Cancelled
    );
    assert!(
        tr.try_recv().is_err(),
        "non-peer queued-cancel must not emit a DM peer notification"
    );

    shutdown_token.cancel();
}

// ---------------------------------------------------------------------------
// #1299: the post-end turn cannot re-open the conversation that just ended
// ---------------------------------------------------------------------------
//
// `MAX_DM_DEPTH` bounds ONE conversation, and `end_conversation_locked`
// clears `depths` and `expired_pairs` — so the next `send_message` between
// the same two agents restarts at depth 1. The post-end turn is where that
// re-entry is immediate and unattended: the agent has just been handed the
// transcript of the conversation that ended and is one `send_message` away
// from starting it again.
//
// `ConversationEnded` runs are `is_peer_message = false`, so before #1299
// `SendMessageTool` was registered with no fold at all. The fix gives them a
// fold of their own, keyed on the ended peer.
//
// These rows pin `apply_send_message_fold` — the whole registration decision
// as `execute_run` makes it — by *executing* the tool it hands to the runtime
// against the real `MessageBus`. "Not delivered" is therefore observed twice:
// in the tool's own result, and in the absence of the `RunTrigger` that would
// have invoked the peer and re-opened the pair.

/// Build the `send_message` tool exactly as `execute_run` would for a run
/// with the given `(is_peer_message, context_id, dm_ended_peer)`.
fn send_tool_for_run(
    state: &AppState,
    agent_id: AgentId,
    agent_name: &str,
    session_id: SessionId,
    is_peer_message: bool,
    context_id: &str,
    dm_ended_peer: Option<&str>,
) -> alms_tools::SendMessageTool {
    let sender: Arc<dyn MessageSender> = state.message_bus.clone();
    crate::runs::lifecycle::apply_send_message_fold(
        alms_tools::SendMessageTool::new(
            sender,
            agent_id,
            agent_name.to_string(),
            state.session_manager.clone(),
            session_id,
        ),
        is_peer_message,
        context_id,
        agent_name,
        dm_ended_peer,
    )
}

/// The acceptance criterion: a `ConversationEnded` run cannot `send_message`
/// the peer whose conversation just ended.
///
/// The mutation this fails on is restoring the unfolded registration — the
/// pre-#1299 `else { None }` arm in `apply_send_message_fold`. Its exact
/// effect is the control row below.
#[tokio::test]
async fn conversation_ended_run_cannot_send_message_to_the_ended_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    // Bob's post-end turn lands on his notifications session (the
    // source-less routing) and his conversation with alice has just ended.
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false, // ConversationEnded runs are NOT peer messages
        "notifications:bob",
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "one more thing" }))
        .await
        .expect("the fold is a non-error result — the agent must not retry");

    assert_eq!(result["folded"], true);
    assert_eq!(result["delivered"], false);
    assert!(
        trigger_rx.try_recv().is_err(),
        "no RunTrigger may be emitted: a delivered send invokes alice and \
         re-opens the pair at depth 1, which is the unbounded loop #1299 closes"
    );

    shutdown_token.cancel();
}

/// Control row for the one above: with no ended peer the same send DOES
/// deliver and DOES re-open the conversation.
///
/// This is what the post-end turn did before #1299 — asserted here so the
/// fold row above cannot pass for the wrong reason (a `MessageBus` that
/// silently refused to deliver in this harness would make it vacuous).
#[tokio::test]
async fn run_with_no_ended_peer_still_delivers_to_that_agent() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (alice_id, bob_id) = seed_alice_bob(&state);
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        "notifications:bob",
        None, // no conversation ended — nothing to fold
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "one more thing" }))
        .await
        .expect("an ordinary send must succeed");

    assert_eq!(result["delivered"], true);
    assert!(result.get("folded").is_none());
    let trigger = trigger_rx
        .try_recv()
        .expect("an unfolded send invokes the recipient — this is the re-open");
    assert_eq!(trigger.agent_id, alice_id);

    shutdown_token.cancel();
}

/// The post-end turn keeps every other capability it has today: the fold
/// removes exactly one recipient, not `send_message` itself. Reporting the
/// outcome to a third agent is one of the things the turn exists for.
#[tokio::test]
async fn conversation_ended_run_keeps_send_message_for_third_parties() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let carol_id = seed_agent(&state, "carol");
    let session = state
        .session_manager
        .get_or_create(bob_id, "notifications:bob");

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        "notifications:bob",
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "carol", "message": "alice and I are done" }))
        .await
        .expect("a send to a third agent must succeed");

    assert_eq!(
        result["delivered"], true,
        "only the ended peer is folded — the turn can still report elsewhere"
    );
    let trigger = trigger_rx
        .try_recv()
        .expect("the third party must actually be invoked");
    assert_eq!(trigger.agent_id, carol_id);

    shutdown_token.cancel();
}

/// The #1198 / #1205 job-episode arm.
///
/// When the end resolves an open job episode, `run_trigger_loop` reroutes the
/// continuation onto the agent's `job_*` session — and that arm is also the
/// one that survives the #1258 interrupted-end suppression, so it can be the
/// ONLY run an end produces. The fold is keyed on the ended peer carried by
/// the trigger, not on `context_id`, precisely so it still applies there: a
/// `job_*` context names no peer, and this is the arm that re-opens with
/// nobody watching.
#[tokio::test]
async fn job_episode_continuation_after_a_dm_end_still_folds_the_ended_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let job_context = format!("job_{}", uuid::Uuid::new_v4());
    let session = state.session_manager.get_or_create(bob_id, &job_context);

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        false,
        &job_context,
        Some("alice"),
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "resuming — one more ask" }))
        .await
        .expect("the fold is a non-error result");

    assert_eq!(
        result["folded"], true,
        "a job-episode continuation of an ended DM must not re-open it either"
    );
    assert!(trigger_rx.try_recv().is_err());

    shutdown_token.cancel();
}

/// The #1154 arm is unchanged: a live peer-triggered DM turn still folds its
/// current peer, read off the `dm:` context id, with `dm_ended_peer` unset.
#[tokio::test]
async fn peer_dm_turn_still_folds_its_current_peer() {
    use alms_tools::Tool;
    let (state, shutdown_token, _cr, mut trigger_rx, _dr) = test_app_state_with_sqlite();
    let (_alice_id, bob_id) = seed_alice_bob(&state);
    let dm_context = alms_core::dm_context_id("alice", "bob");
    let session = state.session_manager.get_or_create(bob_id, &dm_context);

    let tool = send_tool_for_run(
        &state,
        bob_id,
        "bob",
        session.id,
        true, // peer-triggered DM turn
        &dm_context,
        None,
    );

    let result = tool
        .execute(serde_json::json!({ "to": "alice", "message": "replying" }))
        .await
        .expect("the fold is a non-error result");

    assert_eq!(result["folded"], true);
    assert!(trigger_rx.try_recv().is_err());

    shutdown_token.cancel();
}
