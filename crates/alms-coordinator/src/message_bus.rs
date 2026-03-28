//! Agent-to-agent message bus (Layer 2 -- Phase 1: DM only).
//!
//! The `MessageBus` routes messages between agents via shared DM sessions.
//! Each DM conversation uses a **single shared session** -- both agents
//! read from and write to the same session. All messages are stored as
//! `Role::User` with `{from_agent, from_agent_id}` metadata. The
//! `ContextBuilder` performs perspective mapping at context-building time:
//! messages where `from_agent == self` become `"assistant"`, others stay
//! `"user"`.
//!
//! ## Loop prevention
//!
//! The MessageBus tracks a **depth counter** per DM pair that counts
//! consecutive bounces (A->B->A->B...). Delivery is refused when depth
//! exceeds `MAX_DM_DEPTH`. The depth counter resets automatically when
//! no messages have been exchanged for `DEPTH_EXPIRY_SECS` seconds,
//! allowing fresh conversation bursts after a quiet period.

use alms_core::{AgentId, SessionId, dm_context_id};
use alms_runtime::message_sender::{
    ConversationEndReason, DeliveryReceipt, MessageSender, SendError,
};
use alms_session::SessionManager;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, instrument, warn};

/// Maximum message forwarding depth. Prevents infinite A -> B -> A loops.
const MAX_DM_DEPTH: u32 = 20;

/// Seconds of inactivity after which the depth counter resets for a DM pair.
///
/// Raised from 60s to 1800s (30 minutes) because complex agent runs can
/// easily exceed one minute. See discussion on #362 / decision D5 in #384.
const DEPTH_EXPIRY_SECS: u64 = 1800;

// ---------------------------------------------------------------------------
// RunTrigger -- sent to the gateway to create runs
// ---------------------------------------------------------------------------

/// A request to create a run on a target agent's session.
///
/// The gateway's `run_trigger_loop` receives these and creates runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTrigger {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub input: String,
    pub source: MessageSource,
    /// Context ID for the target session (needed by execute_run).
    pub context_id: String,
}

/// Who originated the message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageSource {
    /// Peer-to-peer DM from another agent.
    Agent {
        from_agent: AgentId,
        from_name: String,
    },
    /// Subagent completion notification (bridged from the existing channel).
    SubagentCompletion,
    /// A DM conversation was ended (ignore_message or depth exceeded).
    ///
    /// The peer receives a one-shot notification run so it can act on the
    /// conversation outcome. See #384 for the full lifecycle design.
    ConversationEnded {
        from_agent: AgentId,
        from_name: String,
        reason: ConversationEndReason,
    },
}

// ---------------------------------------------------------------------------
// MessageBus
// ---------------------------------------------------------------------------

/// Agent-to-agent message bus.
///
/// Handles delivery of messages between agents, creating shared DM sessions
/// as needed and triggering runs on the receiving agent.
#[derive(Debug)]
pub struct MessageBus {
    session_manager: Arc<SessionManager>,
    /// Channel to trigger runs on the gateway.
    run_trigger_tx: mpsc::UnboundedSender<RunTrigger>,
    /// Per-DM-pair depth tracker: "dm:a:b" -> (last_sender_name, depth).
    /// Depth increments each time the sender changes within the same pair.
    depths: DashMap<String, (String, u32)>,
    /// Per-DM-pair last activity timestamp for depth expiry.
    last_activity: DashMap<String, Instant>,
}

impl MessageBus {
    /// Create a new MessageBus.
    pub fn new(
        session_manager: Arc<SessionManager>,
        run_trigger_tx: mpsc::UnboundedSender<RunTrigger>,
    ) -> Self {
        Self {
            session_manager,
            run_trigger_tx,
            depths: DashMap::new(),
            last_activity: DashMap::new(),
        }
    }
}

#[async_trait]
impl MessageSender for MessageBus {
    /// Send a message from one agent to another via a shared DM session.
    ///
    /// 1. Validates the send (self-message, depth).
    /// 2. Gets-or-creates the shared DM session (deterministic SessionId).
    /// 3. Appends the message as `Role::User` with `{from_agent}` metadata.
    /// 4. Emits a `RunTrigger` for the recipient.
    #[instrument(
        level = "info",
        skip(self, message),
        fields(
            sender = %sender_name,
            recipient = %recipient_name,
        )
    )]
    async fn send(
        &self,
        sender_name: &str,
        sender_agent_id: AgentId,
        recipient_name: &str,
        recipient_agent_id: AgentId,
        message: &str,
    ) -> Result<DeliveryReceipt, SendError> {
        // --- Validation ---

        if sender_agent_id == recipient_agent_id {
            return Err(SendError::SelfMessage);
        }

        let dm_context = dm_context_id(sender_name, recipient_name);

        // Time-based depth expiry: if no messages have been exchanged in
        // this DM pair for DEPTH_EXPIRY_SECS, reset the depth counter so
        // a new conversation burst can start fresh.
        if let Some(last) = self.last_activity.get(&dm_context)
            && last.elapsed().as_secs() >= DEPTH_EXPIRY_SECS
        {
            self.depths.remove(&dm_context);
        }

        // Opportunistic cleanup: remove expired entries from both DashMaps
        // to prevent unbounded growth from accumulated DM pairs. We only
        // retain entries that have been active within the expiry window.
        self.last_activity.retain(|key, last| {
            if last.elapsed().as_secs() >= DEPTH_EXPIRY_SECS {
                self.depths.remove(key);
                false
            } else {
                true
            }
        });

        // Internal depth tracking: increments each time a different sender
        // sends to the same DM pair. If Alice sends, then Bob replies, then
        // Alice replies again, depth goes 1 -> 2 -> 3.
        let current_depth = {
            let mut entry = self
                .depths
                .entry(dm_context.clone())
                .or_insert_with(|| (String::new(), 0));
            let (last_sender, depth) = entry.value_mut();
            if last_sender != sender_name {
                *depth += 1;
                *last_sender = sender_name.to_string();
            } else if depth == &0 {
                // First message in this DM pair
                *depth = 1;
                *last_sender = sender_name.to_string();
            }
            *depth
        };

        if current_depth > MAX_DM_DEPTH {
            // End the conversation cleanly: write a dm_ended marker to the
            // session, reset the depth counter, and notify the peer.
            // This ensures depth-exceeded conversations get the same lifecycle
            // events as ignore_message-ended conversations (#391).
            if let Err(e) = self
                .end_conversation(
                    sender_name,
                    sender_agent_id,
                    recipient_name,
                    recipient_agent_id,
                    ConversationEndReason::DepthExceeded,
                )
                .await
            {
                warn!(
                    error = %e,
                    "Failed to end conversation on depth exceeded"
                );
            }

            return Err(SendError::DepthExceeded);
        }

        // --- Shared DM session ---

        let session_id = SessionId::deterministic_dm(sender_name, recipient_name);
        let session = self
            .session_manager
            .get_or_create_shared(session_id, &dm_context);

        // --- Write message to shared session ---

        let msg = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(message.to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "from_agent": sender_name,
                "from_agent_id": sender_agent_id.0.to_string(),
                "message_type": "dm",
            })),
        };
        self.session_manager
            .append_message(session.id, msg)
            .map_err(|e| SendError::Internal(e.to_string()))?;

        // --- Update last activity for depth expiry ---
        self.last_activity
            .insert(dm_context.clone(), Instant::now());

        // --- Trigger run on recipient ---
        let trigger = RunTrigger {
            agent_id: recipient_agent_id,
            session_id,
            input: message.to_string(),
            source: MessageSource::Agent {
                from_agent: sender_agent_id,
                from_name: sender_name.to_string(),
            },
            context_id: dm_context,
        };

        if let Err(e) = self.run_trigger_tx.send(trigger) {
            warn!(
                error = %e,
                "Failed to send RunTrigger for peer message (receiver dropped)"
            );
        }

        info!(
            session_id = %session_id.0,
            depth = current_depth,
            "Peer message delivered to shared DM session"
        );

        Ok(DeliveryReceipt { session_id })
    }

    /// End a DM conversation: write a metadata marker, reset depth, and
    /// emit a `RunTrigger` with `ConversationEnded` source for the peer.
    ///
    /// Concurrency safety: `depths.remove()` is used as the atomicity point.
    /// If two agents call `end_conversation` simultaneously for the same pair,
    /// only the one whose `depths.remove()` returns `Some` proceeds with the
    /// marker write and trigger emission. The other observes `None` and
    /// returns early, preventing double notifications. The depth removal also
    /// happens BEFORE the marker write so that a concurrent `send()` cannot
    /// slip a message in after the marker (it would start a fresh depth=1
    /// conversation instead).
    #[instrument(
        level = "info",
        skip(self),
        fields(
            sender = %sender_name,
            peer = %peer_name,
            reason = %reason,
        )
    )]
    async fn end_conversation(
        &self,
        sender_name: &str,
        sender_agent_id: AgentId,
        peer_name: &str,
        peer_agent_id: AgentId,
        reason: ConversationEndReason,
    ) -> Result<(), SendError> {
        // --- S3: Self-message guard (mirrors send()) ---
        if sender_agent_id == peer_agent_id {
            return Err(SendError::SelfMessage);
        }

        let dm_context = dm_context_id(sender_name, peer_name);
        let session_id = SessionId::deterministic_dm(sender_name, peer_name);

        // --- C1+C2: Reset depth FIRST, use remove() as atomicity guard ---
        //
        // depths.remove() returns Some if the entry existed (we are the first
        // caller to end this conversation) or None if it was already removed
        // (a concurrent end_conversation already handled it). This prevents
        // double marker writes and double notification triggers.
        //
        // By removing depth BEFORE writing the marker, a concurrent send()
        // that races in will start a fresh depth=1 conversation rather than
        // appending after the dm_ended marker.
        let was_active = self.depths.remove(&dm_context).is_some();
        self.last_activity.remove(&dm_context);

        if !was_active {
            info!(
                session_id = %session_id.0,
                "end_conversation skipped -- already ended by peer"
            );
            return Ok(());
        }

        // --- S2: Validate that the DM session exists ---
        //
        // A conversation must have started (via send()) before it can end.
        // If the session doesn't exist, the caller has a bug -- return early
        // rather than silently creating an empty session.
        if self.session_manager.get(session_id).is_err() {
            warn!(
                session_id = %session_id.0,
                "end_conversation called but DM session does not exist -- no-op"
            );
            return Ok(());
        }

        // --- Write dm_ended metadata marker to the shared DM session ---
        let marker = alms_session::Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: alms_session::Role::User,
            content: alms_session::Content::Text(String::new()),
            timestamp: alms_core::Timestamp::now(),
            metadata: Some(serde_json::json!({
                "message_type": "dm_ended",
                "ended_by": sender_name,
                "reason": reason.to_string(),
            })),
        };
        self.session_manager
            .append_message(session_id, marker)
            .map_err(|e| SendError::Internal(e.to_string()))?;

        // --- Emit RunTrigger for the peer agent ---
        let notification_context = format!("notifications:{peer_name}");
        let notification_session_id = SessionId::deterministic(&notification_context);

        let input = format!("[DM conversation ended] Agent {sender_name} ended the conversation.");

        let trigger = RunTrigger {
            agent_id: peer_agent_id,
            session_id: notification_session_id,
            input,
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: sender_name.to_string(),
                reason,
            },
            context_id: notification_context,
        };

        if let Err(e) = self.run_trigger_tx.send(trigger) {
            warn!(
                error = %e,
                "Failed to send RunTrigger for conversation end notification (receiver dropped)"
            );
        }

        info!(
            session_id = %session_id.0,
            "DM conversation ended, depth counter reset"
        );

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_session::SessionConfig;

    fn setup() -> (Arc<MessageBus>, mpsc::UnboundedReceiver<RunTrigger>) {
        let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
        let (tx, rx) = mpsc::unbounded_channel();
        let bus = Arc::new(MessageBus::new(session_manager, tx));
        (bus, rx)
    }

    #[tokio::test]
    async fn test_send_creates_shared_session() {
        let (bus, mut rx) = setup();
        let sender_id = AgentId::new();
        let recipient_id = AgentId::new();

        let receipt = bus
            .send("alice", sender_id, "bob", recipient_id, "Hello Bob!")
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
            .send("alice", alice_id, "bob", bob_id, "Hello Bob!")
            .await
            .unwrap();

        // Bob sends to Alice (same shared session, reversed order)
        let r2 = bus
            .send("bob", bob_id, "alice", alice_id, "Hi Alice!")
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
            .send("alice", agent_id, "alice", agent_id, "echo")
            .await
            .unwrap_err();

        assert!(matches!(err, SendError::SelfMessage));
    }

    #[tokio::test]
    async fn test_depth_exceeded_rejected() {
        let (bus, _rx) = setup();
        let a = AgentId::new();
        let b = AgentId::new();

        // Send MAX_DM_DEPTH alternating messages (depth reaches MAX_DM_DEPTH).
        // Each sender change increments depth: alice(1), bob(2), alice(3), ...
        for i in 0..MAX_DM_DEPTH {
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        // Next alternating message should be rejected (depth > MAX_DM_DEPTH)
        let err = bus
            .send(
                if MAX_DM_DEPTH.is_multiple_of(2) {
                    "alice"
                } else {
                    "bob"
                },
                if MAX_DM_DEPTH.is_multiple_of(2) { a } else { b },
                if MAX_DM_DEPTH.is_multiple_of(2) {
                    "bob"
                } else {
                    "alice"
                },
                if MAX_DM_DEPTH.is_multiple_of(2) { b } else { a },
                "one more",
            )
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
        bus.send("alice", a, "bob", b, "msg1").await.unwrap();

        // B -> A should succeed immediately (no cooldown blocking replies)
        bus.send("bob", b, "alice", a, "reply").await.unwrap();
    }

    #[tokio::test]
    async fn test_different_pairs_independent() {
        let (bus, _rx) = setup();
        let a = AgentId::new();
        let b = AgentId::new();
        let c = AgentId::new();

        // A -> B succeeds
        bus.send("alice", a, "bob", b, "msg1").await.unwrap();

        // A -> C should also succeed (different pair)
        bus.send("alice", a, "charlie", c, "msg2").await.unwrap();
    }

    #[tokio::test]
    async fn test_message_metadata_has_from_agent() {
        let (bus, _rx) = setup();
        let a = AgentId::new();
        let b = AgentId::new();

        let receipt = bus.send("alice", a, "bob", b, "Hello").await.unwrap();

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

        // Exhaust depth: send MAX_DM_DEPTH alternating messages
        for i in 0..MAX_DM_DEPTH {
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        // Next message should be rejected (depth exceeded)
        let err = bus
            .send(
                if MAX_DM_DEPTH.is_multiple_of(2) {
                    "alice"
                } else {
                    "bob"
                },
                if MAX_DM_DEPTH.is_multiple_of(2) { a } else { b },
                if MAX_DM_DEPTH.is_multiple_of(2) {
                    "bob"
                } else {
                    "alice"
                },
                if MAX_DM_DEPTH.is_multiple_of(2) { b } else { a },
                "overflow",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SendError::DepthExceeded));

        // Simulate activity expiry: insert a last_activity timestamp in the past.
        let dm_ctx = dm_context_id("alice", "bob");
        bus.last_activity.insert(
            dm_ctx.clone(),
            Instant::now() - std::time::Duration::from_secs(DEPTH_EXPIRY_SECS + 1),
        );

        // After activity expires, depth should be reset -- sending should succeed.
        let receipt = bus.send("alice", a, "bob", b, "fresh start").await.unwrap();
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
            .send("alice", alice_id, "bob", bob_id, "Hello Bob, how are you?")
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
            .send("bob", bob_id, "alice", alice_id, "I'm fine, thanks!")
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
        bus.send("alice", a, "bob", b, "hi bob").await.unwrap();
        bus.send("alice", a, "charlie", c, "hi charlie")
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
            Instant::now() - std::time::Duration::from_secs(DEPTH_EXPIRY_SECS + 1),
        );

        // Sending any message triggers opportunistic cleanup of expired pairs
        bus.send("alice", a, "charlie", c, "still here")
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
        bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!")
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
        bus.send("alice", alice_id, "bob", bob_id, "ping")
            .await
            .unwrap();
        bus.send("bob", bob_id, "alice", alice_id, "pong")
            .await
            .unwrap();
        bus.send("alice", alice_id, "bob", bob_id, "ping2")
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
        bus.send("alice", alice_id, "bob", bob_id, "fresh start")
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
        bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!")
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
        bus.send("alice", alice_id, "bob", bob_id, "Hello!")
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
    #[tokio::test]
    async fn test_simultaneous_end_conversation_only_one_trigger() {
        let (bus, mut rx) = setup();
        let alice_id = AgentId::new();
        let bob_id = AgentId::new();

        // Start a conversation so the DM session and depth exist.
        bus.send("alice", alice_id, "bob", bob_id, "Hello Bob!")
            .await
            .unwrap();
        // Drain the send trigger.
        let _ = rx.try_recv().unwrap();

        // Both agents call end_conversation simultaneously.
        let bus_a = bus.clone();
        let bus_b = bus.clone();

        let (result_a, result_b) = tokio::join!(
            bus_a.end_conversation(
                "alice",
                alice_id,
                "bob",
                bob_id,
                ConversationEndReason::Ignored,
            ),
            bus_b.end_conversation(
                "bob",
                bob_id,
                "alice",
                alice_id,
                ConversationEndReason::Ignored,
            ),
        );

        // Both calls should succeed (no errors).
        result_a.unwrap();
        result_b.unwrap();

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

        // Exhaust depth with alternating messages.
        for i in 0..MAX_DM_DEPTH {
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        // The next message should trigger DepthExceeded.
        let next_sender = if MAX_DM_DEPTH.is_multiple_of(2) {
            "alice"
        } else {
            "bob"
        };
        let (next_id, peer_name, _peer_id) = if MAX_DM_DEPTH.is_multiple_of(2) {
            (a, "bob", b)
        } else {
            (b, "alice", a)
        };

        let err = bus
            .send(next_sender, next_id, peer_name, _peer_id, "overflow")
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

        // Exhaust depth.
        for i in 0..MAX_DM_DEPTH {
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        // Drain all send triggers.
        while rx.try_recv().is_ok() {}

        // Trigger DepthExceeded.
        let next_sender = if MAX_DM_DEPTH.is_multiple_of(2) {
            "alice"
        } else {
            "bob"
        };
        let (next_id, peer_name, peer_id) = if MAX_DM_DEPTH.is_multiple_of(2) {
            (a, "bob", b)
        } else {
            (b, "alice", a)
        };

        let err = bus
            .send(next_sender, next_id, peer_name, peer_id, "overflow")
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

        // Exhaust depth.
        for i in 0..MAX_DM_DEPTH {
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        let dm_ctx = dm_context_id("alice", "bob");
        assert!(
            bus.depths.contains_key(&dm_ctx),
            "depth counter should exist before depth-exceeded"
        );

        // Trigger DepthExceeded.
        let next_sender = if MAX_DM_DEPTH.is_multiple_of(2) {
            "alice"
        } else {
            "bob"
        };
        let (next_id, peer_name, peer_id) = if MAX_DM_DEPTH.is_multiple_of(2) {
            (a, "bob", b)
        } else {
            (b, "alice", a)
        };

        let _ = bus
            .send(next_sender, next_id, peer_name, peer_id, "overflow")
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
        bus.send("alice", a, "bob", b, "fresh start after depth exceeded")
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
        bus.send("alice", alice_id, "bob", bob_id, "Hello!")
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
        bus.send("alice", alice_id, "bob", bob_id, "New conversation!")
            .await
            .unwrap();

        let entry = bus.depths.get(&dm_ctx).unwrap();
        assert_eq!(
            entry.value().1,
            1,
            "depth should be 1 for a fresh conversation after end"
        );
    }
}
