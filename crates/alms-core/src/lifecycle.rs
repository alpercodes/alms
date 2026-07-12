//! Shared lifecycle transition outcomes.

use serde::{Deserialize, Serialize};

/// Largest lifecycle revision that can be represented by SQLite INTEGER.
pub const MAX_LIFECYCLE_REVISION: u64 = i64::MAX as u64;

/// Result of asking a lifecycle state machine to apply a transition.
///
/// Only [`Applied`](Self::Applied) advances the revision. Duplicate terminal
/// requests are reported as [`NoOp`](Self::NoOp); illegal transitions are
/// [`Rejected`](Self::Rejected). This lets callers gate external side effects
/// (SSE, notifications, scheduling) on the same authoritative decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionOutcome<S> {
    Applied { from: S, to: S, revision: u64 },
    NoOp { state: S, revision: u64 },
    Rejected { from: S, to: S, revision: u64 },
}

impl<S> TransitionOutcome<S> {
    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}
