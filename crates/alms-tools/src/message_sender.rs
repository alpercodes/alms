// SPDX-License-Identifier: Apache-2.0

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationEndReason {
    /// The agent called `ignore_message` (chose not to reply).
    Ignored,
    /// The DM depth limit (`MAX_DM_DEPTH`) was reached.
    DepthExceeded,
    /// The user explicitly cancelled the DM conversation via the API.
    UserCancelled,
    /// The originating run failed (LLM error, tool panic, posture trip, etc.).
    ///
    /// `message` carries a short human-readable error string for the peer-side
    /// notification (typically the `AlmsError` `Display` output, possibly
    /// truncated). Introduced so that DM peer state stays consistent when a
    /// peer-triggered run fails partway through — without this, the depth
    /// counter, `dm_ended` marker, and `ConversationEnded` notification all
    /// stayed unset until the 1800s `DEPTH_EXPIRY_SECS` sweep.
    ///
    /// `interrupted` splits the two materially different situations this
    /// variant covers (#1258):
    ///
    /// - `true` — the run **died**: an LLM/tool failure, a panic, a
    ///   setup failure, or a persistence failure during teardown. It never
    ///   reached the end of its turn, so whatever the DM was going to produce
    ///   does not exist.
    /// - `false` — the run **completed**, but its result was unusable: it
    ///   produced nothing deliverable on its last turn (`dm_lifecycle`
    ///   Exit 3 / #1154), or the final delivery hop failed. Earlier turns of
    ///   the same conversation may well have succeeded, so a transcript
    ///   exists and is worth relaying.
    ///
    /// Only [`is_interrupted`](Self::is_interrupted) reads it; it is not on
    /// the wire ([`Display`](std::fmt::Display) is `"errored"` either way).
    Errored { message: String, interrupted: bool },
}

impl ConversationEndReason {
    /// Whether the DM stopped because a **run was cut short**, rather than
    /// because a run **completed**.
    ///
    /// - *Completed* ([`Ignored`](Self::Ignored),
    ///   [`DepthExceeded`](Self::DepthExceeded), and
    ///   `Errored { interrupted: false }`): a run reached the end of its turn.
    ///   Whatever the conversation produced exists in the DM session, and for
    ///   an `Ignored` / `DepthExceeded` / no-reply end that transcript is the
    ///   only copy — the peer's replies never touched the initiator's
    ///   web-chat.
    /// - *Interrupted* ([`UserCancelled`](Self::UserCancelled) and
    ///   `Errored { interrupted: true }`): the run died mid-turn, or the
    ///   operator explicitly stopped it. For a cancel, the operator asked for
    ///   work here to stop; for a failure, the turn that was going to produce
    ///   the outcome never finished.
    ///
    /// The gateway keys on this to decide whether a `ConversationEnded`
    /// trigger deserves a notification **run** at all — see `run_trigger_loop`
    /// in `alms-gateway/src/runs/notifications.rs` (#1258).
    ///
    /// Note what this does **not** claim: an interrupted DM may still have a
    /// non-empty transcript (a cancelled conversation usually does, and the
    /// #1258 incident's own DM held the peer's opening message). The cut is
    /// "was a turn cut short", not "is the transcript empty" — see
    /// `run_trigger_loop`'s docs for why the transcript is the wrong
    /// predicate here.
    #[must_use]
    pub fn is_interrupted(&self) -> bool {
        match self {
            Self::Ignored | Self::DepthExceeded => false,
            Self::UserCancelled => true,
            Self::Errored { interrupted, .. } => *interrupted,
        }
    }

    /// The human-readable failure detail carried by
    /// [`Errored`](Self::Errored), if any.
    ///
    /// Every construction site routes the *variable* part of `message`
    /// through `runs::lifecycle::truncate_error_for_peer`, which sanitises it
    /// via `sanitize_error_for_session` (#911 / #930 / #931) and bounds it at
    /// `PEER_ERROR_MESSAGE_MAX_LEN` (300 chars). Some sites prepend a short
    /// self-authored prefix (`"reply delivery failed: …"`), so the result is
    /// sanitised and bounded but not exactly ≤ 300 chars. Safe to embed in an
    /// SSE frame and in a persisted marker as-is.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Errored { message, .. } => Some(message.as_str()),
            Self::Ignored | Self::DepthExceeded | Self::UserCancelled => None,
        }
    }
}

impl std::fmt::Display for ConversationEndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ignored => write!(f, "ignored"),
            Self::DepthExceeded => write!(f, "depth_exceeded"),
            Self::UserCancelled => write!(f, "user_cancelled"),
            Self::Errored { .. } => write!(f, "errored"),
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

#[cfg(test)]
mod tests {
    use super::ConversationEndReason;

    /// #1258: the completed/interrupted split is the predicate the gateway
    /// uses to decide whether a DM-ended notification gets an LLM turn. A run
    /// that reached the end of its turn left an outcome in the transcript;
    /// one that was cut short did not.
    #[test]
    fn completed_reasons_are_not_interrupted() {
        assert!(!ConversationEndReason::Ignored.is_interrupted());
        assert!(!ConversationEndReason::DepthExceeded.is_interrupted());
    }

    #[test]
    fn cancel_and_died_run_reasons_are_interrupted() {
        assert!(ConversationEndReason::UserCancelled.is_interrupted());
        assert!(
            ConversationEndReason::Errored {
                message: "LLM rate limit exceeded".to_string(),
                interrupted: true,
            }
            .is_interrupted()
        );
    }

    /// The edge that makes `Errored` a *two*-situation variant: a run that
    /// completed and merely produced nothing deliverable (`dm_lifecycle`
    /// Exit 3) is NOT interrupted, so it keeps its notification run and its
    /// transcript still reaches the operator's chat. Collapsing this into
    /// "errored ⇒ interrupted" silently drops the DM's answer.
    #[test]
    fn a_completed_run_with_an_unusable_result_is_not_interrupted() {
        assert!(
            !ConversationEndReason::Errored {
                message: "agent run completed without producing a reply".to_string(),
                interrupted: false,
            }
            .is_interrupted()
        );
    }

    /// Both `Errored` shapes stay `"errored"` on the wire — the new field is
    /// a routing input, not a protocol change (`DM_END_REASON_LABELS` and the
    /// `dm_ended` marker both key on this string).
    #[test]
    fn the_interrupted_flag_is_not_on_the_wire() {
        assert_eq!(
            ConversationEndReason::Errored {
                message: "boom".to_string(),
                interrupted: true,
            }
            .to_string(),
            "errored"
        );
        assert_eq!(
            ConversationEndReason::Errored {
                message: "boom".to_string(),
                interrupted: false,
            }
            .to_string(),
            "errored"
        );
    }

    /// The failure text is the only reason-specific content the operator can
    /// act on, so it must survive as its own field rather than be flattened
    /// into the `errored` label.
    #[test]
    fn detail_carries_only_the_errored_message() {
        assert_eq!(
            ConversationEndReason::Errored {
                message: "LLM rate limit exceeded".to_string(),
                interrupted: true,
            }
            .detail(),
            Some("LLM rate limit exceeded")
        );
        assert_eq!(ConversationEndReason::Ignored.detail(), None);
        assert_eq!(ConversationEndReason::DepthExceeded.detail(), None);
        assert_eq!(ConversationEndReason::UserCancelled.detail(), None);
    }
}
