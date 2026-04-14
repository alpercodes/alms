//! Trait for sending peer messages between agents.
//!
//! This trait is defined in `alms-tools` so the tools (SendMessageTool, etc.)
//! can reference it without depending on `alms-coordinator`. The `MessageBus`
//! in `alms-coordinator` implements this trait.

use alms_core::{AgentId, SessionId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Confirmation returned after successful message delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    /// The shared DM session where the message was persisted.
    pub session_id: SessionId,
}

/// Reason a DM conversation was ended.
///
/// Used by `end_conversation` to communicate why the conversation was
/// terminated. The `MessageBus` writes this reason into the DM session
/// metadata marker and includes it in the `RunTrigger` for peer notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationEndReason {
    /// The agent called `ignore_message` (chose not to reply).
    Ignored,
    /// The DM depth limit (`MAX_DM_DEPTH`) was reached.
    DepthExceeded,
    /// The agent's runtime iteration limit was reached during a DM run
    /// without the agent calling `send_message` or `ignore_message`.
    MaxIterations,
}

impl std::fmt::Display for ConversationEndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ignored => write!(f, "ignored"),
            Self::DepthExceeded => write!(f, "depth_exceeded"),
            Self::MaxIterations => write!(f, "max_iterations"),
        }
    }
}

/// Error type for message delivery failures.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("Recipient agent '{0}' not found in registry")]
    RecipientNotFound(String),

    #[error("Message depth exceeded maximum -- possible loop")]
    DepthExceeded,

    #[error("Cannot send message to self")]
    SelfMessage,

    #[error("Delivery failed: {0}")]
    Internal(String),
}

/// Trait for sending peer messages between agents.
///
/// Analogous to `SubagentDispatcher` -- defined in `alms-runtime` so tools
/// can reference it, implemented by `MessageBus` in `alms-coordinator`.
#[async_trait]
pub trait MessageSender: Send + Sync + std::fmt::Debug {
    /// Send a text message from one agent to another.
    ///
    /// The message is written to the shared DM session and a run trigger
    /// is emitted for the recipient. Depth tracking is handled internally
    /// by the implementation -- agents are unaware of these mechanisms.
    ///
    /// `sender_session_id` is the session the sender is currently running in
    /// (e.g. `web-chat-12345`). It is stored as the "source session" for
    /// the sender in this DM pair so that notification runs can be routed
    /// back to that session instead of an invisible `notifications:` session.
    async fn send(
        &self,
        sender_name: &str,
        sender_agent_id: alms_core::AgentId,
        recipient_name: &str,
        recipient_agent_id: alms_core::AgentId,
        message: &str,
        sender_session_id: Option<alms_core::SessionId>,
    ) -> Result<DeliveryReceipt, SendError>;

    /// Signal the end of a DM conversation between two agents.
    ///
    /// This writes a `dm_ended` metadata marker to the shared DM session,
    /// resets the depth counter for the pair, and emits a `RunTrigger` so
    /// the peer agent receives a notification run.
    async fn end_conversation(
        &self,
        sender_name: &str,
        sender_agent_id: AgentId,
        peer_name: &str,
        peer_agent_id: AgentId,
        reason: ConversationEndReason,
    ) -> Result<(), SendError>;
}
