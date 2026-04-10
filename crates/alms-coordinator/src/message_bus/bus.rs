//! `MessageBus` struct and `MessageSender` trait implementation.
//!
//! Contains the core message routing logic: `send()` delivers peer-to-peer
//! DMs via shared sessions, and `end_conversation()` handles the DM
//! lifecycle end (marker write, depth reset, peer notification).

use super::{DEPTH_EXPIRY_SECS, MAX_DM_DEPTH, MessageSource, RunTrigger};
use alms_core::{AgentId, SessionId, dm_context_id};
use alms_session::SessionManager;
use alms_tools::message_sender::{
    ConversationEndReason, DeliveryReceipt, MessageSender, SendError,
};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, instrument, warn};

// ---------------------------------------------------------------------------
// MessageBus
// ---------------------------------------------------------------------------

/// Agent-to-agent message bus.
///
/// Handles delivery of messages between agents, creating shared DM sessions
/// as needed and triggering runs on the receiving agent.
#[derive(Debug)]
pub struct MessageBus {
    pub(super) session_manager: Arc<SessionManager>,
    /// Channel to trigger runs on the gateway.
    run_trigger_tx: mpsc::UnboundedSender<RunTrigger>,
    /// Per-DM-pair depth tracker: "dm:a:b" -> (last_sender_name, depth).
    /// Depth increments each time the sender changes within the same pair.
    pub(super) depths: DashMap<String, (String, u32)>,
    /// Per-DM-pair last activity timestamp for depth expiry.
    pub(super) last_activity: DashMap<String, Instant>,
    /// Per-DM-pair per-agent source session tracking.
    ///
    /// Key: `(dm_context, agent_name)` -- e.g. `("dm:alice:bob", "alice")`.
    /// Value: the SessionId the agent was in when they first called
    /// `send_message` for this DM pair (e.g. their web-chat session).
    ///
    /// Used by `end_conversation` to route the notification run to the
    /// peer's source session instead of an invisible `notifications:` session.
    /// Entries are cleaned up alongside depth expiry.
    pub(super) source_sessions: DashMap<(String, String), SessionId>,
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
            source_sessions: DashMap::new(),
        }
    }

    /// Remove all source-session entries for a given DM context.
    ///
    /// Called during depth expiry and conversation end to clean up
    /// the `source_sessions` map.
    fn remove_source_sessions_for_dm(&self, dm_context: &str) {
        self.source_sessions.retain(|(ctx, _), _| ctx != dm_context);
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
        sender_session_id: Option<SessionId>,
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
            self.remove_source_sessions_for_dm(&dm_context);
        }

        // Opportunistic cleanup: remove expired entries from all DashMaps
        // to prevent unbounded growth from accumulated DM pairs. We only
        // retain entries that have been active within the expiry window.
        self.last_activity.retain(|key, last| {
            if last.elapsed().as_secs() >= DEPTH_EXPIRY_SECS {
                self.depths.remove(key);
                self.remove_source_sessions_for_dm(key);
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
            //
            // Resilience: if end_conversation fails (e.g. trigger channel is
            // closed), we log and still return DepthExceeded. See #393 tests:
            // test_depth_exceeded_returns_depth_exceeded_even_when_end_conversation_noop
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

        // --- Track source session for notification routing ---
        //
        // Store the sender's current session as their "source session" for
        // this DM pair. Only record it on the FIRST send_message call --
        // subsequent messages sent from within the DM session itself should
        // not overwrite the original source. We use `or_insert` to preserve
        // the first entry.
        //
        // IMPORTANT: Skip recording when the sender_session_id IS the DM
        // session itself. This happens when an agent is triggered by a DM
        // (runs on the DM session) and calls send_message to reply -- its
        // session_id is the DM session, which is not user-facing. Recording
        // it would defeat the `notifications:` fallback. See PR #433 review.
        if let Some(sid) = sender_session_id {
            let dm_session_id = SessionId::deterministic_dm(sender_name, recipient_name);
            if sid != dm_session_id {
                let key = (dm_context.clone(), sender_name.to_string());
                // or_insert preserves the first entry. This matters when an
                // agent's initial send_message is from a web-chat session, but
                // follow-up messages come from DM-triggered runs. We want to
                // keep the web-chat.
                self.source_sessions.entry(key).or_insert(sid);
            }
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

        // --- Look up source sessions for notification routing ---
        //
        // Both the peer AND the sender may need notification runs:
        //
        // - **Peer** (the other agent): always gets a notification so it
        //   knows the conversation ended. Routed to the peer's source
        //   session if available, otherwise to `notifications:{peer_name}`.
        //
        // - **Sender** (the agent that ended the conversation): gets a
        //   notification run ONLY if they have a source session (meaning
        //   they initiated the DM from a user-facing session). This
        //   ensures the user watching the initiator's web-chat sees the
        //   notification with the conversation transcript. Agents without
        //   a source session (pure DM recipients) do NOT get a
        //   self-notification.
        //
        // Both lookups must happen BEFORE `remove_source_sessions_for_dm`
        // cleans up the map.

        // Peer's source session
        let peer_source_key = (dm_context.clone(), peer_name.to_string());
        let peer_source_session = self
            .source_sessions
            .get(&peer_source_key)
            .map(|entry| *entry.value());

        // Sender's source session (#556)
        let sender_source_key = (dm_context.clone(), sender_name.to_string());
        let sender_source_session = self
            .source_sessions
            .get(&sender_source_key)
            .map(|entry| *entry.value());

        let (target_session_id, target_context_id) = if let Some(source_sid) = peer_source_session {
            // Route notification to the peer's source session.
            // Reconstruct the context_id from the session -- use the session's
            // context_id if available, otherwise fall back to notifications.
            let ctx = self
                .session_manager
                .get(source_sid)
                .ok()
                .map(|s| s.context_id.clone())
                .unwrap_or_else(|| format!("notifications:{peer_name}"));
            info!(
                peer = %peer_name,
                source_session = %source_sid.0,
                "Routing notification to peer's source session"
            );
            (source_sid, ctx)
        } else {
            let ctx = format!("notifications:{peer_name}");
            let sid = SessionId::deterministic(&ctx);
            (sid, ctx)
        };

        // --- Clean up source sessions for this DM pair ---
        self.remove_source_sessions_for_dm(&dm_context);

        // --- Emit RunTrigger for the peer agent ---

        let input = format!("[DM conversation ended] Agent {sender_name} ended the conversation.");

        let trigger = RunTrigger {
            agent_id: peer_agent_id,
            session_id: target_session_id,
            input: input.clone(),
            source: MessageSource::ConversationEnded {
                from_agent: sender_agent_id,
                from_name: sender_name.to_string(),
                reason,
                source_session_id: peer_source_session,
            },
            context_id: target_context_id,
        };

        if let Err(e) = self.run_trigger_tx.send(trigger) {
            warn!(
                error = %e,
                "Failed to send RunTrigger for conversation end notification (receiver dropped)"
            );
        }

        // --- Emit RunTrigger for the sender (initiator notification, #556) ---
        //
        // When the sender has a source session, they initiated the DM from
        // a user-facing session and the user expects to see the conversation
        // outcome there. Without this trigger, the initiator only receives a
        // lightweight SSE marker (via notify_dm_ended_to_webchat) but no
        // actual notification run with the conversation transcript.
        //
        // Pure DM recipients (no source session) do NOT get a self-notification
        // because there is no user-facing session to route it to.
        if let Some(source_sid) = sender_source_session {
            let ctx = self
                .session_manager
                .get(source_sid)
                .ok()
                .map(|s| s.context_id.clone())
                .unwrap_or_else(|| format!("notifications:{sender_name}"));
            info!(
                sender = %sender_name,
                source_session = %source_sid.0,
                "Routing self-notification to sender's source session (#556)"
            );
            let sender_trigger = RunTrigger {
                agent_id: sender_agent_id,
                session_id: source_sid,
                input,
                source: MessageSource::ConversationEnded {
                    from_agent: peer_agent_id,
                    from_name: peer_name.to_string(),
                    reason,
                    source_session_id: Some(source_sid),
                },
                context_id: ctx,
            };
            if let Err(e) = self.run_trigger_tx.send(sender_trigger) {
                warn!(
                    error = %e,
                    "Failed to send RunTrigger for sender self-notification (receiver dropped)"
                );
            }
        }

        info!(
            session_id = %session_id.0,
            "DM conversation ended, depth counter reset"
        );

        Ok(())
    }
}
