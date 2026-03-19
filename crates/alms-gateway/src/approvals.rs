//! Approval store and HTTP handlers.
//!
//! When the runtime runs in `Guarded` posture, it emits `ApprovalRequired`
//! events. The gateway stores them here and exposes:
//!   GET  /approvals          — list pending approvals
//!   POST /approvals/{id}     — approve or deny

use crate::api_error;
use crate::server::AppState;
use alms_core::RunId;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// A pending approval waiting for a user decision.
pub struct PendingApproval {
    pub approval_id: Uuid,
    pub run_id: RunId,
    pub tool: String,
    pub params: Value,
    pub requested_at: DateTime<Utc>,
    /// Send `true` to approve, `false` to deny.
    pub decision_tx: tokio::sync::oneshot::Sender<bool>,
}

/// Shared store for pending approvals.
#[derive(Clone, Default)]
pub struct ApprovalStore {
    pending: Arc<DashMap<Uuid, PendingApproval>>,
}

impl fmt::Debug for ApprovalStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalStore")
            .field("pending_count", &self.pending.len())
            .finish()
    }
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
        }
    }

    pub fn insert(&self, approval: PendingApproval) {
        self.pending.insert(approval.approval_id, approval);
    }

    /// Resolve an approval. Returns the approval info if found, `None` otherwise.
    pub fn resolve(&self, id: Uuid, approve: bool) -> Option<ApprovalInfo> {
        if let Some((_, approval)) = self.pending.remove(&id) {
            let info = ApprovalInfo {
                approval_id: approval.approval_id,
                run_id: approval.run_id,
                tool: approval.tool,
                params: approval.params,
                requested_at: approval.requested_at,
            };
            let _ = approval.decision_tx.send(approve);
            Some(info)
        } else {
            None
        }
    }

    /// Remove all pending approvals for a given run (cleanup on run end).
    pub fn clear_for_run(&self, run_id: RunId) {
        self.pending.retain(|_, v| v.run_id != run_id);
    }

    pub fn list_pending(&self) -> Vec<ApprovalInfo> {
        self.pending
            .iter()
            .map(|entry| ApprovalInfo {
                approval_id: entry.approval_id,
                run_id: entry.run_id,
                tool: entry.tool.clone(),
                params: entry.params.clone(),
                requested_at: entry.requested_at,
            })
            .collect()
    }
}

/// Public view of a pending approval (no decision channel).
#[derive(Debug, Serialize)]
pub struct ApprovalInfo {
    pub approval_id: Uuid,
    pub run_id: RunId,
    pub tool: String,
    pub params: Value,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveApprovalRequest {
    pub decision: String, // "approve" or "deny"
}

#[derive(Debug, Deserialize)]
pub struct ListApprovalsQuery {
    pub session_id: Option<alms_core::SessionId>,
}

/// GET /approvals?session_id=<uuid> — list pending approvals, optionally filtered by session
pub async fn list_approvals(
    State(state): State<AppState>,
    Query(query): Query<ListApprovalsQuery>,
) -> impl IntoResponse {
    let mut pending = state.approval_store.list_pending();

    // Filter by session_id if provided (look up session from the run)
    if let Some(session_id) = query.session_id {
        pending.retain(|a| {
            state
                .run_manager
                .get_run(a.run_id)
                .map(|r| r.session_id == session_id)
                .unwrap_or(false)
        });
    }

    Json(serde_json::json!({ "approvals": pending }))
}

/// POST /approvals/{approval_id} — approve or deny
pub async fn resolve_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    Json(req): Json<ResolveApprovalRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let approve = match req.decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                "decision must be 'approve' or 'deny'",
            ));
        }
    };

    if let Some(info) = state.approval_store.resolve(approval_id, approve) {
        // Emit approval_resolved SSE event — only if the run is still tracked.
        let decision_str = if approve { "approve" } else { "deny" };
        if let Some(run) = state.run_manager.get_run(info.run_id) {
            state
                .run_manager
                .send_event(
                    info.run_id,
                    run.session_id,
                    crate::sse::SseEventData::approval_resolved(
                        info.run_id,
                        &approval_id.to_string(),
                        decision_str,
                    ),
                )
                .await;
        } else {
            warn!(
                "resolve_approval: run {} not found, skipping approval_resolved SSE event",
                info.run_id.0
            );
        }
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        Err(api_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Approval not found or already resolved",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::RunId;

    #[test]
    fn test_insert_and_list() {
        let store = ApprovalStore::new();
        let id = Uuid::new_v4();
        let (tx, _rx) = tokio::sync::oneshot::channel();

        store.insert(PendingApproval {
            approval_id: id,
            run_id: RunId::new(),
            tool: "echo".to_string(),
            params: serde_json::json!({"text": "hi"}),
            requested_at: Utc::now(),
            decision_tx: tx,
        });

        let pending = store.list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].approval_id, id);
        assert_eq!(pending[0].tool, "echo");
    }

    #[test]
    fn test_resolve_approve() {
        let store = ApprovalStore::new();
        let id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();

        store.insert(PendingApproval {
            approval_id: id,
            run_id: RunId::new(),
            tool: "echo".to_string(),
            params: serde_json::json!({}),
            requested_at: Utc::now(),
            decision_tx: tx,
        });

        let info = store.resolve(id, true);
        assert!(info.is_some());
        assert_eq!(info.unwrap().tool, "echo");
        assert!(rx.blocking_recv().unwrap());
        assert!(store.list_pending().is_empty());
    }

    #[test]
    fn test_resolve_deny() {
        let store = ApprovalStore::new();
        let id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();

        store.insert(PendingApproval {
            approval_id: id,
            run_id: RunId::new(),
            tool: "echo".to_string(),
            params: serde_json::json!({}),
            requested_at: Utc::now(),
            decision_tx: tx,
        });

        assert!(store.resolve(id, false).is_some());
        assert!(!rx.blocking_recv().unwrap());
    }

    #[test]
    fn test_resolve_nonexistent() {
        let store = ApprovalStore::new();
        assert!(store.resolve(Uuid::new_v4(), true).is_none());
    }

    #[test]
    fn test_resolve_removes_from_pending() {
        let store = ApprovalStore::new();
        let id = Uuid::new_v4();
        let (tx, _rx) = tokio::sync::oneshot::channel();

        store.insert(PendingApproval {
            approval_id: id,
            run_id: RunId::new(),
            tool: "echo".to_string(),
            params: serde_json::json!({}),
            requested_at: Utc::now(),
            decision_tx: tx,
        });

        assert!(store.resolve(id, true).is_some());
        // Second resolve should fail — already removed
        assert!(store.resolve(id, true).is_none());
    }

    #[test]
    fn test_clear_for_run() {
        let store = ApprovalStore::new();
        let run_id = RunId::new();
        let other_run_id = RunId::new();

        for (rid, _) in [(run_id, "a"), (run_id, "b"), (other_run_id, "c")] {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            store.insert(PendingApproval {
                approval_id: Uuid::new_v4(),
                run_id: rid,
                tool: "echo".to_string(),
                params: serde_json::json!({}),
                requested_at: Utc::now(),
                decision_tx: tx,
            });
        }

        assert_eq!(store.list_pending().len(), 3);
        store.clear_for_run(run_id);
        let remaining = store.list_pending();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].run_id, other_run_id);
    }
}
