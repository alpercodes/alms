//! Trait for sending peer messages between agents.
//!
//! This trait is defined in `alms-runtime` so the tools (SendMessageTool, etc.)
//! can reference it without depending on `alms-coordinator`. The `MessageBus`
//! in `alms-coordinator` implements this trait.

use alms_core::SessionId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Confirmation returned after successful message delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    /// The shared DM session where the message was persisted.
    pub session_id: SessionId,
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
    async fn send(
        &self,
        sender_name: &str,
        sender_agent_id: alms_core::AgentId,
        recipient_name: &str,
        recipient_agent_id: alms_core::AgentId,
        message: &str,
    ) -> Result<DeliveryReceipt, SendError>;
}
