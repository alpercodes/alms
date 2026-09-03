// SPDX-License-Identifier: Apache-2.0

//! Lifecycle marker persistence — a single helper that all lifecycle SSE
//! events go through when they need to persist a marker message to the
//! session history.
//!
//! This replaces the ad-hoc pattern where each lifecycle event (job
//! completion, DM ended, subagent completion) had its own marker-writing
//! code.  The centralised helper ensures every marker uses the same
//! `Role::System` + `synthetic: true` metadata shape, which
//! `is_synthetic_marker` in `alms-tools` can filter on with a single
//! field check.
//!
//! # Markers are usually NOT part of the LLM context
//!
//! Markers exist for UI rendering and persistence only. The canonical
//! message-shape invariant (see `alms-runtime::context` module docs, and
//! `ContextBuilder::strip_mid_history_system_markers`) strips every
//! `Role::System` message that appears after the first non-system entry
//! before the message list reaches the provider adapter — so any marker
//! written here will surface via SSE/UI but will never be fed to the LLM.
//! The agent still receives the relevant payload via the
//! `notification_input` user message that `lifecycle.rs` pre-persists
//! alongside the marker.
//!
//! See issue #627 and Tim's architectural audit (#613).
//!
//! # Exception: error markers (issue #874)
//!
//! Markers tagged with `metadata.kind = "error"` are an explicit
//! exception to the rule above. They surface mid-run failures (LLM 4xx/5xx,
//! cancellation, runtime construction error) into the agent's context so
//! a follow-up turn like "why did that fail?" gives the LLM the error
//! text without re-quoting. The runtime's `session_msg_to_llm` rewrites
//! these markers into a `user` message with an `[Error] ...` prefix so
//! they survive `strip_mid_history_system_markers`.

use alms_core::SessionId;
use alms_session::SessionManager;
use tracing::warn;

/// Persist a lifecycle marker message to a session's history.
///
/// The marker is always `Role::System` with `Content::Text(display_text)`
/// and metadata that includes:
///
/// - `"synthetic": true` — the canonical flag that `is_synthetic_marker`
///   checks to exclude these from DM conversation output.
/// - `"type": marker_type` — identifies the specific lifecycle event so
///   the frontend can route it to the correct visual component.
/// - All entries from `extra_metadata` — event-specific fields (e.g.
///   `peer`, `reason`, `status`, `subagent_name`, `job_status`).
///
/// # Arguments
///
/// - `session_manager` — session manager for appending the message.
/// - `session_id` — target session.
/// - `marker_type` — a short identifier (e.g. `"job_notification"`,
///   `"dm_ended_notification"`, `"subagent_completion"`).
/// - `display_text` — human-readable text for the marker body.
/// - `extra_metadata` — additional key-value pairs merged into the
///   metadata object.  Must be a JSON object (non-objects are ignored
///   with a warning).
pub fn persist_lifecycle_marker(
    session_manager: &SessionManager,
    session_id: SessionId,
    marker_type: &str,
    display_text: String,
    extra_metadata: serde_json::Value,
) {
    // Build the canonical metadata: synthetic + type + extra fields.
    let mut meta = serde_json::json!({
        "synthetic": true,
        "type": marker_type,
    });

    // Merge extra_metadata into the canonical fields.
    if let serde_json::Value::Object(extras) = extra_metadata {
        if let serde_json::Value::Object(ref mut base) = meta {
            base.extend(extras);
        }
    } else if !extra_metadata.is_null() {
        warn!(
            marker_type = %marker_type,
            "persist_lifecycle_marker: extra_metadata is not a JSON object — ignoring"
        );
    }

    let marker = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::System,
        content: alms_session::Content::Text(display_text),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(meta),
    };

    if let Err(e) = session_manager.append_message(session_id, marker) {
        warn!(
            marker_type = %marker_type,
            session_id = %session_id.0,
            "Failed to persist lifecycle marker: {e}"
        );
    }
}

/// Persist an error marker to a session's history (issue #874).
///
/// Unlike most lifecycle markers, error markers are intentionally surfaced
/// to the LLM on subsequent turns so the agent can answer follow-up
/// questions like "why did that fail?" without the user re-quoting the
/// error. The runtime's `session_msg_to_llm` detects
/// `metadata.kind == "error"` and rewrites the marker into a `user`
/// message with an `[Error] ...` prefix before LLM submission, bypassing
/// the usual `strip_mid_history_system_markers` pass.
///
/// Persistence shape: `Role::System` + `Content::Text(display_text)` +
/// metadata that includes:
///
/// - `"synthetic": true` — keeps the marker out of DM conversation output.
/// - `"kind": "error"` — flags this marker for context-builder rewriting.
/// - `"type": marker_type` — identifies the specific error category
///   (e.g. `"run_boundary"`, `"runtime_init_error"`).
/// - All entries from `extra_metadata` — event-specific fields (e.g.
///   `error`, `run_id`, `tool`, `error_kind`).
///
/// # Arguments
///
/// - `session_manager` — session manager for appending the message.
/// - `session_id` — target session.
/// - `marker_type` — short identifier for the error category (mirrors the
///   `type` field of `persist_lifecycle_marker`).
/// - `display_text` — human-readable text for the marker body. The
///   context builder will prepend `[Error]` before this text when
///   building the LLM context, so callers should not duplicate that.
/// - `extra_metadata` — additional key-value pairs merged into the
///   metadata object.  Must be a JSON object (non-objects are ignored
///   with a warning).
pub fn persist_error_marker(
    session_manager: &SessionManager,
    session_id: SessionId,
    marker_type: &str,
    display_text: String,
    extra_metadata: serde_json::Value,
) {
    // Build the canonical metadata: synthetic + kind=error + type + extras.
    let mut meta = serde_json::json!({
        "synthetic": true,
        "kind": "error",
        "type": marker_type,
    });

    if let serde_json::Value::Object(extras) = extra_metadata {
        if let serde_json::Value::Object(ref mut base) = meta {
            base.extend(extras);
        }
    } else if !extra_metadata.is_null() {
        warn!(
            marker_type = %marker_type,
            "persist_error_marker: extra_metadata is not a JSON object — ignoring"
        );
    }

    let marker = alms_session::Message {
        id: uuid::Uuid::new_v4().to_string(),
        role: alms_session::Role::System,
        content: alms_session::Content::Text(display_text),
        timestamp: alms_core::Timestamp::now(),
        metadata: Some(meta),
    };

    if let Err(e) = session_manager.append_message(session_id, marker) {
        warn!(
            marker_type = %marker_type,
            session_id = %session_id.0,
            "Failed to persist error marker: {e}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::AgentId;
    use alms_session::{Content, SessionConfig, SessionManager};

    fn setup() -> (SessionManager, SessionId) {
        let mgr = SessionManager::new(SessionConfig::default());
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "test-session");
        (mgr, session.id)
    }

    #[test]
    fn marker_is_persisted_with_correct_shape() {
        let (mgr, session_id) = setup();

        persist_lifecycle_marker(
            &mgr,
            session_id,
            "job_notification",
            "[Scheduled job completed] test-job".to_string(),
            serde_json::json!({"job_status": "success"}),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert_eq!(history.len(), 1, "should have exactly one marker message");

        let msg = &history[0];
        assert_eq!(msg.role, alms_session::Role::System);
        if let Content::Text(ref text) = msg.content {
            assert!(text.contains("test-job"));
        } else {
            panic!("expected text content");
        }

        let meta = msg.metadata.as_ref().expect("should have metadata");
        assert_eq!(meta["synthetic"], true);
        assert_eq!(meta["type"], "job_notification");
        assert_eq!(meta["job_status"], "success");
    }

    #[test]
    fn marker_with_empty_extra_metadata() {
        let (mgr, session_id) = setup();

        persist_lifecycle_marker(
            &mgr,
            session_id,
            "test_marker",
            "Test display text".to_string(),
            serde_json::json!({}),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert_eq!(history.len(), 1);

        let meta = history[0].metadata.as_ref().unwrap();
        assert_eq!(meta["synthetic"], true);
        assert_eq!(meta["type"], "test_marker");
    }

    #[test]
    fn marker_with_null_extra_metadata() {
        let (mgr, session_id) = setup();

        persist_lifecycle_marker(
            &mgr,
            session_id,
            "test_marker",
            "Test".to_string(),
            serde_json::Value::Null,
        );

        let history = mgr.get_history(session_id).unwrap();
        assert_eq!(history.len(), 1);

        let meta = history[0].metadata.as_ref().unwrap();
        assert_eq!(meta["synthetic"], true);
        assert_eq!(meta["type"], "test_marker");
    }

    #[test]
    fn multiple_extra_fields_are_merged() {
        let (mgr, session_id) = setup();

        persist_lifecycle_marker(
            &mgr,
            session_id,
            "dm_ended_notification",
            "DM ended".to_string(),
            serde_json::json!({
                "peer": "bob",
                "reason": "ignored",
                "context_id": "dm:alice:bob",
            }),
        );

        let history = mgr.get_history(session_id).unwrap();
        let meta = history[0].metadata.as_ref().unwrap();
        assert_eq!(meta["synthetic"], true);
        assert_eq!(meta["type"], "dm_ended_notification");
        assert_eq!(meta["peer"], "bob");
        assert_eq!(meta["reason"], "ignored");
        assert_eq!(meta["context_id"], "dm:alice:bob");
    }

    #[test]
    fn marker_filtered_by_is_synthetic_marker() {
        let (mgr, session_id) = setup();

        persist_lifecycle_marker(
            &mgr,
            session_id,
            "subagent_completion",
            "Subagent 'researcher' completed.".to_string(),
            serde_json::json!({"subagent_name": "researcher", "status": "done"}),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert!(
            alms_tools::dm_filter::is_synthetic_marker(&history[0]),
            "lifecycle markers should be filtered by is_synthetic_marker"
        );
    }

    // -----------------------------------------------------------------------
    // persist_error_marker tests (issue #874)
    // -----------------------------------------------------------------------

    #[test]
    fn error_marker_carries_kind_error() {
        let (mgr, session_id) = setup();

        persist_error_marker(
            &mgr,
            session_id,
            "run_boundary",
            "(run failed) connection refused".to_string(),
            serde_json::json!({
                "run_id": "00000000-0000-0000-0000-000000000001",
                "status": "failed",
                "error": "connection refused",
            }),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert_eq!(history.len(), 1);

        let msg = &history[0];
        assert_eq!(msg.role, alms_session::Role::System);
        let meta = msg.metadata.as_ref().unwrap();
        assert_eq!(meta["synthetic"], true);
        assert_eq!(meta["kind"], "error");
        assert_eq!(meta["type"], "run_boundary");
        assert_eq!(meta["status"], "failed");
        assert_eq!(meta["error"], "connection refused");
    }

    #[test]
    fn error_marker_with_empty_extras_still_has_kind() {
        let (mgr, session_id) = setup();

        persist_error_marker(
            &mgr,
            session_id,
            "runtime_init_error",
            "(runtime initialization failed) bad config".to_string(),
            serde_json::json!({}),
        );

        let history = mgr.get_history(session_id).unwrap();
        let meta = history[0].metadata.as_ref().unwrap();
        assert_eq!(meta["kind"], "error");
        assert_eq!(meta["type"], "runtime_init_error");
        assert_eq!(meta["synthetic"], true);
    }

    #[test]
    fn error_marker_filtered_by_is_synthetic_marker() {
        let (mgr, session_id) = setup();

        persist_error_marker(
            &mgr,
            session_id,
            "run_boundary",
            "(run cancelled)".to_string(),
            serde_json::json!({"status": "cancelled"}),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert!(
            alms_tools::dm_filter::is_synthetic_marker(&history[0]),
            "error markers should still be filtered by is_synthetic_marker (DM hygiene)"
        );
    }

    /// Regression test for issue #911 — when the lifecycle layer pre-sanitises
    /// the error text via `alms_core::sanitize_error_for_session` before
    /// calling `persist_error_marker`, neither the marker `display_text` nor
    /// the metadata `error` field contain the raw secret. This locks in the
    /// invariant that the sanitiser hop runs before persistence so a follow-up
    /// LLM turn cannot echo a leaked API key from the agent's own context.
    #[test]
    fn sanitised_error_marker_contains_no_secret() {
        use alms_core::{AlmsError, sanitize_error_for_session};

        let (mgr, session_id) = setup();

        // Simulate what the lifecycle layer does: take a raw provider error
        // that includes an API-key-like token, run it through the sanitiser,
        // then persist the sanitised form.
        let raw_err = AlmsError::Runtime(
            "401 Unauthorized: authorization=Bearer sk-test-12345 from https://api.example.com"
                .into(),
        );
        let safe_reason = sanitize_error_for_session(&raw_err);

        persist_error_marker(
            &mgr,
            session_id,
            "run_boundary",
            format!("(run failed) {safe_reason}"),
            serde_json::json!({
                "run_id": "00000000-0000-0000-0000-000000000001",
                "status": "failed",
                "error": safe_reason,
                "error_kind": "failed",
            }),
        );

        let history = mgr.get_history(session_id).unwrap();
        assert_eq!(history.len(), 1);

        let msg = &history[0];
        let display_text = match &msg.content {
            Content::Text(t) => t.clone(),
            _ => panic!("expected text content"),
        };
        let meta = msg.metadata.as_ref().unwrap();
        let error_field = meta["error"].as_str().unwrap_or("");

        // The fake key, hostname, and URL must NOT appear anywhere persisted —
        // this is the regression Tim flagged in the #906 review.
        for needle in [
            "sk-test-12345",
            "api.example.com",
            "https://",
            "Bearer",
            "authorization",
        ] {
            assert!(
                !display_text.contains(needle),
                "marker display_text should not contain {needle:?}, got {display_text:?}"
            );
            assert!(
                !error_field.contains(needle),
                "marker metadata.error should not contain {needle:?}, got {error_field:?}"
            );
        }

        // The sanitised category label still reaches the marker so the agent
        // sees a useful follow-up signal.
        assert!(
            display_text.contains("LLM authentication error"),
            "sanitised category should appear in display_text, got {display_text:?}"
        );
        assert_eq!(error_field, "LLM authentication error");
    }
}
