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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum message forwarding depth. Prevents infinite A -> B -> A loops.
const MAX_DM_DEPTH: u32 = 20;

/// Seconds of inactivity after which the depth counter resets for a DM pair.
///
/// Raised from 60s to 1800s (30 minutes) because complex agent runs can
/// easily exceed one minute. See discussion on #362 / decision D5 in #384.
const DEPTH_EXPIRY_SECS: u64 = 1800;

/// Buffer capacity for the bounded `RunTrigger` channel (#842 / B11).
///
/// Was an unbounded channel — a runaway producer (e.g. a tight DM ping-pong
/// or a burst of `ConversationEnded` notifications) could grow it without
/// limit. Producers reserve capacity before mutating DM state. A full buffer
/// therefore produces an explicit, side-effect-free delivery error instead
/// of blocking an agent run that may be needed to free gateway queue space.
pub const RUN_TRIGGER_CHANNEL_CAPACITY: usize = 1024;

/// Buffer capacity for the bounded `DmEvent` channel (#842 / B11).
///
/// Same rationale as [`RUN_TRIGGER_CHANNEL_CAPACITY`]; these events drive
/// live DM SSE forwarding and are pushed with back-pressure.
pub const DM_EVENT_CHANNEL_CAPACITY: usize = 1024;

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

// ---------------------------------------------------------------------------
// DmEvent -- forwarded to the gateway for SSE emission to DM session viewers
// ---------------------------------------------------------------------------

/// An event that occurs on a DM session and needs to be forwarded to SSE
/// subscribers watching that session.
///
/// The `MessageBus` has no access to the gateway's SSE infrastructure, so it
/// sends these events via a channel that the gateway's `dm_event_loop`
/// consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmEvent {
    /// The DM session that the message was persisted to.
    pub session_id: SessionId,
    /// The agent who sent/authored the message.
    pub from_agent: String,
    /// The agent ID of the sender.
    pub from_agent_id: AgentId,
    /// The message text that was persisted.
    pub message: String,
    /// Timestamp of the message.
    pub ts: DateTime<Utc>,
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
    /// Both the peer and the sender (when the sender has a source session)
    /// receive a one-shot notification run so they can act on the
    /// conversation outcome. See #384 for the full lifecycle design and
    /// #556 for the sender self-notification addition.
    ConversationEnded {
        from_agent: AgentId,
        from_name: String,
        reason: ConversationEndReason,
        /// `true` for the ender's own self-notification (#556 / #1215): the
        /// RECIPIENT is the agent that ended the DM, and `from_name` is the
        /// PEER. The formatter must then use self-appropriate wording and must
        /// NEVER attribute the ending to `from_name`. `false` for the peer
        /// notification (the normal case), where `from_name` IS the ender and
        /// "Agent {from_name} ended the conversation" is correct.
        self_notification: bool,
        /// The session the target agent was in when they first called
        /// `send_message` for this DM pair (e.g. `web-chat-12345`).
        /// If present, the notification run is routed to this session so
        /// the user sees the agent's reaction. If `None`, the notification
        /// falls back to the `notifications:{agent}` session.
        ///
        /// For the peer trigger, this is the peer's source session.
        /// For the sender self-notification (#556), this is the sender's
        /// own source session.
        source_session_id: Option<SessionId>,
    },
}
