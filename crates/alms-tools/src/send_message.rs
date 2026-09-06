// SPDX-License-Identifier: Apache-2.0

//! send_message tool -- sends a peer message from one agent to another.
//!
//! The tool returns immediately with a delivery confirmation; the sender's
//! loop is never blocked on a reply. That is the *only* thing "asynchronous"
//! means here -- delivery itself is immediate, and the exchange is not a
//! queue the sender has to drain:
//!
//! - `MessageBus::send` persists the message to the shared DM session and
//!   emits a `RunTrigger` for the recipient, so the recipient is *invoked*
//!   to process it (`crates/alms-coordinator/src/message_bus/bus.rs`).
//! - The recipient's reply (its final assistant text, delivered by the DM
//!   completion gate under #1154) goes back through the same path, so the
//!   original sender is invoked in turn on the DM session.
//! - If the recipient ends the conversation instead, `end_conversation`
//!   emits a `ConversationEnded` trigger for the peer unconditionally --
//!   and for an end that *completed*, `run_trigger_loop` starts the run, so
//!   the sender is invoked for that outcome too.
//!
//! Nothing in that loop is reached by polling `read_messages`, which is why
//! the tool description (and the delivered-result note below) say so out
//! loud. See #1111 / #1296 and `docs/layer2-peer-messaging-design.md` § 9.1.
//!
//! ## The one end that arrives without a run (#1258 / #1300)
//!
//! The bus emits that third trigger unconditionally, but the trigger loop
//! does not always start a run from it: `plan_triggered_runs` starts **no run
//! on the trigger's own target** when the end `is_interrupted()` --
//! `UserCancelled`, or `Errored { interrupted: true }` (the peer's run was
//! cancelled or died mid-turn). The unconditional emit is therefore true of
//! the bus and not of the run; the suppression is one crate away, in
//! `crates/alms-gateway/src/runs/notifications.rs`.
//!
//! Read that plan in **evaluation order**, because its first arm is an
//! exception to the suppression rather than a case of it: a resolved #1198
//! job episode routes a continuation onto the *job* session and wins over
//! the interrupted-end check, because "dropping it would stall the job until
//! its deadline". The run-less population is therefore narrower than "every
//! interrupted end" -- it is an interrupted end that resolves no open job
//! episode.
//!
//! #1300 gives that population a delivery instead of silence. In place of
//! the run, the plan returns an `InterruptedEndRecord` and the loop persists
//! it with `persist_error_marker` (#874) onto the same session.
//!
//! **Which session that is, is not "the one you called this from".** The bus
//! routes an end notification to the recorded *source session* for the pair,
//! and two rules make that diverge from `sender_session_id`: the entry is
//! stored with `or_insert`, so the FIRST session an agent messaged this peer
//! from wins and later sends from elsewhere do not move it; and
//! `is_valid_source` rejects internal contexts (`notification` / `subagent` /
//! `episodic`, and the DM session itself), so a send made from one of those
//! records no source at all and the end routes to `notifications:{name}`.
//! The agent-facing strings therefore say "a later turn" and name no session
//! -- a locative would be wrong at both of those edges.
//!
//! `kind: "error"` is the one marker
//! shape `session_msg_to_llm` rewrites into a surviving `[Error] ...` user
//! message, so it reaches the model on that session's next turn. It has to
//! be that shape: the `dm_ended` bus record is empty-text, so `dm_filter`'s
//! `is_synthetic_marker` hides it from `read_messages` *and* `read_session`,
//! and the `dm_ended_notification` marker is `Role::System` + synthetic and
//! is stripped before the provider.
//!
//! [`SendMessageTool::description`] and [`DELIVERED_NOTE`] therefore keep
//! saying "you are notified" without hedging it, and name the two deliveries
//! it can arrive by. Before #1300 they named only the first, deliberately:
//! there was no agent-visible signal at all, so the only caveat available
//! was "you will *usually* be invoked", which leaves exactly one lever
//! (check) -- the polling instinct #1111 exists to close, and one that
//! provably could not detect this case. A caveat is worth its tokens only if
//! it changes what the reader can do; naming a delivery that arrives by
//! itself adds a fact and no lever, which is what makes the second clause
//! worth stating now and not before.

use crate::message_sender::{MessageSender, SendError};
use alms_core::{AgentId, SessionId};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use alms_session::SessionManager;
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

/// Note attached to a successful delivery, re-read by the agent when it
/// plans its next step.
///
/// Carries the same mental model as the tool description: the recipient is
/// being *invoked*, the sender will be invoked back, and `read_messages` is
/// not a waiting room (#1111).
const DELIVERED_NOTE: &str = "Delivered. The recipient is now being invoked in your shared DM \
     session to process this. Their reply triggers a new run there and you will be invoked to \
     handle it; if they end the conversation instead, you are notified then -- by a run, or, \
     when that end was itself cut short, by a note in a later turn rather than a run of \
     its own. Do NOT poll read_messages waiting for the reply -- it will not arrive any \
     sooner, and the system resumes you when it does.";

/// Built-in tool that sends a message to another registered agent.
///
/// The message is delivered to the shared DM session and a run is
/// triggered so the recipient can process it. The sender does NOT block
/// waiting for a response.
#[derive(Debug)]
pub struct SendMessageTool {
    sender: Arc<dyn MessageSender>,
    sender_agent_id: AgentId,
    sender_name: String,
    /// Session manager for agent name resolution.
    session_manager: Arc<SessionManager>,
    /// The session the sender is currently running in.
    ///
    /// Passed to `MessageSender::send()` so the MessageBus can track it
    /// as the "source session" for notification routing.
    sender_session_id: SessionId,
    /// The one peer this run may not `send_message`, and why.
    ///
    /// A call whose recipient matches is *folded*: the tool returns a
    /// non-error result explaining that nothing was sent, and delivers
    /// nothing. `send_message` stays valid for every *other* recipient —
    /// the fold removes exactly one target, never the tool.
    ///
    /// Two run kinds set it (see [`DmFoldReason`]); every other run leaves
    /// it `None` and gets no fold.
    dm_peer: Option<(String, DmFoldReason)>,
}

/// Why `send_message` toward [`SendMessageTool::dm_peer`] is folded.
///
/// The variants differ only in what the agent is told, but they must differ:
/// the implicit-reply note promises the text reaches the peer anyway, which
/// is true of a live DM turn and false once the conversation has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmFoldReason {
    /// #1154: this run is a peer-triggered DM turn. The agent's final
    /// assistant text IS the reply, delivered by the DM completion gate, so
    /// a `send_message` at the peer would double-deliver.
    ImplicitReply,
    /// #1299: this run is the post-end turn for a conversation that just
    /// ended with this peer (a `ConversationEnded` notification run). There
    /// is no completion gate here — a send at the peer would simply re-open
    /// the conversation at depth 1, which is how a pair converses without
    /// bound despite `MAX_DM_DEPTH`.
    ConversationEnded,
}

impl SendMessageTool {
    pub fn new(
        sender: Arc<dyn MessageSender>,
        sender_agent_id: AgentId,
        sender_name: String,
        session_manager: Arc<SessionManager>,
        sender_session_id: SessionId,
    ) -> Self {
        Self {
            sender,
            sender_agent_id,
            sender_name,
            session_manager,
            sender_session_id,
            dm_peer: None,
        }
    }

    /// Set the current DM peer so `send_message` calls aimed at the peer are
    /// folded as an implicit reply instead of delivered
    /// ([`DmFoldReason::ImplicitReply`]).
    pub fn with_dm_peer(mut self, peer: Option<String>) -> Self {
        self.dm_peer = peer.map(|peer| (peer, DmFoldReason::ImplicitReply));
        self
    }

    /// Set the peer whose DM conversation has just ended, so this run's
    /// `send_message` calls aimed at that peer are folded
    /// ([`DmFoldReason::ConversationEnded`], #1299).
    ///
    /// Mutually exclusive with [`Self::with_dm_peer`] — they write the same
    /// field, so the last call wins. A run is either a live DM turn or the
    /// post-end turn, never both.
    pub fn with_ended_dm_peer(mut self, peer: Option<String>) -> Self {
        self.dm_peer = peer.map(|peer| (peer, DmFoldReason::ConversationEnded));
        self
    }

    /// Build the folded (not-delivered) result for a `send_message` call
    /// aimed at the folded peer.
    fn folded_peer_result(peer: &str, reason: DmFoldReason) -> Value {
        let note = match reason {
            DmFoldReason::ImplicitReply => format!(
                "You are already in a direct conversation with '{peer}'. Your \
                 final reply text is delivered to them automatically — do NOT \
                 use send_message for this. The message was NOT sent; include \
                 the content in your final reply text instead."
            ),
            DmFoldReason::ConversationEnded => format!(
                "Your conversation with '{peer}' has just ended — this turn is \
                 for acting on that outcome, not for continuing it. The message \
                 was NOT sent and nothing you write reaches '{peer}' this turn; \
                 messaging them again here would only re-open the conversation \
                 you were notified about. Record what you learned, update your \
                 goals or memories, or report to someone else instead."
            ),
        };
        serde_json::json!({
            "delivered": false,
            "folded": true,
            "note": note,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a direct message to another agent. This is the PEER \
         relationship: they run on their own and their reply is theirs. \
         Sending triggers an LLM run in your shared DM session -- they \
         are actively invoked to process your message, and may think, use \
         tools, or do work before replying. Their reply triggers a run for \
         you in that same DM session: the system invokes you again to handle \
         it. If they end the conversation instead of replying, you are \
         notified then -- by a run, or, when that end was itself cut short, \
         by a note in a later turn rather than a run of its own. \
         'Asynchronous' means your current run is not blocked \
         waiting for the reply -- it does NOT mean delivery is deferred, and \
         it does NOT mean you have to poll: calling read_messages will not \
         make a reply arrive any sooner. Use this for peer work -- asking for \
         a review, sharing an update, requesting help. For a subordinate task \
         whose result you need inline, use invoke_agent. Do NOT use this to \
         reply to an agent you are already in a direct conversation with -- \
         your final reply text is delivered to them automatically."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Name of the target agent (must be registered via `alms agent create`)"
                },
                "message": {
                    "type": "string",
                    "description": "The message to send"
                }
            },
            "required": ["to", "message"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let to = params
            .get("to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'to' is required and must be non-empty".into())
            })?;

        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SandboxError::InvalidParameters(
                    "'message' is required and must be non-empty".into(),
                )
            })?;

        // Fold sends aimed at the folded peer — either the current DM peer
        // (#1154 design default #2: the final reply text is delivered
        // implicitly, so a send would double-deliver) or the peer whose
        // conversation just ended (#1299: a send would re-open it at depth
        // 1). Checked before registry resolution so the fold works even
        // without a SQLite store.
        if let Some((ref peer, reason)) = self.dm_peer
            && to.eq_ignore_ascii_case(peer)
        {
            warn!(
                peer = %peer,
                ?reason,
                "send_message aimed at the folded DM peer — not delivered"
            );
            return Ok(Self::folded_peer_result(peer, reason));
        }

        // Resolve recipient agent from registry
        let store = self.session_manager.store().ok_or_else(|| {
            SandboxError::Io("No agent store available (SQLite not configured)".into())
        })?;

        let recipient = store
            .load_agent_by_name(to)
            .map_err(|e| SandboxError::Io(format!("Failed to look up agent '{to}': {e}")))?;

        let recipient = match recipient {
            Some(r) => r,
            None => {
                return Ok(serde_json::json!({
                    "error": format!("Agent '{to}' not found. Use list_agents to see available agents."),
                }));
            }
        };

        // Defense-in-depth: the registry lookup may resolve an alias /
        // differently-cased name to the canonical record — re-check the
        // canonical name against the folded peer so a non-exact spelling
        // cannot sneak a delivery past the pre-resolution fold.
        if let Some((ref peer, reason)) = self.dm_peer
            && recipient.name.eq_ignore_ascii_case(peer)
        {
            warn!(
                peer = %peer,
                resolved = %recipient.name,
                ?reason,
                "send_message resolved to the folded DM peer — not delivered"
            );
            return Ok(Self::folded_peer_result(peer, reason));
        }

        // Deliver under the recipient's CANONICAL registry name, not the `to`
        // the model typed (#2). `MessageBus::send` derives the DM
        // `context_id` from this string; agent names admit uppercase and
        // resolve case-insensitively, so passing the raw `to` would file
        // `dm:atlas:bob` and `dm:Atlas:bob` as two separate DM sessions for
        // the same pair of agents, splitting the conversation history down
        // the middle of the model's spelling habits.
        match self
            .sender
            .send(
                &self.sender_name,
                self.sender_agent_id,
                &recipient.name,
                recipient.id,
                message,
                Some(self.sender_session_id),
            )
            .await
        {
            Ok(receipt) => Ok(serde_json::json!({
                "delivered": true,
                "dm_session_id": receipt.session_id.0.to_string(),
                "note": DELIVERED_NOTE,
            })),
            Err(SendError::DepthExceeded) => {
                warn!("send_message depth exceeded for {to}");
                Ok(serde_json::json!({
                    "error": "Message depth exceeded maximum -- possible loop detected.",
                }))
            }
            Err(SendError::SelfMessage) => Ok(serde_json::json!({
                "error": "Cannot send a message to yourself.",
            })),
            Err(e) => Err(SandboxError::Io(format!("Message delivery failed: {e}"))),
        }
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SendMessageTool requires a real MessageSender implementation and
    // a SessionManager with SQLite store for agent lookup. Full integration
    // tests live in alms-coordinator's message_bus tests. Here we test
    // parameter validation only.

    /// Minimal mock that always returns an error (never called in param tests).
    #[derive(Debug)]
    struct NoopSender;

    #[async_trait::async_trait]
    impl MessageSender for NoopSender {
        async fn send(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            Err(SendError::Internal("noop".into()))
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Err(SendError::Internal("noop".into()))
        }
    }

    fn make_tool() -> SendMessageTool {
        let mgr = Arc::new(SessionManager::new(alms_session::SessionConfig::default()));
        let sender: Arc<dyn MessageSender> = Arc::new(NoopSender);
        SendMessageTool::new(
            sender,
            AgentId::new(),
            "test-sender".into(),
            mgr,
            SessionId::new(),
        )
    }

    #[tokio::test]
    async fn test_missing_to_is_error() {
        let tool = make_tool();
        let err = tool
            .execute(serde_json::json!({ "message": "hi" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_missing_message_is_error() {
        let tool = make_tool();
        let err = tool
            .execute(serde_json::json!({ "to": "bob" }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_no_store_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "to": "bob", "message": "hi" }))
            .await;
        // Should fail because no SQLite store is configured
        assert!(result.is_err());
    }

    #[test]
    fn test_schema_has_required_fields() {
        let tool = make_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "to"));
        assert!(required.iter().any(|v| v == "message"));
    }

    // ---- DM-peer fold tests (#1154 implicit replies) ----
    //
    // The NoopSender returns an error from `send()`, and `make_tool()` has
    // no SQLite store — so any of these tests passing with `Ok(...)` proves
    // the fold short-circuited BEFORE both registry resolution and delivery.

    #[tokio::test]
    async fn test_send_to_current_dm_peer_is_folded_not_delivered() {
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await
            .expect("fold must be a non-error result");
        assert_eq!(result["delivered"], false);
        assert_eq!(result["folded"], true);
        assert!(
            result.get("error").is_none(),
            "fold must NOT be an error result — the agent should not retry"
        );
    }

    #[tokio::test]
    async fn test_send_to_current_dm_peer_fold_is_case_insensitive() {
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "Alice", "message": "hi" }))
            .await
            .expect("case-insensitive fold must be a non-error result");
        assert_eq!(result["folded"], true);
    }

    #[tokio::test]
    async fn test_send_to_non_peer_during_dm_is_not_folded() {
        // With a DM peer set, a send to a DIFFERENT agent must proceed past
        // the fold — here it hits the missing-store error, proving the fold
        // did not short-circuit.
        let tool = make_tool().with_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "charlie", "message": "hi" }))
            .await;
        assert!(
            result.is_err(),
            "non-peer send must proceed to resolution/delivery (and fail on \
             the missing store here), not be folded"
        );
    }

    // ---- Ended-DM-peer fold tests (#1299 post-end turn) ----
    //
    // Same short-circuit proof as above: `Ok(...)` from a store-less tool
    // whose sender errors can only mean the fold fired first.

    #[tokio::test]
    async fn test_send_to_ended_dm_peer_is_folded_not_delivered() {
        let tool = make_tool().with_ended_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "alice", "message": "let's keep going" }))
            .await
            .expect("fold must be a non-error result");
        assert_eq!(result["delivered"], false);
        assert_eq!(
            result["folded"], true,
            "the post-end turn must not be able to message the peer whose \
             conversation just ended — that re-opens it at depth 1 (#1299)"
        );
        assert!(
            result.get("error").is_none(),
            "fold must NOT be an error result — the agent should not retry"
        );
    }

    /// The two fold reasons must not share a note.
    ///
    /// The implicit-reply note promises the agent that its final reply text
    /// reaches the peer anyway. That is true of a live DM turn and false
    /// after the conversation ended: reusing it post-end would tell the
    /// agent it had replied when nothing was delivered.
    #[tokio::test]
    async fn test_ended_dm_peer_fold_does_not_promise_implicit_delivery() {
        let ended = make_tool()
            .with_ended_dm_peer(Some("alice".into()))
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await
            .expect("fold must be a non-error result");
        let live = make_tool()
            .with_dm_peer(Some("alice".into()))
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await
            .expect("fold must be a non-error result");

        let ended_note = ended["note"].as_str().expect("fold must carry a note");
        let live_note = live["note"].as_str().expect("fold must carry a note");

        assert_ne!(
            ended_note, live_note,
            "the post-end fold must not reuse the implicit-reply note"
        );
        assert!(
            !ended_note.contains("automatically"),
            "the post-end fold must not promise automatic delivery — nothing \
             reaches the peer this turn; got: {ended_note}"
        );
    }

    #[tokio::test]
    async fn test_send_to_other_agent_after_dm_ended_is_not_folded() {
        // The post-end turn keeps `send_message` for everyone else — the
        // fold removes exactly one recipient, not the tool. Here the send
        // proceeds far enough to hit the missing-store error.
        let tool = make_tool().with_ended_dm_peer(Some("alice".into()));
        let result = tool
            .execute(serde_json::json!({ "to": "charlie", "message": "alice and I are done" }))
            .await;
        assert!(
            result.is_err(),
            "a send to a THIRD agent must proceed to resolution/delivery — \
             the post-end turn keeps every capability but the one target"
        );
    }

    // ---- Delivered-result shape (#1111) ----
    //
    // The wording of the note is prose and deliberately not asserted; what
    // is pinnable is that a successful delivery still *carries* one. The
    // note is the only place the agent is told, at planning time, not to
    // poll `read_messages` -- a refactor that dropped the field would
    // silently take that back, and every other test in this file stops
    // short of the delivered path (no store, erroring sender).

    /// A sender that always succeeds, so the delivered branch is reachable.
    #[derive(Debug)]
    struct OkSender(SessionId);

    #[async_trait::async_trait]
    impl MessageSender for OkSender {
        async fn send(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            Ok(crate::message_sender::DeliveryReceipt { session_id: self.0 })
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn delivered_result_carries_a_planning_note() {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        let recipient = alms_core::AgentRecord::for_test("bob");
        store.create_agent(&recipient).unwrap();
        let mgr = Arc::new(
            SessionManager::with_store(alms_session::SessionConfig::default(), store).unwrap(),
        );

        let dm_session = SessionId::new();
        let tool = SendMessageTool::new(
            Arc::new(OkSender(dm_session)),
            AgentId::new(),
            "alice".into(),
            mgr,
            SessionId::new(),
        );

        let result = tool
            .execute(serde_json::json!({ "to": "bob", "message": "hi" }))
            .await
            .expect("delivery must succeed");

        assert_eq!(result["delivered"], true);
        assert_eq!(result["dm_session_id"], dm_session.0.to_string());
        assert_eq!(
            result["note"], DELIVERED_NOTE,
            "a delivered send must still carry the planning note (#1111)"
        );
        assert!(!DELIVERED_NOTE.is_empty());
    }

    #[tokio::test]
    async fn test_no_dm_peer_means_no_fold() {
        let tool = make_tool();
        let result = tool
            .execute(serde_json::json!({ "to": "alice", "message": "hi" }))
            .await;
        assert!(
            result.is_err(),
            "without a DM peer, sends must proceed normally (missing store here)"
        );
    }

    /// A sender that records the recipient name it was handed.
    #[derive(Debug, Default)]
    struct RecordingSender(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl MessageSender for RecordingSender {
        async fn send(
            &self,
            _: &str,
            _: AgentId,
            recipient_name: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            self.0.lock().unwrap().push(recipient_name.to_string());
            Ok(crate::message_sender::DeliveryReceipt {
                session_id: SessionId::new(),
            })
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Ok(())
        }
    }

    /// A sender that records **both** name legs of the DM `context_id`.
    #[derive(Debug, Default)]
    struct RecordingBothLegs(std::sync::Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl MessageSender for RecordingBothLegs {
        async fn send(
            &self,
            sender_name: &str,
            _: AgentId,
            recipient_name: &str,
            _: AgentId,
            _: &str,
            _: Option<SessionId>,
        ) -> Result<crate::message_sender::DeliveryReceipt, SendError> {
            self.0
                .lock()
                .unwrap()
                .push((sender_name.to_string(), recipient_name.to_string()));
            Ok(crate::message_sender::DeliveryReceipt {
                session_id: SessionId::new(),
            })
        }

        async fn end_conversation(
            &self,
            _: &str,
            _: AgentId,
            _: &str,
            _: AgentId,
            _: crate::message_sender::ConversationEndReason,
        ) -> Result<(), SendError> {
            Ok(())
        }
    }

    fn agent_record(name: &str) -> alms_core::AgentRecord {
        alms_core::AgentRecord::for_test(name)
    }

    /// #2: delivery uses the recipient's canonical registry name, not the
    /// `to` string the model typed.
    ///
    /// `MessageBus::send` derives the DM `context_id` from this argument. Now
    /// that names admit uppercase and resolve case-insensitively, passing the
    /// raw `to` would file `dm:Atlas:alice` and `dm:atlas:alice` as two
    /// separate DM sessions between the same pair of agents — the peer's
    /// history would silently split on the sender's spelling habits.
    #[tokio::test]
    async fn delivery_uses_the_recipients_canonical_registry_name() {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        store.create_agent(&agent_record("Atlas")).unwrap();
        let mgr = Arc::new(
            SessionManager::with_store(alms_session::SessionConfig::default(), store).unwrap(),
        );

        let sender = Arc::new(RecordingSender::default());
        let tool = SendMessageTool::new(
            sender.clone(),
            AgentId::new(),
            "alice".into(),
            mgr,
            SessionId::new(),
        );

        for spelling in ["Atlas", "atlas", "ATLAS"] {
            let result = tool
                .execute(serde_json::json!({ "to": spelling, "message": "hi" }))
                .await
                .unwrap_or_else(|e| panic!("send to {spelling} must resolve: {e}"));
            assert_eq!(result["delivered"], true);
        }

        assert_eq!(
            *sender.0.lock().unwrap(),
            vec![
                "Atlas".to_string(),
                "Atlas".to_string(),
                "Atlas".to_string()
            ],
            "every spelling must deliver under the one canonical name"
        );
    }

    /// #2: **both** legs of the DM `context_id` are canonical, so the pair
    /// resolves to one session.
    ///
    /// The sibling test above pins the recipient leg, which is the one that
    /// had the bug. The sender leg (`self.sender_name`) is correct only
    /// because the runtime constructs this tool from `resolved.agent_name` —
    /// a registry lookup — and nothing closer than a doc line said so. This
    /// pins the property the two legs jointly produce: whatever spelling the
    /// model types for `to`, the `dm_context_id` built from the recorded pair
    /// is the canonical one, identical every time.
    ///
    /// **Scope, so the gap is not mistaken for closed:** this pins
    /// *composition*, not *provenance*. The sender name is constructed here by
    /// hand, so what it proves is "given a canonical sender leg, every `to`
    /// spelling yields one context". What makes that leg canonical in
    /// production — `resolved.agent_name` flowing into `SendMessageTool::new`
    /// in the gateway's run lifecycle — is a wiring fact this crate cannot
    /// reach, and is still held by a doc line rather than a test.
    #[tokio::test]
    async fn dm_context_id_is_canonical_on_both_legs() {
        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        store.create_agent(&agent_record("Atlas")).unwrap();
        let mgr = Arc::new(
            SessionManager::with_store(alms_session::SessionConfig::default(), store).unwrap(),
        );

        let sender = Arc::new(RecordingBothLegs::default());
        // The sender name the runtime hands us is already registry-canonical;
        // a mixed-case one proves the tool passes it through untouched rather
        // than folding or re-deriving it.
        let tool = SendMessageTool::new(
            sender.clone(),
            AgentId::new(),
            "Alice".into(),
            mgr,
            SessionId::new(),
        );

        for spelling in ["Atlas", "atlas", "ATLAS", "aTlAs"] {
            tool.execute(serde_json::json!({ "to": spelling, "message": "hi" }))
                .await
                .unwrap_or_else(|e| panic!("send to {spelling} must resolve: {e}"));
        }

        let contexts: Vec<String> = sender
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(from, to)| alms_core::dm_context_id(from, to))
            .collect();

        assert_eq!(
            contexts,
            vec!["dm:Alice:Atlas".to_string(); 4],
            "every spelling must land on one DM context, not fork the pair"
        );
    }
}
