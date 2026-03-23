//! Shared test helpers for the sqlite submodule tests.
//!
//! Eliminates duplication of `new_session()` and `new_message()` across
//! sessions.rs, messages.rs, runs.rs, and audit.rs test modules.

use crate::types::{Content, Message, Role, Session};
use alms_core::{AgentId, Timestamp};

pub(super) fn new_session() -> Session {
    Session::new(AgentId::new(), "test-ctx")
}

pub(super) fn new_message(text: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: Role::User,
        content: Content::Text(text.to_string()),
        timestamp: Timestamp::now(),
        metadata: None,
    }
}
