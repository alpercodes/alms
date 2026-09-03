// SPDX-License-Identifier: Apache-2.0

//! Shared test helpers for the sqlite submodule tests.
//!
//! Eliminates duplication of `new_session()` and `new_message()` across
//! sessions.rs, messages.rs, runs.rs, and audit.rs test modules.

use super::SqliteStore;
use crate::types::{Content, Message, Role, Session};
use alms_core::{AgentId, Timestamp};

/// Execute raw SQL against the store's connection.
///
/// Used to plant deliberately corrupt rows that no public write path would
/// ever produce, so the quarantine counters (#1241) can be tested against the
/// failure they exist for.
pub(super) fn corrupt_with_sql(store: &SqliteStore, sql: &str) {
    store
        .conn
        .lock()
        .execute_batch(sql)
        .expect("raw corruption SQL should apply");
}

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
