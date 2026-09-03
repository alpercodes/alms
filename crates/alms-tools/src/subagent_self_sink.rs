// SPDX-License-Identifier: Apache-2.0

//! `SubagentSelfEventSink` (#1180) — mints a session-bound [`EventForwarder`]
//! that writes a subagent's own run events to its own session's SSE log.
//!
//! Lives in `alms-tools` so the coordinator can build a per-subagent forwarder
//! without depending on `alms-gateway`, which supplies the implementation.

use crate::event_forwarder::EventForwarder;
use alms_core::{RunId, SessionId};
use std::sync::Arc;

/// Factory for a subagent's own-session event forwarder (#1180).
///
/// `forwarder_for` returns an [`EventForwarder`] whose events land on the
/// subagent's OWN session log, so `GET /sessions/{id}/events` streams live and
/// replays (non-null `last_event_id`) — backing the fullscreen subagent view.
///
/// Events forwarded here MUST be untagged (`source_agent = None`): on a
/// session's own stream the frontend renders untagged events as that session's
/// main agent. (The tagged copy for the parent's SubagentBar is a separate
/// forward — the coordinator's subagent→parent relay.)
pub trait SubagentSelfEventSink: Send + Sync + std::fmt::Debug {
    /// Build an [`EventForwarder`] bound to a subagent's own `(session_id, run_id)`.
    fn forwarder_for(&self, session_id: SessionId, run_id: RunId) -> Arc<dyn EventForwarder>;
}
