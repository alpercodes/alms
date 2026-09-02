// SPDX-License-Identifier: Apache-2.0

//! Timeline API — unified chronological view of all agent activity.
//!
//! `GET /agents/{id_or_name}/timeline?limit=N&before=TIMESTAMP`
//!
//! Aggregates runs, tool calls, and significant messages across all sessions
//! for an agent into a single reverse-chronological event stream.

use crate::agents::{get_store, resolve_agent};
use crate::api_error;
use crate::server::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use tracing::instrument;

/// Maximum events per request.
const MAX_LIMIT: usize = 200;
/// Default events per request.
const DEFAULT_LIMIT: usize = 50;

/// Query parameters for the timeline endpoint.
#[derive(Debug, Deserialize)]
pub struct TimelineQuery {
    /// Maximum number of events to return (default 50, max 200).
    pub limit: Option<usize>,
    /// Cursor for pagination — only return events before this RFC3339 timestamp.
    pub before: Option<String>,
}

/// `GET /agents/{id_or_name}/timeline` — unified agent activity timeline.
///
/// Returns a reverse-chronological list of events across all sessions for the
/// agent, interleaving runs, tool calls, and significant messages.
///
/// Supports cursor-based pagination via `before` (RFC3339 timestamp) and
/// `limit` (default 50, max 200).  The client should use the `timestamp` of
/// the last event in the response as the `before` cursor for the next page.
#[instrument(level = "info", skip(state, params))]
pub async fn get_agent_timeline(
    State(state): State<AppState>,
    Path(id_or_name): Path<String>,
    Query(params): Query<TimelineQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let store = get_store(&state)?;
    let agent = resolve_agent(store, &id_or_name)?;

    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);

    let page = store
        .load_timeline_events(agent.id, params.before.as_deref(), limit)
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TIMELINE_ERROR",
                format!("Failed to load timeline: {e}"),
            )
        })?;

    // Build pagination cursor from the last event's timestamp.
    let next_before = page.events.last().map(|e| e.timestamp.clone());

    Ok(Json(serde_json::json!({
        "agent_id": agent.id.0.to_string(),
        "agent_name": agent.name,
        "events": page.events,
        "pagination": {
            "limit": limit,
            "has_more": page.has_more,
            "next_before": next_before,
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limit_clamping() {
        // Over max
        assert_eq!(300_usize.min(MAX_LIMIT), MAX_LIMIT);
        // Under max
        assert_eq!(10_usize.min(MAX_LIMIT), 10);
        // Default
        assert_eq!(DEFAULT_LIMIT, 50);
    }
}
