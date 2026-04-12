//! Timeline aggregation queries -- unified chronological event stream
//! across all sessions for an agent.
//!
//! Used by the `GET /agents/{id}/timeline` endpoint to provide a single
//! view of all agent activity: runs, tool calls, and significant messages.

use super::*;

/// A single event in the agent timeline.
///
/// Represents one meaningful state change across any session the agent
/// participates in.  Produced by [`SqliteStore::load_timeline_events`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineEvent {
    /// When this event occurred (RFC3339).
    pub timestamp: String,
    /// Event classification.
    pub event_type: String,
    /// Session this event belongs to.
    pub session_id: String,
    /// Human-readable session type (chat, dm, subagent, job, notification, telegram, episodic).
    pub session_type: String,
    /// The session's context_id (for display and navigation).
    pub context_id: String,
    /// Associated run ID, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Short human-readable description.
    pub summary: String,
    /// Type-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl SqliteStore {
    /// Load a unified, chronological timeline of events for an agent.
    ///
    /// Aggregates three data sources in a single SQL query via UNION ALL:
    ///
    /// 1. **Runs** -- `run_started` and `run_completed`/`run_failed`/`run_cancelled`
    /// 2. **Tool calls** -- assistant-side tool invocations (role = 'assistant')
    /// 3. **Messages** -- user messages and synthetic system markers
    ///
    /// Events are sorted descending by timestamp and filtered by:
    /// - `agent_id` -- only sessions belonging to this agent
    /// - `before` -- cursor-based pagination (only events before this timestamp)
    /// - `limit` -- maximum number of events to return
    ///
    /// Returns events in **reverse chronological** order (newest first).
    pub fn load_timeline_events(
        &self,
        agent_id: AgentId,
        before: Option<&str>,
        limit: usize,
    ) -> AlmsResult<Vec<TimelineEvent>> {
        let agent_id_str = agent_id.0.to_string();
        // Use a far-future timestamp when no cursor is provided.
        let before_ts = before.unwrap_or("9999-12-31T23:59:59Z");
        let limit_i64 = limit as i64;

        let conn = self.conn.lock();

        // Single query using UNION ALL across the three data sources.
        //
        // Each branch selects the same shape:
        //   timestamp, event_type, session_id, context_id, run_id, summary, metadata
        //
        // The outer query sorts and limits.
        let sql = "
            SELECT timestamp, event_type, session_id, context_id, run_id, summary, metadata
            FROM (
                -- Branch 1a: run_started events (runs with started_at)
                SELECT
                    r.started_at AS timestamp,
                    'run_started' AS event_type,
                    r.session_id,
                    s.context_id,
                    r.run_id,
                    'Started run' AS summary,
                    json_object(
                        'status', r.status,
                        'input', SUBSTR(r.input, 1, 120)
                    ) AS metadata
                FROM runs r
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND r.started_at IS NOT NULL
                  AND r.started_at < ?2

                UNION ALL

                -- Branch 1b: run terminal events (completed/failed/cancelled)
                SELECT
                    r.ended_at AS timestamp,
                    CASE r.status
                        WHEN 'completed' THEN 'run_completed'
                        WHEN 'failed' THEN 'run_failed'
                        WHEN 'cancelled' THEN 'run_cancelled'
                        ELSE 'run_ended'
                    END AS event_type,
                    r.session_id,
                    s.context_id,
                    r.run_id,
                    CASE r.status
                        WHEN 'completed' THEN 'Completed run'
                            || CASE WHEN r.prompt_tokens IS NOT NULL
                                THEN ' (' || (r.prompt_tokens + r.completion_tokens) || ' tokens)'
                                ELSE '' END
                        WHEN 'failed' THEN 'Run failed'
                            || CASE WHEN r.error IS NOT NULL
                                THEN ': ' || SUBSTR(r.error, 1, 80)
                                ELSE '' END
                        WHEN 'cancelled' THEN 'Run cancelled'
                        ELSE 'Run ended'
                    END AS summary,
                    json_object(
                        'status', r.status,
                        'prompt_tokens', r.prompt_tokens,
                        'completion_tokens', r.completion_tokens,
                        'error', r.error,
                        'job_id', r.job_id,
                        'parent_run_id', r.parent_run_id
                    ) AS metadata
                FROM runs r
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND r.ended_at IS NOT NULL
                  AND r.status IN ('completed', 'failed', 'cancelled')
                  AND r.ended_at < ?2

                UNION ALL

                -- Branch 2: tool calls (assistant-side only, to avoid duplicating result rows)
                SELECT
                    tc.timestamp,
                    'tool_call' AS event_type,
                    r.session_id,
                    s.context_id,
                    tc.run_id,
                    'Called ' || COALESCE(tc.tool_name, 'unknown tool') AS summary,
                    json_object(
                        'tool_name', tc.tool_name,
                        'tool_id', tc.tool_id
                    ) AS metadata
                FROM run_tool_calls tc
                JOIN runs r ON tc.run_id = r.run_id
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND tc.role = 'assistant'
                  AND tc.timestamp < ?2

                UNION ALL

                -- Branch 3: user messages (role='user') and synthetic system markers
                SELECT
                    m.timestamp,
                    CASE
                        WHEN m.role = 'user' THEN 'message_received'
                        WHEN m.role = 'system' THEN 'marker'
                        ELSE 'message_sent'
                    END AS event_type,
                    m.session_id,
                    s.context_id,
                    NULL AS run_id,
                    CASE
                        WHEN m.role = 'user' THEN 'User message'
                        WHEN m.role = 'system' THEN 'System event'
                        ELSE 'Agent response'
                    END AS summary,
                    NULL AS metadata
                FROM messages m
                JOIN sessions s ON m.session_id = s.id
                WHERE s.agent_id = ?1
                  AND m.role IN ('user', 'system')
                  AND (m.role = 'user'
                       OR (m.metadata IS NOT NULL AND json_extract(m.metadata, '$.synthetic') = 1))
                  AND m.timestamp < ?2
            )
            ORDER BY timestamp DESC
            LIMIT ?3
        ";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare timeline: {e}")))?;

        let rows = stmt
            .query_map(params![agent_id_str, before_ts, limit_i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| AlmsError::Runtime(format!("SQLite query timeline: {e}")))?
            .filter_map(|r| match r {
                Ok((
                    timestamp,
                    event_type,
                    session_id,
                    context_id,
                    run_id,
                    summary,
                    metadata_str,
                )) => {
                    let session_type = alms_core::classify_session_type(&context_id).to_string();
                    let metadata = metadata_str.and_then(|s| serde_json::from_str(&s).ok());
                    Some(TimelineEvent {
                        timestamp,
                        event_type,
                        session_id,
                        session_type,
                        context_id,
                        run_id,
                        summary,
                        metadata,
                    })
                }
                Err(e) => {
                    tracing::warn!("Skipping unparseable timeline row: {e}");
                    None
                }
            })
            .collect();

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Session;
    use alms_core::run::{Run, TokenUsage, ToolCallRecord, ToolCallRole};

    fn setup_store_with_agent() -> (SqliteStore, AgentId, SessionId) {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        let session = Session::new(agent_id, "web-chat");
        store.save_session(&session).unwrap();
        (store, agent_id, session.id)
    }

    #[test]
    fn test_timeline_empty_agent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_timeline_includes_runs() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        let mut run = Run::new(session_id, agent_id, "hello".to_string());
        run.mark_running();
        run.mark_completed(
            "world".to_string(),
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
            },
        );
        store.save_run(&run).unwrap();

        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        // Should have at least run_started and run_completed
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(
            types.contains(&"run_started"),
            "should contain run_started, got: {types:?}"
        );
        assert!(
            types.contains(&"run_completed"),
            "should contain run_completed, got: {types:?}"
        );
    }

    #[test]
    fn test_timeline_includes_tool_calls() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        let mut run = Run::new(session_id, agent_id, "test".to_string());
        run.mark_running();
        store.save_run(&run).unwrap();

        let tc = ToolCallRecord {
            seq: 0,
            role: ToolCallRole::Assistant,
            tool_name: Some("shell_exec".to_string()),
            tool_id: Some("call_0".to_string()),
            params: Some(r#"{"cmd":"ls"}"#.to_string()),
            result: None,
            timestamp: chrono::Utc::now(),
        };
        store.save_tool_call(run.run_id, &tc).unwrap();

        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        let tool_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "tool_call")
            .collect();
        assert_eq!(tool_events.len(), 1);
        assert!(tool_events[0].summary.contains("shell_exec"));
    }

    #[test]
    fn test_timeline_respects_limit() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Create 5 completed runs to generate 10 events (5 started + 5 completed).
        for _ in 0..5 {
            let mut run = Run::new(session_id, agent_id, "test".to_string());
            run.mark_running();
            run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                },
            );
            store.save_run(&run).unwrap();
        }

        let events = store.load_timeline_events(agent_id, None, 3).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_timeline_cursor_pagination() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Create 3 completed runs.
        for _ in 0..3 {
            let mut run = Run::new(session_id, agent_id, "test".to_string());
            run.mark_running();
            run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                },
            );
            store.save_run(&run).unwrap();
        }

        // Fetch all events first.
        let all_events = store.load_timeline_events(agent_id, None, 100).unwrap();
        assert!(!all_events.is_empty());

        // Use the last event's timestamp as cursor.
        let cursor = all_events.last().unwrap().timestamp.as_str();
        let paged = store
            .load_timeline_events(agent_id, Some(cursor), 100)
            .unwrap();
        // Should have fewer events than the full set (cursor is exclusive).
        assert!(
            paged.len() < all_events.len(),
            "cursor pagination should return fewer events: paged={} all={}",
            paged.len(),
            all_events.len()
        );
    }

    #[test]
    fn test_timeline_session_type_classification() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        // Create sessions of different types.
        let chat_session = Session::new(agent_id, "web-chat");
        let job_session = Session::new(agent_id, "job_abc123");
        store.save_session(&chat_session).unwrap();
        store.save_session(&job_session).unwrap();

        // Create a run in each.
        let mut r1 = Run::new(chat_session.id, agent_id, "hello".to_string());
        r1.mark_running();
        store.save_run(&r1).unwrap();

        let mut r2 = Run::new(job_session.id, agent_id, "job run".to_string());
        r2.mark_running();
        store.save_run(&r2).unwrap();

        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        let types: Vec<(&str, &str)> = events
            .iter()
            .map(|e| (e.session_type.as_str(), e.event_type.as_str()))
            .collect();
        assert!(
            types.iter().any(|(st, _)| *st == "chat"),
            "should have chat session type"
        );
        assert!(
            types.iter().any(|(st, _)| *st == "job"),
            "should have job session type"
        );
    }

    #[test]
    fn test_timeline_isolates_agents() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        let session_a = Session::new(agent_a, "ctx-a");
        let session_b = Session::new(agent_b, "ctx-b");
        store.save_session(&session_a).unwrap();
        store.save_session(&session_b).unwrap();

        let mut run_a = Run::new(session_a.id, agent_a, "a".to_string());
        run_a.mark_running();
        store.save_run(&run_a).unwrap();

        let mut run_b = Run::new(session_b.id, agent_b, "b".to_string());
        run_b.mark_running();
        store.save_run(&run_b).unwrap();

        let events_a = store.load_timeline_events(agent_a, None, 50).unwrap();
        let events_b = store.load_timeline_events(agent_b, None, 50).unwrap();

        // Each agent should only see their own events.
        assert!(
            events_a
                .iter()
                .all(|e| e.session_id == session_a.id.0.to_string())
        );
        assert!(
            events_b
                .iter()
                .all(|e| e.session_id == session_b.id.0.to_string())
        );
    }

    #[test]
    fn test_timeline_run_failed_event() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        let mut run = Run::new(session_id, agent_id, "will fail".to_string());
        run.mark_running();
        run.mark_failed("LLM timeout".to_string());
        store.save_run(&run).unwrap();

        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        let failed: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "run_failed")
            .collect();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].summary.contains("failed"));
    }

    #[test]
    fn test_timeline_reverse_chronological_order() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Create two runs -- events should be newest first.
        for _ in 0..2 {
            let mut run = Run::new(session_id, agent_id, "test".to_string());
            run.mark_running();
            run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                },
            );
            store.save_run(&run).unwrap();
        }

        let events = store.load_timeline_events(agent_id, None, 50).unwrap();
        for pair in events.windows(2) {
            assert!(
                pair[0].timestamp >= pair[1].timestamp,
                "events should be in reverse chronological order: {} >= {}",
                pair[0].timestamp,
                pair[1].timestamp
            );
        }
    }
}
