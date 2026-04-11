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
//! See issue #627 and Tim's architectural audit (#613).

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
}
