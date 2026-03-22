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
//! The MessageBus internally tracks two mechanisms:
//! 1. **Depth tracking**: per DM pair, counts consecutive bounces (A->B->A->B...).
//!    Delivery is refused when depth exceeds `MAX_DM_DEPTH`.
//! 2. **Per-DM-pair cooldown**: after delivering a message, the same pair cannot
//!    send another for `DM_COOLDOWN_SECS` seconds.

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
const MAX_DM_DEPTH: u32 = 5;

/// Minimum seconds between messages in the same DM pair.
const DM_COOLDOWN_SECS: u64 = 5;

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
    /// Per-DM-pair cooldown tracker: "dm:a:b" -> last send instant.
    cooldowns: DashMap<String, Instant>,
    /// Per-DM-pair depth tracker: "dm:a:b" -> (last_sender_name, depth).
    /// Depth increments each time the sender changes within the same pair.
    depths: DashMap<String, (String, u32)>,
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
            cooldowns: DashMap::new(),
            depths: DashMap::new(),
        }
    }
}

#[async_trait]
impl MessageSender for MessageBus {
    /// Send a message from one agent to another via a shared DM session.
    ///
    /// 1. Validates the send (self-message, cooldown, depth).
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

        // Per-DM-pair cooldown check
        if let Some(last_send) = self.cooldowns.get(&dm_context) {
            let elapsed = last_send.elapsed().as_secs();
            if elapsed < DM_COOLDOWN_SECS {
                return Err(SendError::CooldownActive);
            }
        }

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

        // --- Update cooldown ---
        self.cooldowns.insert(dm_context.clone(), Instant::now());

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

        // Manually clear cooldown for testing
        bus.cooldowns.clear();

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
            bus.cooldowns.clear();
            if i % 2 == 0 {
                bus.send("alice", a, "bob", b, "ping").await.unwrap();
            } else {
                bus.send("bob", b, "alice", a, "pong").await.unwrap();
            }
        }

        // Next alternating message should be rejected (depth > MAX_DM_DEPTH)
        bus.cooldowns.clear();
        let err = bus
            .send(
                if MAX_DM_DEPTH % 2 == 0 {
                    "alice"
                } else {
                    "bob"
                },
                if MAX_DM_DEPTH % 2 == 0 { a } else { b },
                if MAX_DM_DEPTH % 2 == 0 {
                    "bob"
                } else {
                    "alice"
                },
                if MAX_DM_DEPTH % 2 == 0 { b } else { a },
                "one more",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SendError::DepthExceeded));
    }

    #[tokio::test]
    async fn test_cooldown_enforced() {
        let (bus, _rx) = setup();
        let a = AgentId::new();
        let b = AgentId::new();

        // First message should succeed
        bus.send("alice", a, "bob", b, "msg1").await.unwrap();

        // Immediate second message should be rejected (cooldown)
        let err = bus.send("alice", a, "bob", b, "msg2").await.unwrap_err();
        assert!(matches!(err, SendError::CooldownActive));
    }

    #[tokio::test]
    async fn test_cooldown_symmetric() {
        let (bus, _rx) = setup();
        let a = AgentId::new();
        let b = AgentId::new();

        // A -> B succeeds
        bus.send("alice", a, "bob", b, "msg1").await.unwrap();

        // B -> A should also be blocked (same DM pair)
        let err = bus.send("bob", b, "alice", a, "reply").await.unwrap_err();
        assert!(matches!(err, SendError::CooldownActive));
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
}
