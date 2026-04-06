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
//!
//! ## Module layout
//!
//! - [`bus`] -- `MessageBus` struct and `MessageSender` trait implementation
//! - [`mod`] (this file) -- shared types (`RunTrigger`, `MessageSource`) and constants

mod bus;

#[cfg(test)]
mod tests;

// Re-export the public API so external callers still use
// `alms_coordinator::message_bus::MessageBus` etc.
pub use bus::MessageBus;

use alms_core::{AgentId, SessionId};
use alms_tools::message_sender::ConversationEndReason;
use serde::{Deserialize, Serialize};

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
        /// The session the peer was in when they first called `send_message`
        /// for this DM pair (e.g. `web-chat-12345`). If present, the
        /// notification run is routed to this session so the user sees the
        /// agent's reaction. If `None`, the notification falls back to the
        /// `notifications:{agent}` session.
        source_session_id: Option<SessionId>,
    },
}
