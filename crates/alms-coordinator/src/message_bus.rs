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
use alms_runtime::message_sender::{DeliveryReceipt, MessageSender, SendError};
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
const DEPTH_EXPIRY_SECS: u64 = 60;

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
}
