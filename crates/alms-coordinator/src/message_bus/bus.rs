// SPDX-License-Identifier: Apache-2.0

//! `MessageBus` struct and `MessageSender` trait implementation.
//!
//! Contains the core message routing logic: `send()` delivers peer-to-peer
//! DMs via shared sessions, and `end_conversation()` handles the DM
//! lifecycle end (marker write, depth reset, peer notification).

use super::{ActivityStamp, DmEvent, MAX_DM_DEPTH, MessageSource, RunTrigger};
use alms_core::{AgentId, SessionId, dm_context_id};
use alms_session::SessionManager;
use alms_tools::message_sender::{
    ConversationEndReason, DeliveryReceipt, MessageSender, SendError,
};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tracing::{debug, info, instrument, warn};

/// Removes an idle per-pair transaction mutex after the current operation.
///
/// This guard must be declared before the mutex clone and lock guard so it
/// drops last. The DashMap shard write lock makes the strong-count check and
/// removal atomic with a concurrent transaction lookup.
struct TransactionCleanup<'a> {
    transactions: &'a DashMap<String, Arc<AsyncMutex<()>>>,
    context: String,
}

impl Drop for TransactionCleanup<'_> {
    fn drop(&mut self) {
        self.transactions
            .remove_if(&self.context, |_, transaction| {
                Arc::strong_count(transaction) == 1
            });
    }
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
    pub(super) session_manager: Arc<SessionManager>,
    /// Channel to trigger runs on the gateway.
    ///
    /// Bounded (#842 / B11): producers reserve capacity before mutating DM
    /// state. Saturation is an explicit, side-effect-free SendError rather
    /// than an await that can deadlock the agent run needed to free capacity.
    run_trigger_tx: mpsc::Sender<RunTrigger>,
    /// Channel to notify the gateway of DM message persistence so SSE events
    /// can be emitted to viewers watching the DM session. See #632.
    ///
    /// Bounded (#842 / B11) for the same reason as `run_trigger_tx`. These
    /// events are best-effort SSE decoration and are dropped on saturation.
    dm_event_tx: Option<mpsc::Sender<DmEvent>>,
    /// Per-DM-pair depth tracker: "dm:a:b" -> (last_sender_name, depth).
    /// Depth increments each time the sender changes within the same pair.
    pub(super) depths: DashMap<String, (String, u32)>,
    /// Per-DM-pair last activity timestamp for depth expiry.
    pub(super) last_activity: DashMap<String, ActivityStamp>,
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
    pub(super) expired_pairs: DashMap<String, ActivityStamp>,
    /// Serializes each DM pair's complete state and persistence transaction.
    /// Its key space matches the deterministic set of persisted DM sessions.
    pub(super) transactions: DashMap<String, Arc<AsyncMutex<()>>>,
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
            transactions: DashMap::new(),
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

    /// Sweep expired pair state while honoring the same transaction boundary
    /// as send/end. Snapshotting keys first avoids holding a DashMap shard
    /// guard across an await.
    async fn sweep_expired_pairs(&self) {
        let expired: Vec<String> = self
            .last_activity
            .iter()
            .filter(|entry| entry.value().is_expired())
            .map(|entry| entry.key().clone())
            .collect();

        for context in expired {
            let _transaction_cleanup = TransactionCleanup {
                transactions: &self.transactions,
                context: context.clone(),
            };
            let transaction = Arc::clone(
                self.transactions
                    .entry(context.clone())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                    .value(),
            );
            let _guard = transaction.lock().await;
            let still_expired = self
                .last_activity
                .get(&context)
                .is_some_and(|last| last.is_expired());
            if still_expired {
                if self.depths.remove(&context).is_some() {
                    self.expired_pairs
                        .insert(context.clone(), ActivityStamp::now());
                }
                self.remove_source_sessions_for_dm(&context);
                self.last_activity.remove(&context);
            }
        }

        let stale_tombstones: Vec<String> = self
            .expired_pairs
            .iter()
            .filter(|entry| entry.value().is_expired())
            .map(|entry| entry.key().clone())
            .collect();
        for context in stale_tombstones {
            let _transaction_cleanup = TransactionCleanup {
                transactions: &self.transactions,
                context: context.clone(),
            };
            let transaction = Arc::clone(
                self.transactions
                    .entry(context.clone())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                    .value(),
            );
            let _guard = transaction.lock().await;
            if self
                .expired_pairs
                .get(&context)
                .is_some_and(|swept| swept.is_expired())
            {
                self.expired_pairs.remove(&context);
            }
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
        sender_session_id: Option<SessionId>,
    ) -> Result<DeliveryReceipt, SendError> {
        // --- Validation ---

        if sender_agent_id == recipient_agent_id {
            return Err(SendError::SelfMessage);
        }

        self.sweep_expired_pairs().await;

        let dm_context = dm_context_id(sender_name, recipient_name);
        let _transaction_cleanup = TransactionCleanup {
            transactions: &self.transactions,
            context: dm_context.clone(),
        };
        let transaction = Arc::clone(
            self.transactions
                .entry(dm_context.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .value(),
        );
        let _transaction_guard = transaction.lock().await;

        // Reserve trigger capacity before mutating DM state. This producer
        // can run inside the same agent turn that ultimately frees gateway
        // queue capacity, so awaiting a full trigger channel would create a
        // circular backpressure deadlock.
        let trigger_permit = self
            .run_trigger_tx
            .clone()
            .try_reserve_owned()
            .map_err(|error| {
                SendError::Internal(format!("run trigger queue unavailable: {error}"))
            })?;

        // Internal depth tracking: increments each time a different sender
        // sends to the same DM pair. If Alice sends, then Bob replies, then
        // Alice replies again, depth goes 1 -> 2 -> 3.
        let (current_depth, depth_exceeded) = {
            let mut entry = self
                .depths
                .entry(dm_context.clone())
                .or_insert_with(|| (String::new(), 0));
            let (last_sender, depth) = entry.value_mut();
            if last_sender != sender_name {
                let next = *depth + 1;
                if next > MAX_DM_DEPTH {
                    (*depth, true)
                } else {
                    *depth = next;
                    *last_sender = sender_name.to_string();
                    (*depth, false)
                }
            } else if depth == &0 {
                // First message in this DM pair
                *depth = 1;
                *last_sender = sender_name.to_string();
                (*depth, false)
            } else {
                (*depth, false)
            }
        };

        if depth_exceeded {
            // No depth/session state was changed for the overflowing message.
            // Release its unused normal-trigger slot, then either complete the
            // terminal notification transaction or return an explicit
            // capacity error that the caller can retry.
            drop(trigger_permit);
            self.end_conversation_locked(
                sender_name,
                sender_agent_id,
                recipient_name,
                recipient_agent_id,
                ConversationEndReason::DepthExceeded,
            )
            .await?;
            return Err(SendError::DepthExceeded);
        }

        // The pair is live again (a depth entry now exists): drop any sweep
        // tombstone so a future double-end after a proper `end_conversation`
        // is not mistaken for a swept pair (which would emit a duplicate
        // marker + trigger).
        self.expired_pairs.remove(&dm_context);

        // B7 (#1154): stamp `last_activity` the moment the depth entry exists,
        // before the `append_message` that can fail below. Expiry keys off
        // `last_activity`, so if the first message's append returns `Err`
        // after the depth
        // entry was created, an entry stamped only on the success path (the
        // historical `self.last_activity.insert` further down) would leave a
        // `depths` entry that NO sweep can ever reclaim. The pair's *next*
        // conversation then resumes from the stale depth and can hit
        // `DepthExceeded` after one exchange ("DM randomly stops"). Stamping
        // here keeps the invariant "every `depths` entry has a matching
        // `last_activity`" intact at every early return after this point.
        // The DepthExceeded branch above removes both maps via the locked end
        // transaction, and the success path re-stamps the timestamp
        // after the append — so this is the floor, not the only write.
        self.last_activity
            .insert(dm_context.clone(), ActivityStamp::now());

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
            // This is best-effort SSE decoration, not delivery state. Never
            // hold the pair transaction behind a slow UI event consumer.
            if let Err(e) = tx.try_send(DmEvent {
                session_id,
                from_agent: sender_name.to_string(),
                from_agent_id: sender_agent_id,
                message: message.to_string(),
                ts: Utc::now(),
            }) {
                debug!(
                    error = %e,
                    "Dropped DmEvent for SSE forwarding"
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
            .insert(dm_context.clone(), ActivityStamp::now());

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

        // Capacity was reserved before any DM state changed, so this send is
        // infallible and cannot participate in circular backpressure.
        trigger_permit.send(trigger);

        info!(
            session_id = %session_id.0,
            depth = current_depth,
            "Peer message delivered to shared DM session"
        );

        Ok(DeliveryReceipt { session_id })
    }

    async fn end_conversation(
        &self,
        sender_name: &str,
        sender_agent_id: AgentId,
        peer_name: &str,
        peer_agent_id: AgentId,
        reason: ConversationEndReason,
    ) -> Result<(), SendError> {
        if sender_agent_id == peer_agent_id {
            return Err(SendError::SelfMessage);
        }

        let dm_context = dm_context_id(sender_name, peer_name);
        let _transaction_cleanup = TransactionCleanup {
            transactions: &self.transactions,
            context: dm_context.clone(),
        };
        let transaction = Arc::clone(
            self.transactions
                .entry(dm_context)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .value(),
        );
        let _transaction_guard = transaction.lock().await;
        self.end_conversation_locked(
            sender_name,
            sender_agent_id,
            peer_name,
            peer_agent_id,
            reason,
        )
        .await
    }
}

impl MessageBus {
    /// End a DM conversation: write a metadata marker, reset depth, and
    /// emit a `RunTrigger` with `ConversationEnded` source for the peer.
    ///
    /// The caller holds the pair transaction lock across the complete
    /// depth/source/persistence boundary. The depth removal remains the
    /// idempotency check for duplicate ends.
    #[instrument(
        level = "info",
        skip(self),
        fields(
            sender = %sender_name,
            peer = %peer_name,
            reason = %reason,
        )
    )]
    async fn end_conversation_locked(
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
        if !self.depths.contains_key(&dm_context) && !self.expired_pairs.contains_key(&dm_context) {
            info!(
                session_id = %session_id.0,
                "end_conversation skipped -- already ended by peer"
            );
            return Ok(());
        }

        let needs_self_notification = self
            .source_sessions
            .contains_key(&(dm_context.clone(), sender_name.to_string()))
            || self
                .source_sessions
                .contains_key(&(dm_context.clone(), peer_name.to_string()));

        // Reserve the exact notification cardinality before the depth/marker
        // transaction so saturation is explicit and side-effect free.
        let peer_trigger_permit =
            self.run_trigger_tx
                .clone()
                .try_reserve_owned()
                .map_err(|error| {
                    SendError::Internal(format!("run trigger queue unavailable: {error}"))
                })?;
        let self_trigger_permit = if needs_self_notification {
            Some(
                self.run_trigger_tx
                    .clone()
                    .try_reserve_owned()
                    .map_err(|error| {
                        SendError::Internal(format!("run trigger queue unavailable: {error}"))
                    })?,
            )
        } else {
            None
        };

        // Inspect but do not consume the idempotency state yet. The pair lock
        // keeps it stable through marker persistence and excludes concurrent
        // send/end operations.
        let was_active = self.depths.contains_key(&dm_context);

        if !was_active {
            // No live depth entry. Distinguish "already ended by the
            // peer" (skip — the peer's call wrote the marker and emitted
            // the triggers) from "removed by the inactivity sweep"
            // (#1154 / B5: proceed — nobody has signalled the end yet,
            // and skipping would strand the peer with no `dm_ended`
            // marker and no `ConversationEnded` notification). The
            // The pair lock excludes concurrent post-sweep end calls; the
            // tombstone remains present until marker persistence commits.
            let was_swept = self.expired_pairs.contains_key(&dm_context);
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
            self.depths.remove(&dm_context);
            self.last_activity.remove(&dm_context);
            self.expired_pairs.remove(&dm_context);
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

        // The fallible marker append committed. Consume the idempotency state
        // only now so a storage failure leaves the end transaction retryable.
        if was_active {
            self.depths.remove(&dm_context);
        }
        self.last_activity.remove(&dm_context);
        self.expired_pairs.remove(&dm_context);

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

        // Capacity for both possible end notifications was reserved before
        // the depth and marker transaction.
        peer_trigger_permit.send(trigger);

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
        if needs_self_notification {
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
            self_trigger_permit
                .expect("self notification capacity reserved")
                .send(sender_trigger);
        }

        info!(
            session_id = %session_id.0,
            "DM conversation ended, depth counter reset"
        );

        Ok(())
    }
}
