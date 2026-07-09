//! `MessageBus` struct and `MessageSender` trait implementation.
//!
//! Contains the core message routing logic: `send()` delivers peer-to-peer
//! DMs via shared sessions, and `end_conversation()` handles the DM
//! lifecycle end (marker write, depth reset, peer notification).

use super::{DEPTH_EXPIRY_SECS, DmEvent, MAX_DM_DEPTH, MessageSource, RunTrigger};
use alms_core::{AgentId, SessionId, dm_context_id};
use alms_session::SessionManager;
use alms_tools::message_sender::{
    ConversationEndReason, DeliveryReceipt, MessageSender, SendError,
};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

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
    ///
    /// Bounded (#842 / B11): a runaway producer can no longer grow this queue
    /// without limit. `MessageBus::send` / `end_conversation` are async and
    /// push triggers with `Sender::send().await`, so when the buffer is full
    /// the sender applies back-pressure (awaits a free slot) rather than
    /// dropping the trigger — a lost `RunTrigger` would silently strand a DM
    /// turn, exactly the failure class #1154 is closing.
    run_trigger_tx: mpsc::Sender<RunTrigger>,
    /// Channel to notify the gateway of DM message persistence so SSE events
    /// can be emitted to viewers watching the DM session. See #632.
    ///
    /// Bounded (#842 / B11) for the same reason as `run_trigger_tx`; these
    /// events drive live DM SSE forwarding and are pushed with back-pressure.
    dm_event_tx: Option<mpsc::Sender<DmEvent>>,
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
    /// Tombstones for DM pairs whose depth entry was removed by the
    /// `DEPTH_EXPIRY_SECS` inactivity sweep (#1154 / B5).
    ///
    /// `end_conversation` uses `depths.remove()` as its atomicity guard:
    /// `None` historically meant "already ended by the peer" and the call
    /// returned without writing the `dm_ended` marker or notifying the
    /// peer. But the expiry sweep ALSO removes depth entries — conflating
    /// "peer ended it" with "expiry swept it" meant a late
    /// `ignore_message` / `end_conversation` on a swept pair silently
    /// dropped the end: no marker, no `ConversationEnded` trigger, peer
    /// stranded. The tombstone separates the two cases: sweeps record the
    /// pair here, and `end_conversation` proceeds with the full end when
    /// it finds (and consumes) a tombstone for a pair with no live depth
    /// entry. Entries are removed when consumed, when the pair becomes
    /// active again (`send`), or after `DEPTH_EXPIRY_SECS` via the
    /// opportunistic cleanup pass.
    pub(super) expired_pairs: DashMap<String, Instant>,
}

impl MessageBus {
    /// Create a new MessageBus.
    pub fn new(
        session_manager: Arc<SessionManager>,
        run_trigger_tx: mpsc::Sender<RunTrigger>,
    ) -> Self {
        Self {
            session_manager,
            run_trigger_tx,
            dm_event_tx: None,
            depths: DashMap::new(),
            last_activity: DashMap::new(),
            source_sessions: DashMap::new(),
            expired_pairs: DashMap::new(),
        }
    }

    /// Attach a DM event channel for SSE forwarding.
    ///
    /// When set, the MessageBus emits [`DmEvent`] notifications whenever a
    /// message is persisted to a DM session. The gateway's `dm_event_loop`
    /// consumes these and pushes SSE events to viewers watching that session.
    /// See #632.
    pub fn with_dm_event_channel(mut self, tx: mpsc::Sender<DmEvent>) -> Self {
        self.dm_event_tx = Some(tx);
        self
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
        // a new conversation burst can start fresh. The sweep leaves a
        // tombstone in `expired_pairs` so a late `end_conversation` can
        // distinguish "swept by expiry" from "already ended by the peer"
        // (#1154 / B5).
        if let Some(last) = self.last_activity.get(&dm_context)
            && last.elapsed().as_secs() >= DEPTH_EXPIRY_SECS
        {
            if self.depths.remove(&dm_context).is_some() {
                self.expired_pairs
                    .insert(dm_context.clone(), Instant::now());
            }
            self.remove_source_sessions_for_dm(&dm_context);
        }

        // Opportunistic cleanup: remove expired entries from all DashMaps
        // to prevent unbounded growth from accumulated DM pairs. We only
        // retain entries that have been active within the expiry window.
        self.last_activity.retain(|key, last| {
            if last.elapsed().as_secs() >= DEPTH_EXPIRY_SECS {
                if self.depths.remove(key).is_some() {
                    self.expired_pairs.insert(key.clone(), Instant::now());
                }
                self.remove_source_sessions_for_dm(key);
                false
            } else {
                true
            }
        });

        // Tombstones are themselves bounded: a swept pair whose end never
        // arrives within another DEPTH_EXPIRY_SECS window is dropped (a
        // later end_conversation then takes the historical skip path).
        self.expired_pairs
            .retain(|_, swept_at| swept_at.elapsed().as_secs() < DEPTH_EXPIRY_SECS);

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

        // The pair is live again (a depth entry now exists): drop any sweep
        // tombstone so a future double-end after a proper `end_conversation`
        // is not mistaken for a swept pair (which would emit a duplicate
        // marker + trigger).
        self.expired_pairs.remove(&dm_context);

        // B7 (#1154): stamp `last_activity` the moment the depth entry exists,
        // BEFORE the depth-exceeded check and the `append_message` that can
        // fail below. Both expiry paths key off `last_activity`, so if the
        // first message's `append_message` returns `Err` after the depth
        // entry was created, an entry stamped only on the success path (the
        // historical `self.last_activity.insert` further down) would leave a
        // `depths` entry that NO sweep can ever reclaim. The pair's *next*
        // conversation then resumes from the stale depth and can hit
        // `DepthExceeded` after one exchange ("DM randomly stops"). Stamping
        // here keeps the invariant "every `depths` entry has a matching
        // `last_activity`" intact at every early return after this point.
        // The DepthExceeded branch below removes both maps via
        // `end_conversation`, and the success path re-stamps the timestamp
        // after the append — so this is the floor, not the only write.
        self.last_activity
            .insert(dm_context.clone(), Instant::now());

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
        // IMPORTANT: Record a source session unless it is (a) an internal
        // non-conversational session (notification, subagent, episodic),
        // or (b) the *same* DM session that this send creates — a DM session
        // cannot be its own source.  Other DM sessions ARE valid sources
        // (cross-DM scenario: Alice is in DM-with-Bob and sends to Charlie;
        // the DM-with-Bob session is Alice's source for the Alice→Charlie DM).
        // See #656 (original same-DM guard) and #680 (whitelist overcorrection).
        //
        // Job sessions (`job_*`) ARE valid sources as of #1198: a scheduled
        // job's agent sends DMs from its job session, and the job-episode
        // model needs the `ConversationEnded` trigger to (a) exist for the
        // job agent even when IT ends the conversation (the sender
        // self-notification in `end_conversation` fires only when a source
        // session is recorded), and (b) route back to the job session so the
        // agent resumes with its full job context instead of on the invisible
        // `notifications:{agent}` session. See
        // docs/jobs-await-completion-design.md § D3.
        if let Some(sid) = sender_session_id {
            let is_valid_source = self
                .session_manager
                .get(sid)
                .ok()
                .map(|s| {
                    let session_type = alms_core::classify_session_type(&s.context_id);
                    // Reject internal/non-conversational session types.
                    let not_internal =
                        !matches!(session_type, "notification" | "subagent" | "episodic");
                    // Reject the same DM session — it cannot be its own source.
                    let not_same_dm = s.context_id != dm_context;
                    not_internal && not_same_dm
                })
                .unwrap_or(false);
            if is_valid_source {
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

        // --- Emit DmEvent for SSE forwarding (#632) ---
        //
        // Notify the gateway that a new message was persisted to the DM
        // session so it can push an SSE event to any web UI client watching
        // this session live.  Without this, DM messages are invisible during
        // live viewing and only appear on reload.
        if let Some(ref tx) = self.dm_event_tx {
            // Bounded send with back-pressure (#842 / B11): `await`s a free
            // slot if the buffer is full rather than dropping the event.
            // Err means the receiver was dropped (gateway shutting down) —
            // best-effort, log and continue.
            if let Err(e) = tx
                .send(DmEvent {
                    session_id,
                    from_agent: sender_name.to_string(),
                    from_agent_id: sender_agent_id,
                    message: message.to_string(),
                    ts: Utc::now(),
                })
                .await
            {
                debug!(
                    error = %e,
                    "Failed to send DmEvent for SSE forwarding (receiver dropped)"
                );
            }
        }

        // --- Refresh last activity for depth expiry ---
        //
        // `last_activity` was already stamped at depth-entry creation (B7
        // floor, above). This re-stamp moves it forward to "message actually
        // persisted" time on the success path, so the expiry clock measures
        // idle time since the last *delivered* message rather than since the
        // send was attempted.
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

        // Bounded send with back-pressure (#842 / B11): never drop a
        // `RunTrigger` — a lost trigger silently strands the peer's DM turn.
        // `await`s a free slot if the buffer is full; Err only when the
        // receiver was dropped (gateway shutting down).
        if let Err(e) = self.run_trigger_tx.send(trigger).await {
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

        if was_active {
            // Defensive: a live depth entry means any old tombstone is
            // stale (the pair restarted after a sweep) — drop it so a
            // later duplicate end takes the skip path below.
            self.expired_pairs.remove(&dm_context);
        } else {
            // No live depth entry. Distinguish "already ended by the
            // peer" (skip — the peer's call wrote the marker and emitted
            // the triggers) from "removed by the inactivity sweep"
            // (#1154 / B5: proceed — nobody has signalled the end yet,
            // and skipping would strand the peer with no `dm_ended`
            // marker and no `ConversationEnded` notification). The
            // tombstone `remove()` doubles as the atomicity guard for
            // concurrent post-sweep end calls: only the caller that
            // consumes the tombstone proceeds.
            let was_swept = self.expired_pairs.remove(&dm_context).is_some();
            if !was_swept {
                info!(
                    session_id = %session_id.0,
                    "end_conversation skipped -- already ended by peer"
                );
                return Ok(());
            }
            info!(
                session_id = %session_id.0,
                "end_conversation on expiry-swept pair -- proceeding with \
                 marker write and peer notification (#1154 / B5)"
            );
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
        //   self-notification whenever there is a "human side" to this
        //   conversation -- i.e. EITHER agent initiated from a source
        //   session. It is routed to the sender's OWN source session when
        //   it has one (#556 initiator-ends / #1198 D3 job-ends), otherwise
        //   to `notifications:{sender_name}` (#1215 receiver-ends): a pure
        //   recipient that ENDS the DM must still be notified in its
        //   notification session, mirroring how the peer trigger routes a
        //   source-less peer. When NEITHER agent has a source session (both
        //   DM-triggered / internal) the sender gets NO self-notification --
        //   there is no user watching either side and firing one would break
        //   the "exactly one trigger per end" idempotency/atomicity guards.
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
                reason: reason.clone(),
                // Peer notification: `from_name` (the sender) IS the ender, so
                // "Agent {from_name} ended the conversation" is correct.
                self_notification: false,
                source_session_id: peer_source_session,
            },
            context_id: target_context_id,
        };

        // Bounded send with back-pressure (#842 / B11) — never drop the
        // peer's end notification.
        if let Err(e) = self.run_trigger_tx.send(trigger).await {
            warn!(
                error = %e,
                "Failed to send RunTrigger for conversation end notification (receiver dropped)"
            );
        }

        // --- Emit RunTrigger for the sender (self-notification, #556 / #1215) ---
        //
        // The sender (the agent that ended the conversation) is notified
        // whenever EITHER agent has a source session -- i.e. this was a
        // "real" DM with a human side, not a purely internal/DM-triggered
        // exchange:
        //
        // - Sender HAS a source session: route to it, so the user watching
        //   that session sees the outcome with the conversation transcript
        //   (#556 initiator-ends; #1198 D3 job-ends -> resumes on the job
        //   session with full job context, source_session_id = Some).
        //
        // - Sender has NO source session but the PEER does: this is the
        //   #1215 receiver-ends case. Route to `notifications:{sender_name}`
        //   with source_session_id = None, mirroring how the peer trigger
        //   routes a source-less peer. The gateway then surfaces the
        //   DM-ended banner on the receiver's web-chat (if any) instead of
        //   the receiver getting NOTHING.
        //
        // - NEITHER agent has a source session: no self-notification (the
        //   `both source-less` gate). Both notifications would be invisible
        //   `notifications:` runs with no user watching, and emitting one
        //   would double the trigger count, breaking the "exactly one
        //   trigger per end" idempotency/atomicity guarantees.
        if sender_source_session.is_some() || peer_source_session.is_some() {
            let (sender_target_session_id, sender_target_context_id, sender_source_field) =
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
                    (source_sid, ctx, Some(source_sid))
                } else {
                    let ctx = format!("notifications:{sender_name}");
                    let sid = SessionId::deterministic(&ctx);
                    info!(
                        sender = %sender_name,
                        "Routing source-less ender's self-notification to \
                         notifications:{sender_name} (#1215 receiver-ends)"
                    );
                    (sid, ctx, None)
                };
            let sender_trigger = RunTrigger {
                agent_id: sender_agent_id,
                session_id: sender_target_session_id,
                input,
                source: MessageSource::ConversationEnded {
                    from_agent: peer_agent_id,
                    from_name: peer_name.to_string(),
                    reason,
                    // Self-notification: the RECIPIENT (sender) ended the DM
                    // and `from_name` is the PEER, so the formatter must use
                    // self-appropriate wording and never blame the peer (#1215).
                    self_notification: true,
                    source_session_id: sender_source_field,
                },
                context_id: sender_target_context_id,
            };
            // Bounded send with back-pressure (#842 / B11).
            if let Err(e) = self.run_trigger_tx.send(sender_trigger).await {
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
