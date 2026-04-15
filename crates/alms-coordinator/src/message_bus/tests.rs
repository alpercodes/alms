use super::*;
use alms_core::{AgentId, SessionId, dm_context_id};
use alms_session::SessionConfig;
use alms_tools::message_sender::{ConversationEndReason, MessageSender, SendError};
use std::sync::Arc;
use tokio::sync::mpsc;

fn setup() -> (Arc<MessageBus>, mpsc::UnboundedReceiver<RunTrigger>) {
    let session_manager = Arc::new(alms_session::SessionManager::new(SessionConfig::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let bus = Arc::new(MessageBus::new(session_manager, tx));
    (bus, rx)
}

/// After `exhaust_depth`, returns the identity of the agent whose next
/// send will overflow: `(sender_name, sender_id, peer_name, peer_id)`.
///
/// `MAX_DM_DEPTH` alternating messages start with alice (even indices),
/// so the next sender depends on whether the total count is even or odd.
fn overflow_sender(a: AgentId, b: AgentId) -> (&'static str, AgentId, &'static str, AgentId) {
    if MAX_DM_DEPTH.is_multiple_of(2) {
        ("alice", a, "bob", b)
    } else {
        ("bob", b, "alice", a)
    }
}

/// Send MAX_DM_DEPTH alternating messages between alice and bob to exhaust
/// the depth counter. After calling this, the next alternating send will
/// trigger `SendError::DepthExceeded`.
async fn exhaust_depth(bus: &MessageBus, alice_id: AgentId, bob_id: AgentId) {
    for i in 0..MAX_DM_DEPTH {
        if i % 2 == 0 {
            bus.send("alice", alice_id, "bob", bob_id, "ping", None)
                .await
                .unwrap();
        } else {
            bus.send("bob", bob_id, "alice", alice_id, "pong", None)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn test_send_creates_shared_session() {
    let (bus, mut rx) = setup();
    let sender_id = AgentId::new();
    let recipient_id = AgentId::new();

    let receipt = bus
        .send("alice", sender_id, "bob", recipient_id, "Hello Bob!", None)
        .await
        .unwrap();

    // Both sides should resolve to the same session
    let expected_id = SessionId::deterministic_dm("alice", "bob");
    assert_eq!(receipt.session_id, expected_id);

    // Session should have one User message with from_agent metadata
    let history = bus.session_manager.get_history(receipt.session_id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, alms_session::Role::User);
    let meta = history[0].metadata.as_ref().unwrap();
    assert_eq!(meta["from_agent"], "alice");
    assert_eq!(meta["message_type"], "dm");

    // RunTrigger should have been emitted for the recipient
    let trigger = rx.try_recv().unwrap();
    assert_eq!(trigger.agent_id, recipient_id);
    assert_eq!(trigger.session_id, expected_id);
    assert_eq!(trigger.input, "Hello Bob!");
}

#[tokio::test]
async fn test_shared_session_accumulates_messages() {
    let (bus, _rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Alice sends to Bob
    let r1 = bus
        .send("alice", alice_id, "bob", bob_id, "Hello Bob!", None)
        .await
        .unwrap();

    // Bob sends to Alice (same shared session, reversed order)
    let r2 = bus
        .send("bob", bob_id, "alice", alice_id, "Hi Alice!", None)
        .await
        .unwrap();

    // Both should resolve to the same session
    assert_eq!(r1.session_id, r2.session_id);

    // Session should have two messages
    let history = bus.session_manager.get_history(r1.session_id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].metadata.as_ref().unwrap()["from_agent"], "alice");
    assert_eq!(history[1].metadata.as_ref().unwrap()["from_agent"], "bob");

    // Both messages stored as Role::User
    assert_eq!(history[0].role, alms_session::Role::User);
    assert_eq!(history[1].role, alms_session::Role::User);
}

#[tokio::test]
async fn test_self_message_rejected() {
    let (bus, _rx) = setup();
    let agent_id = AgentId::new();

    let err = bus
        .send("alice", agent_id, "alice", agent_id, "echo", None)
        .await
        .unwrap_err();

    assert!(matches!(err, SendError::SelfMessage));
}

#[tokio::test]
async fn test_depth_exceeded_rejected() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    // Next alternating message should be rejected (depth > MAX_DM_DEPTH)
    let (sender, sender_id, peer, peer_id) = overflow_sender(a, b);
    let err = bus
        .send(sender, sender_id, peer, peer_id, "one more", None)
        .await
        .unwrap_err();
    assert!(matches!(err, SendError::DepthExceeded));
}

#[tokio::test]
async fn test_reply_not_blocked() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    // A -> B succeeds
    bus.send("alice", a, "bob", b, "msg1", None).await.unwrap();

    // B -> A should succeed immediately (no cooldown blocking replies)
    bus.send("bob", b, "alice", a, "reply", None).await.unwrap();
}

#[tokio::test]
async fn test_different_pairs_independent() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();
    let c = AgentId::new();

    // A -> B succeeds
    bus.send("alice", a, "bob", b, "msg1", None).await.unwrap();

    // A -> C should also succeed (different pair)
    bus.send("alice", a, "charlie", c, "msg2", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_message_metadata_has_from_agent() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    let receipt = bus.send("alice", a, "bob", b, "Hello", None).await.unwrap();

    let history = bus.session_manager.get_history(receipt.session_id).unwrap();
    let meta = history[0].metadata.as_ref().unwrap();
    assert_eq!(meta["from_agent"], "alice");
    assert_eq!(meta["from_agent_id"], a.0.to_string());
}

#[tokio::test]
async fn test_depth_resets_after_activity_expires() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    // Next message should be rejected (depth exceeded)
    let (sender, sender_id, peer, peer_id) = overflow_sender(a, b);
    let err = bus
        .send(sender, sender_id, peer, peer_id, "overflow", None)
        .await
        .unwrap_err();
    assert!(matches!(err, SendError::DepthExceeded));

    // Simulate activity expiry: insert a last_activity timestamp in the past.
    let dm_ctx = dm_context_id("alice", "bob");
    bus.last_activity.insert(
        dm_ctx.clone(),
        std::time::Instant::now() - std::time::Duration::from_secs(DEPTH_EXPIRY_SECS + 1),
    );

    // After activity expires, depth should be reset -- sending should succeed.
    let receipt = bus
        .send("alice", a, "bob", b, "fresh start", None)
        .await
        .unwrap();
    assert_eq!(
        receipt.session_id,
        SessionId::deterministic_dm("alice", "bob")
    );
}

/// Integration test: full DM round-trip through MessageBus verifying
/// shared session, correct metadata, and no duplicate messages.
///
/// This test exercises the flow that was broken by C1 (session split)
/// and C2 (double-write). It verifies that:
/// 1. MessageBus writes the sender's message to the shared DM session.
/// 2. The RunTrigger contains the correct session_id and context_id.
/// 3. Looking up the session by the deterministic SessionId finds the
///    message that was written.
/// 4. No duplicate messages exist in the session.
#[tokio::test]
async fn test_full_dm_roundtrip_no_duplicate() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Step 1: Alice sends a message to Bob via the MessageBus.
    let receipt = bus
        .send(
            "alice",
            alice_id,
            "bob",
            bob_id,
            "Hello Bob, how are you?",
            None,
        )
        .await
        .unwrap();

    let expected_session_id = SessionId::deterministic_dm("alice", "bob");
    assert_eq!(receipt.session_id, expected_session_id);

    // Step 2: Verify the RunTrigger was emitted with correct fields.
    let trigger = rx.try_recv().unwrap();
    assert_eq!(trigger.agent_id, bob_id);
    assert_eq!(trigger.session_id, expected_session_id);
    assert_eq!(trigger.input, "Hello Bob, how are you?");
    assert_eq!(trigger.context_id, dm_context_id("alice", "bob"));

    // Step 3: Verify the shared session has exactly ONE message
    // (written by MessageBus) — no duplicate.
    let history = bus
        .session_manager
        .get_history(expected_session_id)
        .unwrap();
    assert_eq!(
        history.len(),
        1,
        "Session should have exactly 1 message (from MessageBus), not {}",
        history.len()
    );

    // Step 4: Verify metadata is correct (from_agent present).
    let msg = &history[0];
    assert_eq!(msg.role, alms_session::Role::User);
    let meta = msg.metadata.as_ref().expect("metadata should be present");
    assert_eq!(meta["from_agent"], "alice");
    assert_eq!(meta["from_agent_id"], alice_id.0.to_string());
    assert_eq!(meta["message_type"], "dm");

    // Step 5: The shared session should be findable by SessionId directly
    // (this is what run_on_session uses — if this fails, C1 is not fixed).
    let session = bus.session_manager.get(expected_session_id).unwrap();
    assert_eq!(session.id, expected_session_id);

    // Step 6: A second message from Bob should use the SAME session.
    let receipt2 = bus
        .send("bob", bob_id, "alice", alice_id, "I'm fine, thanks!", None)
        .await
        .unwrap();
    assert_eq!(receipt2.session_id, expected_session_id);

    let history2 = bus
        .session_manager
        .get_history(expected_session_id)
        .unwrap();
    assert_eq!(
        history2.len(),
        2,
        "Session should have exactly 2 messages after Bob's reply"
    );
    assert_eq!(
        history2[0].metadata.as_ref().unwrap()["from_agent"],
        "alice"
    );
    assert_eq!(history2[1].metadata.as_ref().unwrap()["from_agent"], "bob");
}

#[tokio::test]
async fn test_expired_entries_cleaned_up() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();
    let c = AgentId::new();

    // Create two DM pairs: alice-bob and alice-charlie
    bus.send("alice", a, "bob", b, "hi bob", None)
        .await
        .unwrap();
    bus.send("alice", a, "charlie", c, "hi charlie", None)
        .await
        .unwrap();

    let ab_ctx = dm_context_id("alice", "bob");
    let ac_ctx = dm_context_id("alice", "charlie");

    // Both pairs should have entries in the DashMaps
    assert!(bus.depths.contains_key(&ab_ctx));
    assert!(bus.depths.contains_key(&ac_ctx));
    assert!(bus.last_activity.contains_key(&ab_ctx));
    assert!(bus.last_activity.contains_key(&ac_ctx));

    // Expire alice-bob by backdating its last_activity
    bus.last_activity.insert(
        ab_ctx.clone(),
        std::time::Instant::now() - std::time::Duration::from_secs(DEPTH_EXPIRY_SECS + 1),
    );

    // Sending any message triggers opportunistic cleanup of expired pairs
    bus.send("alice", a, "charlie", c, "still here", None)
        .await
        .unwrap();

    // alice-bob should have been cleaned up (expired)
    assert!(
        !bus.depths.contains_key(&ab_ctx),
        "expired depths entry should be removed"
    );
    assert!(
        !bus.last_activity.contains_key(&ab_ctx),
        "expired last_activity entry should be removed"
    );

    // alice-charlie should still be present (active)
    assert!(bus.depths.contains_key(&ac_ctx));
    assert!(bus.last_activity.contains_key(&ac_ctx));
}

// -----------------------------------------------------------------------
// end_conversation tests (#386)
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_end_conversation_writes_marker_to_dm_session() {
    let (bus, mut _rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation so the DM session exists.
    bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!", None)
        .await
        .unwrap();

    // Drain the send trigger.
    let _ = _rx.try_recv();

    // End the conversation from alice's side.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // The DM session should now contain 2 messages: the original DM + the marker.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history.len(),
        2,
        "DM session should have 2 messages (dm + dm_ended marker)"
    );

    // Verify the marker message metadata.
    let marker = &history[1];
    let meta = marker
        .metadata
        .as_ref()
        .expect("marker should have metadata");
    assert_eq!(meta["message_type"], "dm_ended");
    assert_eq!(meta["ended_by"], "alice");
    assert_eq!(meta["reason"], "ignored");

    // Marker content should be empty.
    match &marker.content {
        alms_session::Content::Text(t) => {
            assert!(t.is_empty(), "marker content should be empty")
        }
        other => panic!("expected Text content, got {:?}", other),
    }
}

#[tokio::test]
async fn test_end_conversation_resets_depth_counter() {
    let (bus, _rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Build up some depth with alternating messages.
    bus.send("alice", alice_id, "bob", bob_id, "ping", None)
        .await
        .unwrap();
    bus.send("bob", bob_id, "alice", alice_id, "pong", None)
        .await
        .unwrap();
    bus.send("alice", alice_id, "bob", bob_id, "ping2", None)
        .await
        .unwrap();

    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        bus.depths.contains_key(&dm_ctx),
        "depth counter should exist after messages"
    );

    // End the conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Depth counter and last_activity should both be removed.
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth counter should be reset after end_conversation"
    );
    assert!(
        !bus.last_activity.contains_key(&dm_ctx),
        "last_activity should be removed after end_conversation"
    );

    // A new conversation should be possible immediately (fresh depth).
    bus.send("alice", alice_id, "bob", bob_id, "fresh start", None)
        .await
        .unwrap();
    let entry = bus.depths.get(&dm_ctx).unwrap();
    assert_eq!(entry.value().1, 1, "depth should restart at 1");
}

#[tokio::test]
async fn test_end_conversation_emits_run_trigger_with_conversation_ended() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation.
    bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!", None)
        .await
        .unwrap();

    // Drain the send trigger.
    let _ = rx.try_recv().unwrap();

    // End the conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // A RunTrigger should have been emitted for the peer (bob).
    let trigger = rx.try_recv().expect("should have received a RunTrigger");

    // Verify trigger fields.
    assert_eq!(trigger.agent_id, bob_id);
    assert_eq!(
        trigger.context_id, "notifications:bob",
        "notification should target the notifications session"
    );
    assert_eq!(
        trigger.session_id,
        SessionId::deterministic("notifications:bob"),
        "session ID should be deterministic from the notification context"
    );
    assert_eq!(
        trigger.input,
        "[DM conversation ended] Agent alice ended the conversation."
    );

    // Verify source variant.
    match &trigger.source {
        MessageSource::ConversationEnded {
            from_agent,
            from_name,
            reason,
            ..
        } => {
            assert_eq!(*from_agent, alice_id);
            assert_eq!(from_name, "alice");
            assert_eq!(*reason, ConversationEndReason::Ignored);
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

#[tokio::test]
async fn test_end_conversation_depth_exceeded_reason() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation.
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    // End with DepthExceeded reason.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::DepthExceeded,
    )
    .await
    .unwrap();

    // Verify marker has depth_exceeded reason.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    let marker = history.last().unwrap();
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["reason"], "depth_exceeded");

    // Verify trigger source has the correct reason.
    let trigger = rx.try_recv().unwrap();
    match &trigger.source {
        MessageSource::ConversationEnded { reason, .. } => {
            assert_eq!(*reason, ConversationEndReason::DepthExceeded);
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// When send_message is called with a source session, the notification
/// run should be routed to that source session instead of `notifications:`.
#[tokio::test]
async fn test_source_session_routes_notification_to_source() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Alice sends from her web-chat session.
    let alice_session = SessionId::new();
    // Create the session in the manager so get() works during end_conversation.
    bus.session_manager
        .get_or_create_shared(alice_session, "web-chat-alice");
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Hello Bob!",
        Some(alice_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob replies from the DM session (no source session -- triggered by DM).
    bus.send("bob", bob_id, "alice", alice_id, "Hi Alice!", None)
        .await
        .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob ends the conversation.
    bus.end_conversation(
        "bob",
        bob_id,
        "alice",
        alice_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // The notification should go to Alice's source session (web-chat).
    let trigger = rx.try_recv().expect("should have received notification");
    assert_eq!(trigger.agent_id, alice_id);
    assert_eq!(
        trigger.session_id, alice_session,
        "notification should go to Alice's source session, not notifications:"
    );
    assert_eq!(trigger.context_id, "web-chat-alice");

    match &trigger.source {
        MessageSource::ConversationEnded {
            source_session_id, ..
        } => {
            assert_eq!(
                *source_session_id,
                Some(alice_session),
                "source_session_id should be Alice's web-chat session"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// When a DM-triggered agent replies via `send_message`, the sender's
/// session ID is the DM session itself. This should NOT be recorded as
/// a source session, because it would defeat the `notifications:` fallback.
///
/// Scenario: Alice sends from web-chat. Bob is triggered by the DM, so
/// Bob's run executes on the DM session. Bob calls `send_message("alice",
/// "reply")` with `sender_session_id = Some(dm_session_id)`. The DM session
/// should be filtered out, so Bob has no source session. When Alice ends
/// the conversation, Bob's notification falls back to `notifications:bob`.
#[tokio::test]
async fn test_dm_session_not_recorded_as_source_session() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Alice sends from her web-chat session.
    let alice_session = SessionId::new();
    bus.session_manager
        .get_or_create_shared(alice_session, "web-chat-alice");
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Hello Bob!",
        Some(alice_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob replies from within the DM session (DM-triggered agent).
    // In production, SendMessageTool passes the current session_id,
    // which for a DM-triggered run IS the DM session.
    let dm_session_id = SessionId::deterministic_dm("bob", "alice");
    bus.send(
        "bob",
        bob_id,
        "alice",
        alice_id,
        "Hi Alice!",
        Some(dm_session_id),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Verify that the DM session was NOT stored as Bob's source session.
    let bob_key = (dm_context_id("bob", "alice"), "bob".to_string());
    assert!(
        !bus.source_sessions.contains_key(&bob_key),
        "DM session should not be recorded as Bob's source session"
    );

    // Alice's web-chat source session should still be recorded.
    let alice_key = (dm_context_id("alice", "bob"), "alice".to_string());
    assert!(
        bus.source_sessions.contains_key(&alice_key),
        "Alice's web-chat should still be recorded as her source session"
    );

    // Now Alice ends the conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Bob's notification should fall back to notifications:bob
    // (not the DM session).
    let trigger = rx.try_recv().expect("should have received notification");
    assert_eq!(trigger.agent_id, bob_id);
    assert_eq!(
        trigger.context_id, "notifications:bob",
        "notification should fall back to notifications:bob, not the DM session"
    );
    assert_eq!(
        trigger.session_id,
        SessionId::deterministic("notifications:bob"),
    );

    match &trigger.source {
        MessageSource::ConversationEnded {
            source_session_id, ..
        } => {
            assert!(
                source_session_id.is_none(),
                "source_session_id should be None (DM session was filtered out)"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// When no source session is provided, notification falls back to
/// the `notifications:` session (existing behavior).
#[tokio::test]
async fn test_no_source_session_falls_back_to_notifications() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Both agents send without a source session (e.g. both triggered by DMs).
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    bus.send("bob", bob_id, "alice", alice_id, "Hi!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    // Alice ends the conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // The notification should fall back to notifications:bob.
    let trigger = rx.try_recv().expect("should have received notification");
    assert_eq!(trigger.agent_id, bob_id);
    assert_eq!(trigger.context_id, "notifications:bob");
    assert_eq!(
        trigger.session_id,
        SessionId::deterministic("notifications:bob"),
    );

    match &trigger.source {
        MessageSource::ConversationEnded {
            source_session_id, ..
        } => {
            assert!(
                source_session_id.is_none(),
                "source_session_id should be None when no source session was provided"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// Source sessions are cleaned up when the conversation ends, so a
/// new conversation gets fresh source tracking.
#[tokio::test]
async fn test_source_sessions_cleaned_up_after_end_conversation() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    let alice_session = SessionId::new();
    bus.session_manager
        .get_or_create_shared(alice_session, "web-chat-alice");

    // First conversation: Alice sends from web-chat.
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "First conv",
        Some(alice_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv();

    // End conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain peer notification trigger
    let _ = rx.try_recv(); // drain sender self-notification trigger (#556)

    // Source session map should be empty now.
    assert!(
        bus.source_sessions.is_empty(),
        "source_sessions should be cleaned up after end_conversation"
    );

    // Second conversation: Alice sends WITHOUT a source session.
    bus.send("alice", alice_id, "bob", bob_id, "Second conv", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    // End conversation again.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // This time, notification should go to notifications:bob (no source).
    let trigger = rx.try_recv().expect("should have received notification");
    assert_eq!(trigger.context_id, "notifications:bob");
}

/// When both agents have source sessions (each was invoked from a
/// separate web-chat), the notification for each peer should route to
/// the correct source session.
///
/// Scenario: chamunchuk sends from web-chat-A, blumbum sends from
/// web-chat-B (a user is talking to blumbum separately). When the
/// conversation ends, chamunchuk's notification goes to web-chat-A and
/// blumbum's notification goes to web-chat-B.
#[tokio::test]
async fn test_both_agents_have_source_sessions() {
    let (bus, mut rx) = setup();
    let chamunchuk_id = AgentId::new();
    let blumbum_id = AgentId::new();

    // Create the two source sessions in the session manager.
    let webchat_a = SessionId::new();
    bus.session_manager
        .get_or_create_shared(webchat_a, "web-chat-A");
    let webchat_b = SessionId::new();
    bus.session_manager
        .get_or_create_shared(webchat_b, "web-chat-B");

    // Chamunchuk sends from web-chat-A.
    bus.send(
        "chamunchuk",
        chamunchuk_id,
        "blumbum",
        blumbum_id,
        "Hey blumbum!",
        Some(webchat_a),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Blumbum replies from web-chat-B (a user is talking to blumbum
    // separately, so blumbum's tool call passes web-chat-B as source).
    bus.send(
        "blumbum",
        blumbum_id,
        "chamunchuk",
        chamunchuk_id,
        "Hi chamunchuk!",
        Some(webchat_b),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Chamunchuk ends the conversation.
    bus.end_conversation(
        "chamunchuk",
        chamunchuk_id,
        "blumbum",
        blumbum_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Blumbum's notification (peer) should route to web-chat-B.
    let trigger = rx
        .try_recv()
        .expect("should have received peer notification");
    assert_eq!(trigger.agent_id, blumbum_id);
    assert_eq!(
        trigger.session_id, webchat_b,
        "blumbum's notification should go to web-chat-B"
    );
    assert_eq!(trigger.context_id, "web-chat-B");

    match &trigger.source {
        MessageSource::ConversationEnded {
            source_session_id, ..
        } => {
            assert_eq!(
                *source_session_id,
                Some(webchat_b),
                "source_session_id should be blumbum's web-chat-B"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // Chamunchuk's self-notification (sender with source session, #556)
    // should route to web-chat-A.
    let sender_trigger = rx
        .try_recv()
        .expect("should have received sender self-notification (#556)");
    assert_eq!(sender_trigger.agent_id, chamunchuk_id);
    assert_eq!(
        sender_trigger.session_id, webchat_a,
        "chamunchuk's self-notification should go to web-chat-A"
    );
    assert_eq!(sender_trigger.context_id, "web-chat-A");

    match &sender_trigger.source {
        MessageSource::ConversationEnded {
            from_name,
            source_session_id,
            ..
        } => {
            assert_eq!(from_name, "blumbum", "from_name should be the peer");
            assert_eq!(
                *source_session_id,
                Some(webchat_a),
                "source_session_id should be chamunchuk's web-chat-A"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // Now test the reverse: start a new conversation and have blumbum
    // end it, so chamunchuk's notification goes to web-chat-A.
    //
    // The source sessions were cleaned up by end_conversation, so we
    // need to establish them again.
    bus.send(
        "chamunchuk",
        chamunchuk_id,
        "blumbum",
        blumbum_id,
        "Round 2!",
        Some(webchat_a),
    )
    .await
    .unwrap();
    let _ = rx.try_recv();

    bus.send(
        "blumbum",
        blumbum_id,
        "chamunchuk",
        chamunchuk_id,
        "Round 2 reply!",
        Some(webchat_b),
    )
    .await
    .unwrap();
    let _ = rx.try_recv();

    // This time, blumbum ends the conversation.
    bus.end_conversation(
        "blumbum",
        blumbum_id,
        "chamunchuk",
        chamunchuk_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Chamunchuk's notification (peer) should route to web-chat-A.
    let trigger = rx
        .try_recv()
        .expect("should have received peer notification");
    assert_eq!(trigger.agent_id, chamunchuk_id);
    assert_eq!(
        trigger.session_id, webchat_a,
        "chamunchuk's notification should go to web-chat-A"
    );
    assert_eq!(trigger.context_id, "web-chat-A");

    match &trigger.source {
        MessageSource::ConversationEnded {
            source_session_id, ..
        } => {
            assert_eq!(
                *source_session_id,
                Some(webchat_a),
                "source_session_id should be chamunchuk's web-chat-A"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // Blumbum's self-notification (sender with source session, #556)
    // should route to web-chat-B.
    let sender_trigger = rx
        .try_recv()
        .expect("should have received sender self-notification (#556)");
    assert_eq!(sender_trigger.agent_id, blumbum_id);
    assert_eq!(
        sender_trigger.session_id, webchat_b,
        "blumbum's self-notification should go to web-chat-B"
    );
    assert_eq!(sender_trigger.context_id, "web-chat-B");

    match &sender_trigger.source {
        MessageSource::ConversationEnded {
            from_name,
            source_session_id,
            ..
        } => {
            assert_eq!(from_name, "chamunchuk", "from_name should be the peer");
            assert_eq!(
                *source_session_id,
                Some(webchat_b),
                "source_session_id should be blumbum's web-chat-B"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// Regression test for #556: when the initiating agent (the one with a
/// source session) ends the DM conversation, they should receive a
/// self-notification run in their source session so the user watching
/// that session sees the conversation outcome with transcript.
///
/// Before this fix, only the PEER received a notification; the initiator
/// only got a lightweight SSE marker without a full notification run.
#[tokio::test]
async fn test_initiator_gets_self_notification_when_ending_dm() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Alice sends from her web-chat session (she is the initiator).
    let alice_session = SessionId::new();
    bus.session_manager
        .get_or_create_shared(alice_session, "web-chat-alice");
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Hello Bob!",
        Some(alice_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob replies without a source session (pure DM recipient).
    bus.send("bob", bob_id, "alice", alice_id, "Hi Alice!", None)
        .await
        .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Alice (the initiator) ends the conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Trigger 1: Bob's notification (peer) goes to notifications:bob.
    let peer_trigger = rx
        .try_recv()
        .expect("should have received peer notification for bob");
    assert_eq!(peer_trigger.agent_id, bob_id);
    assert_eq!(peer_trigger.context_id, "notifications:bob");

    // Trigger 2 (#556): Alice's self-notification goes to her source session.
    let self_trigger = rx
        .try_recv()
        .expect("should have received sender self-notification for alice (#556)");
    assert_eq!(self_trigger.agent_id, alice_id);
    assert_eq!(
        self_trigger.session_id, alice_session,
        "initiator's self-notification should go to their source session"
    );
    assert_eq!(self_trigger.context_id, "web-chat-alice");

    // The self-notification should appear as if it came FROM the peer.
    match &self_trigger.source {
        MessageSource::ConversationEnded {
            from_name,
            source_session_id,
            ..
        } => {
            assert_eq!(from_name, "bob", "from_name should be the peer");
            assert_eq!(
                *source_session_id,
                Some(alice_session),
                "source_session_id should be alice's web-chat session"
            );
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // No more triggers.
    assert!(rx.try_recv().is_err(), "should not have any more triggers");
}

/// When a pure DM recipient (no source session) ends the conversation,
/// they should NOT receive a self-notification (no source session to
/// route it to). Only the peer gets notified.
#[tokio::test]
async fn test_recipient_no_self_notification_when_ending_dm() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Alice sends from her web-chat session.
    let alice_session = SessionId::new();
    bus.session_manager
        .get_or_create_shared(alice_session, "web-chat-alice");
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Hello Bob!",
        Some(alice_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob replies without a source session (pure DM recipient).
    bus.send("bob", bob_id, "alice", alice_id, "Hi Alice!", None)
        .await
        .unwrap();
    let _ = rx.try_recv(); // drain send trigger

    // Bob (the recipient, no source session) ends the conversation.
    bus.end_conversation(
        "bob",
        bob_id,
        "alice",
        alice_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Only one trigger: Alice's notification (peer) goes to her source session.
    let peer_trigger = rx
        .try_recv()
        .expect("should have received peer notification for alice");
    assert_eq!(peer_trigger.agent_id, alice_id);
    assert_eq!(
        peer_trigger.session_id, alice_session,
        "alice's notification should go to her source session"
    );

    // No self-notification for Bob (he has no source session).
    assert!(
        rx.try_recv().is_err(),
        "bob should NOT receive a self-notification (no source session)"
    );
}

/// S3: end_conversation should reject sender == peer (self-message guard).
#[tokio::test]
async fn test_end_conversation_self_message_rejected() {
    let (bus, _rx) = setup();
    let agent_id = AgentId::new();

    // Ending a conversation with yourself should fail.
    let err = bus
        .end_conversation(
            "alice",
            agent_id,
            "alice",
            agent_id,
            ConversationEndReason::Ignored,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, SendError::SelfMessage));
}

/// S2: end_conversation should no-op when the DM session doesn't exist
/// (no conversation was ever started).
#[tokio::test]
async fn test_end_conversation_nonexistent_session_is_noop() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Pre-insert a depth entry so depths.remove() succeeds (simulates
    // a state where depth tracking exists but the session was never
    // properly created -- shouldn't happen in practice, but tests the
    // defensive check).
    let dm_ctx = dm_context_id("alice", "bob");
    bus.depths.insert(dm_ctx.clone(), ("alice".to_string(), 1));

    // end_conversation should succeed (no error) but not emit a trigger.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // No trigger should have been emitted (session didn't exist).
    assert!(
        rx.try_recv().is_err(),
        "no RunTrigger should be emitted when DM session doesn't exist"
    );

    // Depth entry should still have been removed.
    assert!(!bus.depths.contains_key(&dm_ctx));
}

/// S4: Simultaneous end_conversation from both agents should produce
/// exactly one notification trigger (not two). This tests the C1 fix:
/// depths.remove() as the atomicity guard.
///
/// Uses multi-threaded runtime + `tokio::spawn` so the two calls can
/// truly race on separate OS threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_simultaneous_end_conversation_only_one_trigger() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation so the DM session and depth exist.
    bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!", None)
        .await
        .unwrap();
    // Drain the send trigger.
    let _ = rx.try_recv().unwrap();

    // Both agents call end_conversation simultaneously via tokio::spawn
    // so they execute on separate worker threads for true concurrency.
    let bus_a = bus.clone();
    let bus_b = bus.clone();

    let handle_a = tokio::spawn(async move {
        bus_a
            .end_conversation(
                "alice",
                alice_id,
                "bob",
                bob_id,
                ConversationEndReason::Ignored,
            )
            .await
    });
    let handle_b = tokio::spawn(async move {
        bus_b
            .end_conversation(
                "bob",
                bob_id,
                "alice",
                alice_id,
                ConversationEndReason::Ignored,
            )
            .await
    });

    // Both calls should succeed (no errors).
    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();

    // Count triggers: exactly one should have been emitted.
    // (The loser of the depths.remove() race returns early without
    // writing a marker or emitting a trigger.)
    let mut trigger_count = 0;
    while rx.try_recv().is_ok() {
        trigger_count += 1;
    }

    assert_eq!(
        trigger_count, 1,
        "simultaneous end_conversation should produce exactly 1 trigger, got {trigger_count}"
    );

    // Depth and last_activity should both be cleaned up.
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(!bus.depths.contains_key(&dm_ctx));
    assert!(!bus.last_activity.contains_key(&dm_ctx));
}

// -----------------------------------------------------------------------
// depth-exceeded auto-end tests (#391)
// -----------------------------------------------------------------------

/// When depth is exceeded, the DM session should contain a dm_ended marker
/// with reason "depth_exceeded".
#[tokio::test]
async fn test_depth_exceeded_writes_dm_ended_marker() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    // The next message should trigger DepthExceeded.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);

    let err = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await
        .unwrap_err();
    assert!(matches!(err, SendError::DepthExceeded));

    // The DM session should have a dm_ended marker as the last message.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    let marker = history.last().expect("should have messages");
    let meta = marker
        .metadata
        .as_ref()
        .expect("marker should have metadata");
    assert_eq!(meta["message_type"], "dm_ended");
    assert_eq!(meta["ended_by"], next_sender);
    assert_eq!(meta["reason"], "depth_exceeded");
}

/// When depth is exceeded, a ConversationEnded RunTrigger should be emitted
/// for the peer agent.
#[tokio::test]
async fn test_depth_exceeded_emits_notification_trigger() {
    let (bus, mut rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    // Drain all send triggers.
    while rx.try_recv().is_ok() {}

    // Trigger DepthExceeded.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);

    let err = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await
        .unwrap_err();
    assert!(matches!(err, SendError::DepthExceeded));

    // A ConversationEnded trigger should have been emitted for the peer.
    let trigger = rx
        .try_recv()
        .expect("should have received a notification trigger");
    assert_eq!(trigger.agent_id, peer_id);
    assert_eq!(
        trigger.context_id,
        format!("notifications:{peer_name}"),
        "notification should target the peer's notifications session"
    );

    match &trigger.source {
        MessageSource::ConversationEnded {
            from_agent,
            from_name,
            reason,
            ..
        } => {
            assert_eq!(*from_agent, next_id);
            assert_eq!(from_name, next_sender);
            assert_eq!(*reason, ConversationEndReason::DepthExceeded);
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }
}

/// When depth is exceeded, the depth counter should be reset so a new
/// conversation can start immediately.
#[tokio::test]
async fn test_depth_exceeded_resets_depth_counter() {
    let (bus, _rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        bus.depths.contains_key(&dm_ctx),
        "depth counter should exist before depth-exceeded"
    );

    // Trigger DepthExceeded.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);

    let _ = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await;

    // Depth counter should have been reset.
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth counter should be reset after depth-exceeded"
    );
    assert!(
        !bus.last_activity.contains_key(&dm_ctx),
        "last_activity should be removed after depth-exceeded"
    );

    // A fresh conversation should work immediately.
    bus.send(
        "alice",
        a,
        "bob",
        b,
        "fresh start after depth exceeded",
        None,
    )
    .await
    .unwrap();

    let entry = bus.depths.get(&dm_ctx).unwrap();
    assert_eq!(
        entry.value().1,
        1,
        "depth should restart at 1 after depth-exceeded reset"
    );
}

/// C2: After end_conversation, a concurrent send() should start a fresh
/// conversation (depth=1) rather than appending after the dm_ended marker.
#[tokio::test]
async fn test_send_after_end_starts_fresh_conversation() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start and end a conversation.
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();
    let _ = rx.try_recv();

    // The depth counter should be gone.
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(!bus.depths.contains_key(&dm_ctx));

    // A new send() should succeed and start at depth=1.
    bus.send("alice", alice_id, "bob", bob_id, "New conversation!", None)
        .await
        .unwrap();

    let entry = bus.depths.get(&dm_ctx).unwrap();
    assert_eq!(
        entry.value().1,
        1,
        "depth should be 1 for a fresh conversation after end"
    );
}

// -----------------------------------------------------------------------
// DM lifecycle integration tests (#393 -- Phase 9 of #384)
// -----------------------------------------------------------------------

/// F2.1: When the run_trigger_tx receiver is dropped (gateway shut down),
/// end_conversation should still return Ok — the trigger send failure is
/// logged but not propagated.
#[tokio::test]
async fn test_end_conversation_trigger_channel_dropped_returns_ok() {
    let session_manager = Arc::new(alms_session::SessionManager::new(SessionConfig::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let bus = Arc::new(MessageBus::new(session_manager, tx));

    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation so the session and depth exist.
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();

    // Drop the receiver to simulate the gateway shutting down.
    drop(rx);

    // end_conversation should still succeed — the RunTrigger send failure
    // is logged but does not cause an error return.
    let result = bus
        .end_conversation(
            "alice",
            alice_id,
            "bob",
            bob_id,
            ConversationEndReason::Ignored,
        )
        .await;

    assert!(
        result.is_ok(),
        "end_conversation should return Ok even when trigger channel is dropped, got: {:?}",
        result
    );

    // The dm_ended marker should still have been written to the session.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    let marker = history.last().unwrap();
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["message_type"], "dm_ended");
    assert_eq!(meta["ended_by"], "alice");

    // Depth counter should still have been reset.
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth counter should still be reset even when trigger channel is dropped"
    );
}

/// F2.1 (variant): When the trigger channel is dropped, send() should
/// still succeed for normal messages — the trigger failure is logged but
/// the delivery receipt is returned.
#[tokio::test]
async fn test_send_trigger_channel_dropped_still_returns_receipt() {
    let session_manager = Arc::new(alms_session::SessionManager::new(SessionConfig::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let bus = Arc::new(MessageBus::new(session_manager, tx));

    // Drop the receiver before sending any messages.
    drop(rx);

    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // send() should still succeed — the message is persisted to the
    // session even though the RunTrigger can't be delivered.
    let receipt = bus
        .send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await;

    assert!(
        receipt.is_ok(),
        "send() should return Ok even when trigger channel is dropped, got: {:?}",
        receipt
    );

    // The message should still be in the session.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].metadata.as_ref().unwrap()["from_agent"], "alice");
}

/// Full round-trip: ignore_message flow (end_conversation with Ignored
/// reason) produces all expected lifecycle effects in one test.
///
/// Verifies the complete sequence:
///   1. Conversation starts (alice -> bob)
///   2. Bob replies (bob -> alice)
///   3. Alice ignores (end_conversation with Ignored)
///   4. dm_ended marker written to DM session
///   5. Depth counter reset (new conversation possible immediately)
///   6. ConversationEnded RunTrigger emitted for bob
///   7. Trigger targets the notifications:bob session
#[tokio::test]
async fn test_ignore_full_roundtrip_lifecycle() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // 1. Alice starts the conversation.
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Hey Bob, can you help?",
        None,
    )
    .await
    .unwrap();
    let trigger1 = rx.try_recv().unwrap();
    assert_eq!(trigger1.agent_id, bob_id);

    // 2. Bob replies.
    bus.send(
        "bob",
        bob_id,
        "alice",
        alice_id,
        "Sure, what do you need?",
        None,
    )
    .await
    .unwrap();
    let trigger2 = rx.try_recv().unwrap();
    assert_eq!(trigger2.agent_id, alice_id);

    // 3. Alice ignores (end_conversation with Ignored reason).
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // 4. dm_ended marker should be in the DM session.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history.len(),
        3,
        "DM session should have 3 messages: alice DM + bob DM + dm_ended marker"
    );
    let marker = &history[2];
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["message_type"], "dm_ended");
    assert_eq!(meta["ended_by"], "alice");
    assert_eq!(meta["reason"], "ignored");

    // 5. Depth counter should be reset.
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth counter should be reset after ignore"
    );
    assert!(
        !bus.last_activity.contains_key(&dm_ctx),
        "last_activity should be removed after ignore"
    );

    // 6. ConversationEnded RunTrigger should have been emitted for bob.
    let notification_trigger = rx.try_recv().unwrap();
    assert_eq!(notification_trigger.agent_id, bob_id);

    match &notification_trigger.source {
        MessageSource::ConversationEnded {
            from_agent,
            from_name,
            reason,
            ..
        } => {
            assert_eq!(*from_agent, alice_id);
            assert_eq!(from_name, "alice");
            assert_eq!(*reason, ConversationEndReason::Ignored);
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // 7. Trigger should target the notifications:bob session.
    assert_eq!(notification_trigger.context_id, "notifications:bob");
    assert_eq!(
        notification_trigger.session_id,
        SessionId::deterministic("notifications:bob")
    );

    // Bonus: a new conversation should be possible immediately.
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "Fresh conversation!",
        None,
    )
    .await
    .unwrap();
    let entry = bus.depths.get(&dm_ctx).unwrap();
    assert_eq!(
        entry.value().1,
        1,
        "fresh conversation should start at depth 1"
    );
}

/// Full round-trip: depth-exceeded flow produces all expected lifecycle
/// effects in one test.
///
/// Verifies the complete sequence:
///   1. Exhaust the depth counter to MAX_DM_DEPTH
///   2. Next send returns DepthExceeded
///   3. dm_ended marker written with reason "depth_exceeded"
///   4. Depth counter reset (new conversation possible immediately)
///   5. ConversationEnded RunTrigger emitted for the peer
///   6. Trigger targets the notifications:{peer} session
#[tokio::test]
async fn test_depth_exceeded_full_roundtrip_lifecycle() {
    let (bus, mut rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    // 1. Exhaust the depth counter.
    exhaust_depth(&bus, a, b).await;

    // Drain all send triggers from the exchange.
    while rx.try_recv().is_ok() {}

    // 2. Next alternating send returns DepthExceeded.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);

    let err = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await
        .unwrap_err();
    assert!(matches!(err, SendError::DepthExceeded));

    // 3. dm_ended marker should be written with depth_exceeded reason.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    let marker = history.last().unwrap();
    let meta = marker.metadata.as_ref().unwrap();
    assert_eq!(meta["message_type"], "dm_ended");
    assert_eq!(meta["ended_by"], next_sender);
    assert_eq!(meta["reason"], "depth_exceeded");

    // 4. Depth counter should be reset.
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth counter should be reset after depth exceeded"
    );
    assert!(
        !bus.last_activity.contains_key(&dm_ctx),
        "last_activity should be removed after depth exceeded"
    );

    // 5. ConversationEnded RunTrigger should have been emitted for the peer.
    let notification = rx
        .try_recv()
        .expect("should have received a notification trigger");
    assert_eq!(notification.agent_id, peer_id);

    match &notification.source {
        MessageSource::ConversationEnded {
            from_agent,
            from_name,
            reason,
            ..
        } => {
            assert_eq!(*from_agent, next_id);
            assert_eq!(from_name, next_sender);
            assert_eq!(*reason, ConversationEndReason::DepthExceeded);
        }
        other => panic!("expected ConversationEnded, got {:?}", other),
    }

    // 6. Trigger should target the notifications:{peer} session.
    assert_eq!(
        notification.context_id,
        format!("notifications:{peer_name}")
    );
    assert_eq!(
        notification.session_id,
        SessionId::deterministic(&format!("notifications:{peer_name}"))
    );

    // Bonus: a new conversation should be possible immediately.
    bus.send("alice", a, "bob", b, "fresh start", None)
        .await
        .unwrap();
    let entry = bus.depths.get(&dm_ctx).unwrap();
    assert_eq!(entry.value().1, 1, "depth should restart at 1");
}

/// S1 resilience: When end_conversation fails during depth-exceeded
/// handling, send() should still return DepthExceeded (not an internal
/// error). This tests the TODO from PR #403.
///
/// We simulate failure by dropping the trigger channel AND having no
/// DM session (so the session write in end_conversation would succeed
/// but there's nothing to write to — the depths.remove() happens first,
/// then the session check returns early). The key assertion is that
/// send() returns DepthExceeded regardless.
#[tokio::test]
async fn test_depth_exceeded_returns_depth_exceeded_even_when_end_conversation_noop() {
    let session_manager = Arc::new(alms_session::SessionManager::new(SessionConfig::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let bus = Arc::new(MessageBus::new(session_manager, tx));

    // Drop the receiver so trigger sends will fail silently.
    drop(rx);

    let a = AgentId::new();
    let b = AgentId::new();

    // Exhaust the depth counter.
    exhaust_depth(&bus, a, b).await;

    // Next send should return DepthExceeded. The end_conversation call
    // inside send() will proceed (session exists from exhaust_depth
    // messages), but the trigger send will fail because rx is dropped.
    // Crucially, send() must still return DepthExceeded, not an internal error.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);

    let err = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await
        .unwrap_err();

    assert!(
        matches!(err, SendError::DepthExceeded),
        "send() should return DepthExceeded even when end_conversation trigger fails, got: {:?}",
        err
    );

    // The depth counter should still have been reset (end_conversation
    // runs depths.remove() before the trigger send that fails).
    let dm_ctx = dm_context_id("alice", "bob");
    assert!(
        !bus.depths.contains_key(&dm_ctx),
        "depth should still be reset even when trigger channel is dropped"
    );
}

/// S1 resilience (variant): simultaneous depth-exceeded from both sides.
///
/// If two agents both try to send at MAX_DM_DEPTH+1 simultaneously,
/// both should get DepthExceeded, and only one notification trigger should
/// fire (the depths.remove() atomicity guard prevents doubles).
///
/// Uses multi-threaded runtime + `tokio::spawn` so the two calls can
/// truly race on separate OS threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_simultaneous_depth_exceeded_both_get_error() {
    let (bus, mut rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    exhaust_depth(&bus, a, b).await;

    // Drain all send triggers from the initial exchange.
    while rx.try_recv().is_ok() {}

    // Both agents try to send one more message simultaneously via
    // tokio::spawn so they execute on separate worker threads for
    // true concurrency.
    let bus_a = bus.clone();
    let bus_b = bus.clone();

    let handle_a =
        tokio::spawn(async move { bus_a.send("alice", a, "bob", b, "overflow-a", None).await });
    let handle_b =
        tokio::spawn(async move { bus_b.send("bob", b, "alice", a, "overflow-b", None).await });

    let result_a = handle_a.await.unwrap();
    let result_b = handle_b.await.unwrap();

    // Count how many got DepthExceeded.
    let a_exceeded = matches!(result_a, Err(SendError::DepthExceeded));
    let b_exceeded = matches!(result_b, Err(SendError::DepthExceeded));

    // At least one must be DepthExceeded (the one that sees the depth
    // above the limit). It's possible one succeeds if it races in after
    // the other's end_conversation resets the depth, but then the depth
    // counter would restart at 1 — which is fine.
    assert!(
        a_exceeded || b_exceeded,
        "At least one sender must get DepthExceeded; a={:?}, b={:?}",
        result_a,
        result_b
    );

    // Count notification triggers (ConversationEnded).
    let mut notification_count = 0;
    while let Ok(trigger) = rx.try_recv() {
        if matches!(trigger.source, MessageSource::ConversationEnded { .. }) {
            notification_count += 1;
        }
    }

    // At most one ConversationEnded notification should have fired
    // (from the first end_conversation call; the second sees
    // depths.remove() returning None and skips). Zero is possible
    // if neither hit depth exceeded (race where one resets depth
    // before the other checks), but that would mean both succeeded.
    assert!(
        notification_count <= 1,
        "At most 1 ConversationEnded notification should fire, got {notification_count}"
    );
}

/// Verify end_conversation is idempotent: calling it twice for the same
/// DM pair should not produce duplicate markers or triggers.
#[tokio::test]
async fn test_end_conversation_idempotent() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start a conversation.
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    // End conversation first time.
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // End conversation second time (same direction).
    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Count triggers: should be exactly 1 (from the first call).
    let mut trigger_count = 0;
    while rx.try_recv().is_ok() {
        trigger_count += 1;
    }
    assert_eq!(
        trigger_count, 1,
        "idempotent end_conversation should produce exactly 1 trigger, got {trigger_count}"
    );

    // DM session should have exactly 2 messages: the DM + ONE marker.
    let session_id = SessionId::deterministic_dm("alice", "bob");
    let history = bus.session_manager.get_history(session_id).unwrap();
    assert_eq!(
        history.len(),
        2,
        "session should have 2 messages (dm + 1 marker), got {}",
        history.len()
    );
}

/// Notification session ID is deterministic and distinct from the DM
/// session ID — ensures conversation-end notifications don't pollute
/// the DM session.
#[tokio::test]
async fn test_notification_session_id_is_distinct_from_dm_session() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Start and end a conversation.
    bus.send("alice", alice_id, "bob", bob_id, "Hello!", None)
        .await
        .unwrap();
    let _ = rx.try_recv();

    bus.end_conversation(
        "alice",
        alice_id,
        "bob",
        bob_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    let notification_trigger = rx.try_recv().unwrap();

    // The notification session ID should NOT be the DM session ID.
    let dm_session_id = SessionId::deterministic_dm("alice", "bob");
    assert_ne!(
        notification_trigger.session_id, dm_session_id,
        "notification session should be distinct from the DM session"
    );

    // It should be the deterministic ID for "notifications:bob".
    let expected_notification_id = SessionId::deterministic("notifications:bob");
    assert_eq!(notification_trigger.session_id, expected_notification_id);
}

/// After end_conversation + depth reset, verify that the full depth
/// budget is available again (not just one message, but the full
/// MAX_DM_DEPTH messages).
#[tokio::test]
async fn test_depth_fully_replenished_after_end_conversation() {
    let (bus, mut rx) = setup();
    let a = AgentId::new();
    let b = AgentId::new();

    // Exhaust depth.
    exhaust_depth(&bus, a, b).await;
    while rx.try_recv().is_ok() {}

    // Trigger DepthExceeded and reset.
    let (next_sender, next_id, peer_name, peer_id) = overflow_sender(a, b);
    let _ = bus
        .send(next_sender, next_id, peer_name, peer_id, "overflow", None)
        .await;
    while rx.try_recv().is_ok() {}

    // Now exhaust depth again — should be able to send MAX_DM_DEPTH
    // more messages without hitting the limit.
    exhaust_depth(&bus, a, b).await;

    // The next message should trigger DepthExceeded again (confirming
    // the full budget was restored, not just 1 message).
    let err = bus
        .send(
            next_sender,
            next_id,
            peer_name,
            peer_id,
            "overflow again",
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, SendError::DepthExceeded),
        "should hit DepthExceeded again after full replenishment"
    );
}

/// Regression test for #656: when agent A is running on DM session X
/// (dm:A:B) and calls send_message to agent C, the source session for
/// the A-C DM pair IS recorded as the A-B DM session (#680).
///
/// Cross-DM source sessions are valid: Alice is in a conversation with
/// Bob and decides to message Charlie — when the Alice-Charlie DM ends,
/// the notification should route back to Alice's DM-with-Bob session
/// (where she is actively working), not fall through to notifications.
#[tokio::test]
async fn test_cross_dm_source_session_recorded() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();
    let charlie_id = AgentId::new();

    // Create a DM session between alice and bob.
    let dm_ab_session = SessionId::deterministic_dm("alice", "bob");
    let dm_ab_context = dm_context_id("alice", "bob");
    bus.session_manager
        .get_or_create_shared(dm_ab_session, &dm_ab_context);

    // Alice sends to bob from a web-chat session (establishes source).
    let alice_webchat = SessionId::new();
    bus.session_manager
        .get_or_create_shared(alice_webchat, "web-chat-alice");
    bus.send(
        "alice",
        alice_id,
        "bob",
        bob_id,
        "hello bob",
        Some(alice_webchat),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain bob's trigger

    // Now alice is running on the dm:alice:bob session and calls
    // send_message to charlie. The sender_session_id is the DM session.
    bus.send(
        "alice",
        alice_id,
        "charlie",
        charlie_id,
        "hello charlie",
        Some(dm_ab_session),
    )
    .await
    .unwrap();
    let _ = rx.try_recv(); // drain charlie's trigger

    // The source session for the alice-charlie DM SHOULD be recorded as
    // the dm:alice:bob session (cross-DM source is valid, #680).
    let ac_context = dm_context_id("alice", "charlie");
    let key = (ac_context.clone(), "alice".to_string());
    assert!(
        bus.source_sessions.contains_key(&key),
        "Cross-DM session should be recorded as source session"
    );
    assert_eq!(
        *bus.source_sessions.get(&key).unwrap().value(),
        dm_ab_session,
        "Source session should be Alice's DM-with-Bob session"
    );

    // End the alice-charlie conversation.
    bus.end_conversation(
        "alice",
        alice_id,
        "charlie",
        charlie_id,
        ConversationEndReason::Ignored,
    )
    .await
    .unwrap();

    // Charlie has no source session — notification goes to notifications:charlie.
    let trigger = rx
        .try_recv()
        .expect("should receive charlie's notification");
    assert_eq!(trigger.agent_id, charlie_id);
    assert!(
        trigger.context_id.starts_with("notifications:"),
        "charlie's notification should go to notifications: session, got: {}",
        trigger.context_id,
    );

    // Alice SHOULD get a self-notification routed to her DM-with-Bob session
    // (her source for the alice-charlie DM).
    let alice_trigger = rx
        .try_recv()
        .expect("alice should get a self-notification routed to dm:alice:bob");
    assert_eq!(alice_trigger.agent_id, alice_id);
    assert_eq!(
        alice_trigger.session_id, dm_ab_session,
        "alice's notification should route to dm:alice:bob session"
    );
    assert_eq!(
        alice_trigger.context_id, dm_ab_context,
        "alice's notification context should be dm:alice:bob"
    );
}

/// Internal session types (notification, subagent, episodic, job) must NOT
/// be recorded as source sessions, even after #680 relaxed the whitelist
/// to a denylist.
#[tokio::test]
async fn test_internal_session_types_not_recorded_as_source() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    let internal_contexts = [
        "notifications:alice",
        "subagent_research",
        "episodic:main",
        "job_550e8400-e29b-41d4-a716-446655440000",
    ];

    for ctx in &internal_contexts {
        let internal_session = SessionId::new();
        bus.session_manager
            .get_or_create_shared(internal_session, *ctx);
        bus.send(
            "alice",
            alice_id,
            "bob",
            bob_id,
            "hello from internal",
            Some(internal_session),
        )
        .await
        .unwrap();
        let _ = rx.try_recv(); // drain trigger

        let key = (dm_context_id("alice", "bob"), "alice".to_string());
        assert!(
            !bus.source_sessions.contains_key(&key),
            "Internal session type '{ctx}' should not be recorded as source session"
        );

        // Clean up for next iteration: remove depth/activity entries.
        bus.depths.remove(&dm_context_id("alice", "bob"));
        bus.last_activity.remove(&dm_context_id("alice", "bob"));
    }
}

/// **MANDATORY safety test (issue #685)**:
///
/// Persisted reasoning messages (`message_type: "reasoning"`) in a DM
/// session must NOT leak to the peer agent via `MessageBus::send()`.
///
/// The MessageBus writes its OWN `SessionMessage` with `message_type: "dm"`
/// and does NOT read back or forward existing session content.  This test
/// verifies that invariant: after manually writing a reasoning message to
/// the shared DM session, a subsequent `send()` from the peer does NOT
/// include the reasoning text -- only the new DM message appears.
#[tokio::test]
async fn test_reasoning_messages_not_forwarded_by_message_bus() {
    let (bus, mut rx) = setup();
    let alice_id = AgentId::new();
    let bob_id = AgentId::new();

    // Step 1: Alice sends a DM to Bob (creates the shared session).
    let receipt = bus
        .send("alice", alice_id, "bob", bob_id, "Hello Bob!", None)
        .await
        .unwrap();
    let dm_session_id = receipt.session_id;
    // Drain the RunTrigger.
    let _ = rx.try_recv().unwrap();

    // Step 2: Manually persist a reasoning message to the DM session,
    // simulating what the runtime does when it lifts the !is_dm guard.
    let reasoning_msg = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::Text(
            "Let me think about how to respond to Alice...".to_string(),
        ),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "message_type": "reasoning",
            "from_agent": "bob",
            "from_agent_id": bob_id.0.to_string(),
            "run_id": "00000000-0000-0000-0000-000000000099",
        })),
    };
    bus.session_manager
        .append_message(dm_session_id, reasoning_msg)
        .unwrap();

    // Also persist a reasoning tool call entry.
    let reasoning_tool = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::User,
        content: alms_session::Content::ToolCall {
            name: "read_session".to_string(),
            params: serde_json::json!({"session_id": "test"}),
        },
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(serde_json::json!({
            "message_type": "reasoning",
            "from_agent": "bob",
            "from_agent_id": bob_id.0.to_string(),
            "run_id": "00000000-0000-0000-0000-000000000099",
            "tool_call_id": "tc_test",
            "tool_invocation_id": "inv_test",
        })),
    };
    bus.session_manager
        .append_message(dm_session_id, reasoning_tool)
        .unwrap();

    // Step 3: Bob sends a reply via MessageBus.
    let receipt2 = bus
        .send("bob", bob_id, "alice", alice_id, "Hi Alice!", None)
        .await
        .unwrap();
    assert_eq!(receipt2.session_id, dm_session_id);

    // Step 4: Verify the RunTrigger only contains Bob's reply text,
    // NOT any reasoning content from the session.
    let trigger = rx.try_recv().unwrap();
    assert_eq!(trigger.input, "Hi Alice!");
    assert_eq!(trigger.agent_id, alice_id);

    // Step 5: Verify the session history contains the reasoning
    // messages (they are persisted) but they have the "reasoning"
    // message_type marker that dm_filter will catch.
    let history = bus.session_manager.get_history(dm_session_id).unwrap();

    // Should have: alice's DM, bob's reasoning text, bob's reasoning
    // tool call, bob's DM reply = 4 messages.
    assert_eq!(history.len(), 4, "Expected 4 messages in DM session");

    // Verify reasoning messages are present with correct metadata.
    let reasoning_msgs: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("reasoning")
        })
        .collect();
    assert_eq!(
        reasoning_msgs.len(),
        2,
        "Expected 2 reasoning messages in session"
    );

    // Verify that dm_filter correctly identifies reasoning messages
    // as synthetic markers (the load-bearing invariant).
    for msg in &reasoning_msgs {
        assert!(
            alms_tools::dm_filter::is_synthetic_marker(msg),
            "Reasoning message must be filtered by dm_filter::is_synthetic_marker"
        );
    }

    // Verify that DM messages are NOT filtered.
    let dm_msgs: Vec<_> = history
        .iter()
        .filter(|m| {
            m.metadata
                .as_ref()
                .and_then(|meta| meta.get("message_type"))
                .and_then(|v| v.as_str())
                == Some("dm")
        })
        .collect();
    assert_eq!(dm_msgs.len(), 2, "Expected 2 DM messages in session");
    for msg in &dm_msgs {
        assert!(
            !alms_tools::dm_filter::is_synthetic_marker(msg),
            "Real DM messages must NOT be filtered by dm_filter"
        );
    }
}
