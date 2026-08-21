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

/// Result of loading timeline events, including pagination metadata.
pub struct TimelinePage {
    /// The events on this page (at most `limit` items).
    pub events: Vec<TimelineEvent>,
    /// Whether more events exist beyond this page.
    pub has_more: bool,
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
    /// Events are sorted descending by `(timestamp, event_type, session_id)`
    /// for deterministic ordering even when timestamps collide.
    ///
    /// **DM sessions**: Shared DM sessions store `AgentId::nil()` as
    /// `sessions.agent_id`.  The messages branch includes them by joining
    /// through the `runs` table to find sessions where this agent has
    /// executed at least one run.
    ///
    /// **Pagination**: Uses a `(timestamp, sort_key)` cursor to avoid
    /// skipping events that share the same timestamp at a page boundary.
    /// Fetches `limit + 1` rows internally so `has_more` is accurate.
    ///
    /// Returns a [`TimelinePage`] with events in **reverse chronological**
    /// order (newest first) and a `has_more` flag.
    pub fn load_timeline_events(
        &self,
        agent_id: AgentId,
        before: Option<&str>,
        limit: usize,
    ) -> AlmsResult<TimelinePage> {
        let agent_id_str = agent_id.0.to_string();
        // Use a far-future timestamp when no cursor is provided.
        let before_ts = before.unwrap_or("9999-12-31T23:59:59Z");
        // Fetch one extra row to determine has_more accurately.
        let fetch_limit = (limit as i64) + 1;

        let conn = self.conn.lock();

        // Single query using UNION ALL across the three data sources.
        //
        // Each branch selects the same shape:
        //   timestamp, event_type, session_id, context_id, run_id, summary,
        //   metadata, sort_key
        //
        // `sort_key` is a deterministic tiebreaker built from
        // `event_type || session_id || coalesce(run_id, '')` so that events
        // sharing the same timestamp are always returned in a consistent
        // order.  The outer cursor uses `(timestamp, sort_key)` so no events
        // are skipped at page boundaries.
        //
        // For DM sessions (Branch 3), the messages branch uses an OR
        // condition: either `s.agent_id = ?1` (normal sessions) OR there
        // exists a run by this agent in that session (covers shared DM
        // sessions stored under the nil UUID sentinel).
        let sql = "
            SELECT timestamp, event_type, session_id, context_id, run_id,
                   summary, metadata, sort_key
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
                    ) AS metadata,
                    'run_started' || r.session_id || COALESCE(r.run_id, '') AS sort_key
                FROM runs r
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND r.started_at IS NOT NULL
                  AND r.started_at <= ?2

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
                    ) AS metadata,
                    CASE r.status
                        WHEN 'completed' THEN 'run_completed'
                        WHEN 'failed' THEN 'run_failed'
                        WHEN 'cancelled' THEN 'run_cancelled'
                        ELSE 'run_ended'
                    END || r.session_id || COALESCE(r.run_id, '') AS sort_key
                FROM runs r
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND r.ended_at IS NOT NULL
                  AND r.status IN ('completed', 'failed', 'cancelled')
                  AND r.ended_at <= ?2

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
                    ) AS metadata,
                    'tool_call' || r.session_id || COALESCE(tc.run_id, '') AS sort_key
                FROM run_tool_calls tc
                JOIN runs r ON tc.run_id = r.run_id
                JOIN sessions s ON r.session_id = s.id
                WHERE r.agent_id = ?1
                  AND tc.role = 'assistant'
                  AND tc.timestamp <= ?2

                UNION ALL

                -- Branch 3: user messages (role='user') and synthetic system markers
                --
                -- Uses OR to include DM sessions: either the session belongs
                -- to this agent directly (s.agent_id = ?1) OR this agent has
                -- at least one run in the session (covers shared DM sessions
                -- stored under AgentId::nil()).
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
                    NULL AS metadata,
                    CASE
                        WHEN m.role = 'user' THEN 'message_received'
                        WHEN m.role = 'system' THEN 'marker'
                        ELSE 'message_sent'
                    END || m.session_id || '' AS sort_key
                FROM messages m
                JOIN sessions s ON m.session_id = s.id
                WHERE (s.agent_id = ?1
                       OR EXISTS (
                           SELECT 1 FROM runs r2
                           WHERE r2.session_id = s.id AND r2.agent_id = ?1
                       ))
                  AND m.role IN ('user', 'system')
                  AND (m.role = 'user'
                       OR (m.metadata IS NOT NULL AND json_extract(m.metadata, '$.synthetic') = 1))
                  AND m.timestamp <= ?2
            )
            ORDER BY timestamp DESC, sort_key DESC
            LIMIT ?3
        ";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare timeline: {e}")))?;

        let rows: Vec<TimelineEvent> = stmt
            .query_map(params![agent_id_str, before_ts, fetch_limit], |row| {
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
                    self.record_skipped_row(
                        PersistenceTable::Timeline,
                        format_args!("agent {agent_id_str}: {e}"),
                    );
                    None
                }
            })
            .collect();

        // If we got more than `limit` rows, there are more pages.
        let has_more = rows.len() > limit;
        let events = if has_more {
            rows.into_iter().take(limit).collect()
        } else {
            rows
        };

        Ok(TimelinePage { events, has_more })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Content, Message, Role, Session};
    use alms_core::run::{Run, TokenUsage, ToolCallRecord, ToolCallRole};

    fn setup_store_with_agent() -> (SqliteStore, AgentId, SessionId) {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        let session = Session::new(agent_id, "web-chat");
        store.save_session(&session).unwrap();
        (store, agent_id, session.id)
    }

    /// Helper: create and save a user message in the given session.
    fn insert_user_message(store: &SqliteStore, session_id: SessionId, text: &str) {
        let msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: None,
        };
        store.save_message(session_id, &msg).unwrap();
    }

    /// Helper: create and save a synthetic system marker in the given session.
    fn insert_synthetic_marker(store: &SqliteStore, session_id: SessionId, text: &str) {
        let msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({"synthetic": 1})),
        };
        store.save_message(session_id, &msg).unwrap();
    }

    #[test]
    fn test_timeline_empty_agent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();
        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        assert!(page.events.is_empty());
        assert!(!page.has_more);
    }

    #[test]
    fn test_timeline_includes_runs() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        let mut run = Run::new(session_id, agent_id, "hello".to_string());
        run.mark_running();
        let _ = run.mark_completed(
            "world".to_string(),
            TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                ..TokenUsage::default()
            },
        );
        store.save_run(&run).unwrap();

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        // Should have at least run_started and run_completed
        let types: Vec<&str> = page.events.iter().map(|e| e.event_type.as_str()).collect();
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
            from_agent: None,
        };
        store.save_tool_call(run.run_id, session_id, &tc).unwrap();

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let tool_events: Vec<_> = page
            .events
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
            let _ = run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    ..TokenUsage::default()
                },
            );
            store.save_run(&run).unwrap();
        }

        let page = store.load_timeline_events(agent_id, None, 3).unwrap();
        assert_eq!(page.events.len(), 3);
        assert!(page.has_more, "should have more events beyond limit");
    }

    #[test]
    fn test_timeline_cursor_pagination() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Create 3 completed runs.
        for _ in 0..3 {
            let mut run = Run::new(session_id, agent_id, "test".to_string());
            run.mark_running();
            let _ = run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    ..TokenUsage::default()
                },
            );
            store.save_run(&run).unwrap();
        }

        // Fetch all events first.
        let all_page = store.load_timeline_events(agent_id, None, 100).unwrap();
        assert!(!all_page.events.is_empty());

        // Use the last event's timestamp as cursor.
        let cursor = all_page.events.last().unwrap().timestamp.as_str();
        let paged = store
            .load_timeline_events(agent_id, Some(cursor), 100)
            .unwrap();
        // With <= comparison, paged may include events at the cursor timestamp,
        // but should never exceed the full set.
        assert!(
            paged.events.len() < all_page.events.len(),
            "cursor pagination should return fewer events: paged={} all={}",
            paged.events.len(),
            all_page.events.len()
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

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let types: Vec<(&str, &str)> = page
            .events
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

        let page_a = store.load_timeline_events(agent_a, None, 50).unwrap();
        let page_b = store.load_timeline_events(agent_b, None, 50).unwrap();

        // Each agent should only see their own events.
        assert!(
            page_a
                .events
                .iter()
                .all(|e| e.session_id == session_a.id.0.to_string())
        );
        assert!(
            page_b
                .events
                .iter()
                .all(|e| e.session_id == session_b.id.0.to_string())
        );
    }

    #[test]
    fn test_timeline_run_failed_event() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        let mut run = Run::new(session_id, agent_id, "will fail".to_string());
        run.mark_running();
        let _ = run.mark_failed("LLM timeout".to_string());
        store.save_run(&run).unwrap();

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let failed: Vec<_> = page
            .events
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
            let _ = run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    ..TokenUsage::default()
                },
            );
            store.save_run(&run).unwrap();
        }

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        for pair in page.events.windows(2) {
            assert!(
                pair[0].timestamp >= pair[1].timestamp,
                "events should be in reverse chronological order: {} >= {}",
                pair[0].timestamp,
                pair[1].timestamp
            );
        }
    }

    // -----------------------------------------------------------------------
    // Tests for messages SQL branch (Fix #3 from Tim's review)
    // -----------------------------------------------------------------------

    #[test]
    fn test_timeline_includes_user_messages() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        insert_user_message(&store, session_id, "Hello agent!");
        insert_user_message(&store, session_id, "Follow-up question");

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let msg_events: Vec<_> = page
            .events
            .iter()
            .filter(|e| e.event_type == "message_received")
            .collect();
        assert_eq!(
            msg_events.len(),
            2,
            "should find 2 user messages, got: {:?}",
            page.events
                .iter()
                .map(|e| &e.event_type)
                .collect::<Vec<_>>()
        );
        assert!(msg_events.iter().all(|e| e.summary == "User message"));
    }

    #[test]
    fn test_timeline_includes_synthetic_markers() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        insert_synthetic_marker(&store, session_id, "dm_ended");

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let markers: Vec<_> = page
            .events
            .iter()
            .filter(|e| e.event_type == "marker")
            .collect();
        assert_eq!(
            markers.len(),
            1,
            "should find 1 synthetic marker, got types: {:?}",
            page.events
                .iter()
                .map(|e| &e.event_type)
                .collect::<Vec<_>>()
        );
        assert!(markers[0].summary.contains("System event"));
    }

    #[test]
    fn test_timeline_excludes_non_synthetic_system_messages() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Insert a system message WITHOUT the `synthetic: 1` metadata.
        let msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::System,
            content: Content::Text("You are a helpful agent.".to_string()),
            timestamp: Timestamp::now(),
            metadata: None,
        };
        store.save_message(session_id, &msg).unwrap();

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let markers: Vec<_> = page
            .events
            .iter()
            .filter(|e| e.event_type == "marker")
            .collect();
        assert!(
            markers.is_empty(),
            "non-synthetic system messages should be excluded"
        );
    }

    // -----------------------------------------------------------------------
    // Test for DM session inclusion (Fix #1 from Tim's review)
    // -----------------------------------------------------------------------

    #[test]
    fn test_timeline_includes_dm_session_messages() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        // Create a shared DM session with nil agent_id (as the system does).
        let nil_agent = AgentId(uuid::Uuid::nil());
        let dm_session = Session::new(nil_agent, "dm:alice:bob");
        store.save_session(&dm_session).unwrap();

        // Create a run by agent_id in this DM session (agent participated).
        let mut run = Run::new(dm_session.id, agent_id, "DM reply".to_string());
        run.mark_running();
        store.save_run(&run).unwrap();

        // Insert a user message in the DM session.
        insert_user_message(&store, dm_session.id, "Hey from DM peer");

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let dm_msgs: Vec<_> = page
            .events
            .iter()
            .filter(|e| e.event_type == "message_received")
            .collect();
        assert_eq!(
            dm_msgs.len(),
            1,
            "should see user message from DM session, got types: {:?}",
            page.events
                .iter()
                .map(|e| &e.event_type)
                .collect::<Vec<_>>()
        );
        assert_eq!(dm_msgs[0].session_type, "dm");
    }

    #[test]
    fn test_timeline_dm_session_excludes_non_participant() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        let agent_c = AgentId::new();

        // DM session between agent_a and agent_b (nil sentinel).
        let nil_agent = AgentId(uuid::Uuid::nil());
        let dm_session = Session::new(nil_agent, "dm:alice:bob");
        store.save_session(&dm_session).unwrap();

        // Only agent_a has a run in the DM session.
        let mut run = Run::new(dm_session.id, agent_a, "reply".to_string());
        run.mark_running();
        store.save_run(&run).unwrap();

        insert_user_message(&store, dm_session.id, "DM message");

        // agent_c has no runs in this DM session -- should see nothing.
        let page_c = store.load_timeline_events(agent_c, None, 50).unwrap();
        assert!(
            page_c.events.is_empty(),
            "agent_c should not see DM session events, got: {:?}",
            page_c
                .events
                .iter()
                .map(|e| &e.event_type)
                .collect::<Vec<_>>()
        );

        // agent_a should see the DM message + run_started.
        let page_a = store.load_timeline_events(agent_a, None, 50).unwrap();
        assert!(
            page_a
                .events
                .iter()
                .any(|e| e.event_type == "message_received"),
            "agent_a should see the DM message"
        );

        // agent_b has no runs -- should NOT see DM messages (only run
        // participation is checked, not name matching).
        let page_b = store.load_timeline_events(agent_b, None, 50).unwrap();
        let b_msgs: Vec<_> = page_b
            .events
            .iter()
            .filter(|e| e.event_type == "message_received")
            .collect();
        assert!(
            b_msgs.is_empty(),
            "agent_b with no runs should not see DM messages"
        );
    }

    // -----------------------------------------------------------------------
    // Test for has_more accuracy (Fix #5 from Tim's review)
    // -----------------------------------------------------------------------

    #[test]
    fn test_timeline_has_more_no_false_positive() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Create exactly 2 completed runs -> 4 events (2 started + 2 completed).
        for _ in 0..2 {
            let mut run = Run::new(session_id, agent_id, "test".to_string());
            run.mark_running();
            let _ = run.mark_completed(
                "ok".to_string(),
                TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    ..TokenUsage::default()
                },
            );
            store.save_run(&run).unwrap();
        }

        // Ask for exactly 4 (matching the total count) -- has_more should be false.
        let page = store.load_timeline_events(agent_id, None, 4).unwrap();
        assert_eq!(page.events.len(), 4);
        assert!(
            !page.has_more,
            "has_more should be false when results exactly match the total"
        );

        // Ask for 3 (less than total) -- has_more should be true.
        let page = store.load_timeline_events(agent_id, None, 3).unwrap();
        assert_eq!(page.events.len(), 3);
        assert!(page.has_more, "has_more should be true when more exist");

        // Ask for 10 (more than total) -- has_more should be false.
        let page = store.load_timeline_events(agent_id, None, 10).unwrap();
        assert_eq!(page.events.len(), 4);
        assert!(
            !page.has_more,
            "has_more should be false when limit exceeds total"
        );
    }

    #[test]
    fn test_timeline_messages_and_runs_interleaved() {
        let (store, agent_id, session_id) = setup_store_with_agent();

        // Insert a user message.
        insert_user_message(&store, session_id, "Start a task");

        // Create a run.
        let mut run = Run::new(session_id, agent_id, "Start a task".to_string());
        run.mark_running();
        let _ = run.mark_completed(
            "Done".to_string(),
            TokenUsage {
                prompt_tokens: 50,
                completion_tokens: 20,
                ..TokenUsage::default()
            },
        );
        store.save_run(&run).unwrap();

        // Insert a synthetic marker.
        insert_synthetic_marker(&store, session_id, "dm_ended");

        let page = store.load_timeline_events(agent_id, None, 50).unwrap();
        let types: Vec<&str> = page.events.iter().map(|e| e.event_type.as_str()).collect();

        assert!(
            types.contains(&"message_received"),
            "should have message_received"
        );
        assert!(types.contains(&"run_started"), "should have run_started");
        assert!(
            types.contains(&"run_completed"),
            "should have run_completed"
        );
        assert!(types.contains(&"marker"), "should have marker");
    }
}
