//! Agent registry -- CRUD, migration, default management.

use super::*;

impl SqliteStore {
    // ── Agents ────────────────────────────────────────────────────────────────

    /// Atomically insert an agent record only if the agents table is empty,
    /// and mark it as the default agent.
    ///
    /// Returns `true` if the insert happened, `false` if agents already existed.
    /// The INSERT and set-default happen in a single transaction to avoid both
    /// the TOCTOU race and a partial-failure state where the agent is created
    /// but not yet marked as default.
    pub fn create_agent_if_none_exist(&self, agent: &AgentRecord) -> AlmsResult<bool> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin: {e}")))?;

        let exists: bool = tx
            .query_row("SELECT 1 FROM agents LIMIT 1", [], |_row| Ok(true))
            .unwrap_or(false);

        if exists {
            return Ok(false);
        }

        tx.execute(
            "INSERT INTO agents \
             (id, name, description, model, posture, provider, telegram_token, \
              is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
              gemini_thinking_budget, summary_provider, summary_model, worktree_mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                agent.id.0.to_string(),
                &agent.name,
                &agent.description,
                agent.model.as_deref(),
                agent.posture.as_deref(),
                agent.provider.as_deref(),
                agent.telegram_token.as_deref(),
                1i32,
                agent.created_at.to_rfc3339(),
                agent.last_active.to_rfc3339(),
                agent.thinking_budget_tokens.map(i64::from),
                agent.reasoning_effort.map(|e| e.as_wire_str().to_string()),
                agent.gemini_thinking_budget.map(i64::from),
                agent.summary_provider.as_deref(),
                agent.summary_model.as_deref(),
                agent.worktree_mode.as_wire_str(),
            ],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite create_agent_if_none_exist: {e}")))?;

        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit: {e}")))?;
        Ok(true)
    }

    /// Insert a new agent record. Fails if the name or id already exists.
    pub fn create_agent(&self, agent: &AgentRecord) -> AlmsResult<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO agents \
                 (id, name, description, model, posture, provider, telegram_token, \
                  is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
                  gemini_thinking_budget, summary_provider, summary_model, worktree_mode) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    agent.id.0.to_string(),
                    &agent.name,
                    &agent.description,
                    agent.model.as_deref(),
                    agent.posture.as_deref(),
                    agent.provider.as_deref(),
                    agent.telegram_token.as_deref(),
                    agent.is_default as i32,
                    agent.created_at.to_rfc3339(),
                    agent.last_active.to_rfc3339(),
                    agent.thinking_budget_tokens.map(i64::from),
                    agent.reasoning_effort.map(|e| e.as_wire_str().to_string()),
                    agent.gemini_thinking_budget.map(i64::from),
                    agent.summary_provider.as_deref(),
                    agent.summary_model.as_deref(),
                    agent.worktree_mode.as_wire_str(),
                ],
            )
            .map_err(|e| match &e {
                rusqlite::Error::SqliteFailure(err, _)
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    AlmsError::DuplicateName(agent.name.clone())
                }
                _ => AlmsError::Runtime(format!("SQLite create_agent: {e}")),
            })?;
        Ok(())
    }

    /// Update an existing agent's mutable config fields (matched by id).
    ///
    /// Does NOT update `name` or `is_default` -- use `set_default_agent()` for
    /// default changes, and name is immutable after creation.
    pub fn update_agent(&self, agent: &AgentRecord) -> AlmsResult<()> {
        let affected = self
            .conn
            .lock()
            .execute(
                "UPDATE agents SET description = ?1, model = ?2, \
                 posture = ?3, provider = ?4, telegram_token = ?5, \
                 thinking_budget_tokens = ?6, reasoning_effort = ?7, \
                 gemini_thinking_budget = ?8, summary_provider = ?9, \
                 summary_model = ?10, worktree_mode = ?11, \
                 last_active = ?12 WHERE id = ?13",
                params![
                    &agent.description,
                    agent.model.as_deref(),
                    agent.posture.as_deref(),
                    agent.provider.as_deref(),
                    agent.telegram_token.as_deref(),
                    agent.thinking_budget_tokens.map(i64::from),
                    agent.reasoning_effort.map(|e| e.as_wire_str().to_string()),
                    agent.gemini_thinking_budget.map(i64::from),
                    agent.summary_provider.as_deref(),
                    agent.summary_model.as_deref(),
                    agent.worktree_mode.as_wire_str(),
                    agent.last_active.to_rfc3339(),
                    agent.id.0.to_string(),
                ],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite update_agent: {e}")))?;
        if affected == 0 {
            return Err(AlmsError::AgentNotFound(agent.id.0.to_string()));
        }
        Ok(())
    }

    /// Load an agent by its UUID.
    pub fn load_agent_by_id(&self, id: AgentId) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, posture, provider, telegram_token, \
             is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode \
             FROM agents WHERE id = ?1",
            params![id.0.to_string()],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite load_agent_by_id: {e}"))),
        }
    }

    /// Load an agent by its unique name slug.
    pub fn load_agent_by_name(&self, name: &str) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, posture, provider, telegram_token, \
             is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode \
             FROM agents WHERE name = ?1",
            params![name],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!(
                "SQLite load_agent_by_name: {e}"
            ))),
        }
    }

    /// Load the default agent, if one exists.
    pub fn get_default_agent(&self) -> AlmsResult<Option<AgentRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, name, description, model, posture, provider, telegram_token, \
             is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode \
             FROM agents WHERE is_default = 1 LIMIT 1",
            [],
            parse_agent_row,
        );
        match result {
            Ok(agent) => Ok(Some(agent)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AlmsError::Runtime(format!("SQLite get_default_agent: {e}"))),
        }
    }

    /// List all agents, ordered by creation time.
    pub fn list_agents(&self) -> AlmsResult<Vec<AgentRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, model, posture, provider, telegram_token, \
                 is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
                 gemini_thinking_budget, summary_provider, summary_model, worktree_mode \
                 FROM agents ORDER BY created_at",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare agents: {e}")))?;

        let rows = stmt
            .query_map([], parse_agent_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query agents: {e}")))?
            .filter_map(|r| match r {
                Ok(agent) => Some(agent),
                Err(e) => {
                    tracing::warn!("Skipping unparseable agent row: {}", e);
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Load all agents that have a Telegram bot token configured.
    ///
    /// Used by the gateway to spawn per-agent polling loops at startup.
    pub fn agents_with_telegram(&self) -> AlmsResult<Vec<AgentRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, description, model, posture, provider, telegram_token, \
                 is_default, created_at, last_active, thinking_budget_tokens, reasoning_effort, \
                 gemini_thinking_budget, summary_provider, summary_model, worktree_mode \
                 FROM agents WHERE telegram_token IS NOT NULL AND telegram_token != '' \
                 ORDER BY created_at",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare agents_with_telegram: {e}")))?;

        let rows = stmt
            .query_map([], parse_agent_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query agents_with_telegram: {e}")))?
            .filter_map(|r| match r {
                Ok(agent) => Some(agent),
                Err(e) => {
                    tracing::warn!("Skipping unparseable agent row: {}", e);
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Delete an agent and all its dependent data (sessions, messages, audit
    /// events, context summaries, jobs).
    ///
    /// Wrapped in a transaction so a crash mid-delete cannot leave orphaned
    /// rows. Returns `true` if the agent existed and was deleted.
    pub fn delete_agent(&self, id: AgentId) -> AlmsResult<bool> {
        let mut conn = self.conn.lock();
        let id_str = id.0.to_string();

        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin delete_agent: {e}")))?;

        // 1. Collect session IDs belonging to this agent
        let session_ids: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM sessions WHERE agent_id = ?1")
                .map_err(|e| AlmsError::Runtime(format!("SQLite prepare session query: {e}")))?;
            stmt.query_map(params![&id_str], |row| row.get(0))
                .map_err(|e| AlmsError::Runtime(format!("SQLite query agent sessions: {e}")))?
                .filter_map(|r| r.ok())
                .collect()
        };

        // 2. Delete dependent rows for each session (FK order)
        for sid in &session_ids {
            tx.execute(
                "DELETE FROM context_summaries WHERE session_id = ?1",
                params![sid],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete summaries for session: {e}")))?;
            // Delete cross-session episodic summaries for this session
            // (added in #874). The `session_summaries` table has
            // `session_id REFERENCES sessions(id)`, so leaving these rows
            // behind would block the `DELETE FROM sessions` step below
            // with a FOREIGN KEY constraint failure (#985).
            tx.execute(
                "DELETE FROM session_summaries WHERE session_id = ?1",
                params![sid],
            )
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite delete session_summaries for session: {e}"))
            })?;
            tx.execute(
                "DELETE FROM audit_events WHERE session_id = ?1",
                params![sid],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete audit for session: {e}")))?;
            tx.execute("DELETE FROM messages WHERE session_id = ?1", params![sid])
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete messages for session: {e}"))
                })?;
            // Delete tool call records for runs belonging to this session.
            tx.execute(
                "DELETE FROM run_tool_calls WHERE run_id IN \
                 (SELECT run_id FROM runs WHERE session_id = ?1)",
                params![sid],
            )
            .map_err(|e| {
                AlmsError::Runtime(format!("SQLite delete run tool calls for session: {e}"))
            })?;
            // Delete runs belonging to this session.
            tx.execute("DELETE FROM runs WHERE session_id = ?1", params![sid])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete runs for session: {e}")))?;
        }

        // 3. Delete the sessions themselves
        tx.execute("DELETE FROM sessions WHERE agent_id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete agent sessions: {e}")))?;

        // 4. Clear DM-orphan rows the agent created in shared DM sessions (#992).
        //
        // DM sessions are owned by `AgentId::nil()` (sentinel), so the
        // `WHERE agent_id = ?1` query in step 1 never picks them up. The
        // deleted agent's contributions to those shared sessions live in
        // `runs.agent_id`, `run_tool_calls.from_agent`, and
        // `session_summaries.agent_id` -- none of which carry FKs against
        // `agents`, so they don't block the delete, but they accumulate as
        // dangling rows over time with multi-agent DM use.
        //
        // Order: `run_tool_calls` first (logically depends on `runs`),
        // then `session_summaries`, then `runs`. No FK enforces this --
        // it's an audit-clarity choice. We do NOT delete the shared DM
        // session row itself: the surviving partner still uses it.
        tx.execute(
            "DELETE FROM run_tool_calls WHERE from_agent = ?1",
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete dm-orphan run_tool_calls: {e}")))?;
        tx.execute(
            "DELETE FROM session_summaries WHERE agent_id = ?1",
            params![&id_str],
        )
        .map_err(|e| {
            AlmsError::Runtime(format!("SQLite delete dm-orphan session_summaries: {e}"))
        })?;
        tx.execute("DELETE FROM runs WHERE agent_id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete dm-orphan runs: {e}")))?;

        // 5. Delete jobs belonging to this agent
        tx.execute("DELETE FROM jobs WHERE agent_id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete agent jobs: {e}")))?;

        // 6. Delete the agent row
        let affected = tx
            .execute("DELETE FROM agents WHERE id = ?1", params![&id_str])
            .map_err(|e| AlmsError::Runtime(format!("SQLite delete_agent: {e}")))?;

        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit delete_agent: {e}")))?;
        Ok(affected > 0)
    }

    /// Set an agent as the default, clearing any previous default.
    ///
    /// Wrapped in a transaction so a crash between the two UPDATEs cannot
    /// leave the system with zero default agents.
    ///
    /// Returns `AgentNotFound` if the given ID does not exist in the table.
    pub fn set_default_agent(&self, id: AgentId) -> AlmsResult<()> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| AlmsError::Runtime(format!("SQLite begin: {e}")))?;
        tx.execute("UPDATE agents SET is_default = 0 WHERE is_default = 1", [])
            .map_err(|e| AlmsError::Runtime(format!("SQLite clear_default: {e}")))?;
        let affected = tx
            .execute(
                "UPDATE agents SET is_default = 1 WHERE id = ?1",
                params![id.0.to_string()],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite set_default: {e}")))?;
        if affected == 0 {
            return Err(AlmsError::AgentNotFound(id.0.to_string()));
        }
        tx.commit()
            .map_err(|e| AlmsError::Runtime(format!("SQLite commit: {e}")))?;
        Ok(())
    }

    /// Update an agent's `last_active` timestamp.
    pub fn touch_agent(&self, id: AgentId) -> AlmsResult<()> {
        let rows = self
            .conn
            .lock()
            .execute(
                "UPDATE agents SET last_active = ?1 WHERE id = ?2",
                params![chrono::Utc::now().to_rfc3339(), id.0.to_string()],
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite touch_agent: {e}")))?;
        if rows == 0 {
            tracing::debug!(agent_id = %id, "touch_agent: no agent found with this ID");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::new_message;
    use super::super::*;
    use crate::types::Session;
    use alms_core::job::{Job, JobId, JobSchedule, JobStatus};
    use alms_core::registry::AgentRecord;
    use alms_core::run::{Run, ToolCallRecord, ToolCallRole};

    fn new_agent(name: &str) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(),
            name: name.to_string(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: alms_core::WorktreeMode::Off,
            is_default: false,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_agent_create_and_load_by_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.id, agent.id);
        assert_eq!(loaded.name, "atlas");
        assert!(!loaded.is_default);
        assert!(loaded.model.is_none());
    }

    #[test]
    fn test_agent_load_by_name() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("researcher");
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_name("researcher").unwrap().unwrap();
        assert_eq!(loaded.id, agent.id);

        // Non-existent name returns None
        assert!(store.load_agent_by_name("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_agent_list_ordered() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut a1 = new_agent("alpha");
        a1.created_at = chrono::Utc::now() - chrono::Duration::seconds(10);
        store.create_agent(&a1).unwrap();

        let a2 = new_agent("beta");
        store.create_agent(&a2).unwrap();

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "alpha");
        assert_eq!(agents[1].name, "beta");
    }

    #[test]
    fn test_agent_delete() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("doomed");
        store.create_agent(&agent).unwrap();

        assert!(store.delete_agent(agent.id).unwrap());
        assert!(store.load_agent_by_id(agent.id).unwrap().is_none());

        // Deleting again returns false
        assert!(!store.delete_agent(agent.id).unwrap());
    }

    #[test]
    fn test_agent_delete_cascades_sessions_messages_audit_jobs() {
        let store = SqliteStore::open_in_memory().unwrap();

        // Create two agents -- one to delete, one to keep as control
        let doomed = new_agent("doomed");
        let survivor = new_agent("survivor");
        store.create_agent(&doomed).unwrap();
        store.create_agent(&survivor).unwrap();

        // Create sessions for both agents
        let ds = Session::new(doomed.id, "ctx-doomed");
        let ss = Session::new(survivor.id, "ctx-survivor");
        store.save_session(&ds).unwrap();
        store.save_session(&ss).unwrap();

        // Add messages to both sessions
        store
            .save_message(ds.id, &new_message("doomed msg"))
            .unwrap();
        store
            .save_message(ss.id, &new_message("survivor msg"))
            .unwrap();

        // Add audit events to both sessions
        let doomed_audit = AuditEvent::allow(
            ds.id,
            "echo",
            serde_json::json!({"text": "hi"}),
            serde_json::json!("hi"),
        );
        let survivor_audit = AuditEvent::allow(
            ss.id,
            "echo",
            serde_json::json!({"text": "ok"}),
            serde_json::json!("ok"),
        );
        store.save_audit(&doomed_audit).unwrap();
        store.save_audit(&survivor_audit).unwrap();

        // Add context summaries to both sessions
        let summary = ContextSummary {
            text: "test summary".to_string(),
            messages_covered: 1,
            updated_at: Some(Timestamp::now()),
        };
        store.save_summary(ds.id, &summary).unwrap();
        store.save_summary(ss.id, &summary).unwrap();

        // Add jobs for both agents
        let doomed_job = Job {
            id: JobId::new(),
            agent_id: doomed.id,
            prompt: "doomed job".to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        let survivor_job = Job {
            id: JobId::new(),
            agent_id: survivor.id,
            prompt: "survivor job".to_string(),
            schedule: JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            status: JobStatus::Pending,
            created_at: chrono::Utc::now(),
            next_run_at: None,
            last_run_at: None,
        };
        store.save_job(&doomed_job).unwrap();
        store.save_job(&survivor_job).unwrap();

        // Delete the doomed agent -- should cascade
        assert!(store.delete_agent(doomed.id).unwrap());

        // Doomed agent's data is gone
        assert!(store.load_agent_by_id(doomed.id).unwrap().is_none());
        assert!(store.load_sessions_by_agent(doomed.id).unwrap().is_empty());
        assert!(store.load_messages(ds.id).unwrap().is_empty());
        assert!(store.load_audit(ds.id).unwrap().is_empty());

        // Survivor agent's data is untouched
        assert!(store.load_agent_by_id(survivor.id).unwrap().is_some());
        let survivor_sessions = store.load_sessions_by_agent(survivor.id).unwrap();
        assert_eq!(survivor_sessions.len(), 1);
        assert_eq!(store.load_messages(ss.id).unwrap().len(), 1);
        assert_eq!(store.load_audit(ss.id).unwrap().len(), 1);

        // Survivor's job still exists, doomed's job is gone
        let all_jobs = store.load_all_jobs_unfiltered().unwrap();
        assert_eq!(all_jobs.len(), 1);
        assert_eq!(all_jobs[0].agent_id, survivor.id);
    }

    #[test]
    fn test_agent_set_default_clears_previous() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut a1 = new_agent("first");
        a1.is_default = true;
        store.create_agent(&a1).unwrap();

        let a2 = new_agent("second");
        store.create_agent(&a2).unwrap();

        // Set second as default
        store.set_default_agent(a2.id).unwrap();

        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.id, a2.id);

        // First should no longer be default
        let first = store.load_agent_by_id(a1.id).unwrap().unwrap();
        assert!(!first.is_default);
    }

    #[test]
    fn test_agent_unique_name_constraint() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a1 = new_agent("unique");
        store.create_agent(&a1).unwrap();

        // Different ID, same name -- should fail (UNIQUE constraint)
        let mut a2 = new_agent("unique");
        a2.id = AgentId::new(); // different UUID
        // INSERT OR REPLACE keys on PRIMARY KEY (id), not name.
        // A different id with the same name should violate UNIQUE.
        let result = store.create_agent(&a2);
        assert!(
            matches!(result, Err(alms_core::AlmsError::DuplicateName(ref name)) if name == "unique"),
            "Expected DuplicateName error, got: {:?}",
            result,
        );
    }

    #[test]
    fn test_agent_touch_updates_last_active() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("touchme");
        agent.last_active = chrono::Utc::now() - chrono::Duration::seconds(100);
        store.create_agent(&agent).unwrap();

        let before = store.load_agent_by_id(agent.id).unwrap().unwrap();
        store.touch_agent(agent.id).unwrap();
        let after = store.load_agent_by_id(agent.id).unwrap().unwrap();

        assert!(after.last_active > before.last_active);
    }

    #[test]
    fn test_agent_touch_nonexistent_succeeds() {
        let store = SqliteStore::open_in_memory().unwrap();
        let fake_id = AgentId(uuid::Uuid::new_v4());
        // Should succeed (not error) even for a nonexistent agent.
        store.touch_agent(fake_id).unwrap();
    }

    #[test]
    fn test_agent_with_overrides() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("custom");
        agent.model = Some("anthropic/claude-sonnet-4-20250514".to_string());
        agent.posture = Some("guarded".to_string());
        agent.description = "A custom agent".to_string();
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(
            loaded.model.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
        assert_eq!(loaded.posture.as_deref(), Some("guarded"));
        assert_eq!(loaded.description, "A custom agent");
    }

    #[test]
    fn test_agent_get_default_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert!(store.get_default_agent().unwrap().is_none());
    }

    #[test]
    fn test_agent_update_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("mutable");
        store.create_agent(&agent).unwrap();

        agent.description = "Updated description".to_string();
        agent.model = Some("new-model".to_string());
        agent.posture = Some("guarded".to_string());
        store.update_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.description, "Updated description");
        assert_eq!(loaded.model.as_deref(), Some("new-model"));
        assert_eq!(loaded.posture.as_deref(), Some("guarded"));
    }

    #[test]
    fn test_agent_set_default_nonexistent_errors() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("exists");
        agent.is_default = true;
        store.create_agent(&agent).unwrap();

        // Setting a nonexistent agent as default should error
        let fake_id = AgentId::new();
        let result = store.set_default_agent(fake_id);
        assert!(result.is_err());

        // The existing agent should still be default (rollback undid the clear)
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(loaded.is_default);
    }

    #[test]
    fn test_create_agent_if_none_exist_inserts_when_empty() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("main");
        let inserted = store.create_agent_if_none_exist(&agent).unwrap();

        assert!(inserted);
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "main");
        assert!(
            agents[0].is_default,
            "inserted agent should be marked as default"
        );
    }

    #[test]
    fn test_create_agent_if_none_exist_skips_when_agents_present() {
        let store = SqliteStore::open_in_memory().unwrap();

        // Pre-populate an agent
        let existing = new_agent("atlas");
        store.create_agent(&existing).unwrap();

        // Attempt to insert another agent via the atomic method
        let new = new_agent("main");
        let inserted = store.create_agent_if_none_exist(&new).unwrap();

        assert!(!inserted);
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "atlas");
    }

    #[test]
    fn test_create_agent_if_none_exist_idempotent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("main");

        let first = store.create_agent_if_none_exist(&agent).unwrap();
        assert!(first);

        // Second call sees the agent inserted by the first call and returns false
        let second = store.create_agent_if_none_exist(&agent).unwrap();
        assert!(!second);

        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
    }

    #[test]
    fn test_delete_agent_cascades_tool_calls_and_runs() {
        let store = SqliteStore::open_in_memory().unwrap();

        let doomed = new_agent("doomed");
        let survivor = new_agent("survivor");
        store.create_agent(&doomed).unwrap();
        store.create_agent(&survivor).unwrap();

        // Create sessions, runs, and tool calls for both agents.
        let ds = Session::new(doomed.id, "ctx-doomed");
        let ss = Session::new(survivor.id, "ctx-survivor");
        store.save_session(&ds).unwrap();
        store.save_session(&ss).unwrap();

        let d_run = Run::new(ds.id, doomed.id, "hello".to_string());
        let d_run_id = d_run.run_id;
        store.save_run(&d_run).unwrap();
        store
            .save_tool_calls(
                d_run_id,
                &[
                    ToolCallRecord {
                        seq: 0,
                        role: ToolCallRole::Assistant,
                        tool_name: Some("echo".to_string()),
                        tool_id: Some("call_0".to_string()),
                        params: Some(r#"{"text":"hello"}"#.to_string()),
                        result: None,
                        timestamp: chrono::Utc::now(),
                        from_agent: None,
                    },
                    ToolCallRecord {
                        seq: 1,
                        role: ToolCallRole::Tool,
                        tool_name: Some("echo".to_string()),
                        tool_id: Some("call_0".to_string()),
                        params: None,
                        result: Some(r#""result_ok""#.to_string()),
                        timestamp: chrono::Utc::now(),
                        from_agent: None,
                    },
                ],
            )
            .unwrap();

        let s_run = Run::new(ss.id, survivor.id, "hello".to_string());
        let s_run_id = s_run.run_id;
        store.save_run(&s_run).unwrap();
        store
            .save_tool_call(
                s_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("math".to_string()),
                    tool_id: Some("call_0".to_string()),
                    params: Some(r#"{"text":"hello"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: None,
                },
            )
            .unwrap();

        // Delete the doomed agent.
        assert!(store.delete_agent(doomed.id).unwrap());

        // Doomed agent's tool calls and runs are gone.
        assert_eq!(store.count_tool_calls(d_run_id).unwrap(), 0);
        assert!(store.load_run(d_run_id).unwrap().is_none());

        // Survivor's data is untouched.
        assert_eq!(store.count_tool_calls(s_run_id).unwrap(), 1);
        assert!(store.load_run(s_run_id).unwrap().is_some());
    }

    #[test]
    fn test_delete_agent_cascades_session_summaries() {
        // Regression test for #985: `delete_agent` must remove
        // `session_summaries` rows (added in #874) before deleting the
        // sessions themselves. The `session_summaries.session_id`
        // column has a `REFERENCES sessions(id)` FK, so any leftover
        // row triggers `FOREIGN KEY constraint failed` on the
        // `DELETE FROM sessions WHERE agent_id = ?1` step.
        let store = SqliteStore::open_in_memory().unwrap();

        // Two agents -- one to delete with a full child-row history, one
        // as a control to assert isolation.
        let doomed = new_agent("doomed");
        let survivor = new_agent("survivor");
        store.create_agent(&doomed).unwrap();
        store.create_agent(&survivor).unwrap();

        // Sessions for both.
        let ds = Session::new(doomed.id, "ctx-doomed");
        let ss = Session::new(survivor.id, "ctx-survivor");
        store.save_session(&ds).unwrap();
        store.save_session(&ss).unwrap();

        // Episodic summaries for both -- this is the row that would
        // block the cascade pre-fix.
        store
            .upsert_session_summary(
                doomed.id,
                ds.id,
                "doomed session summary",
                None,
                Some("User chat"),
            )
            .unwrap();
        store
            .upsert_session_summary(
                survivor.id,
                ss.id,
                "survivor session summary",
                None,
                Some("User chat"),
            )
            .unwrap();

        // Plus a run + tool call so the full v0.2.x child-row
        // history is exercised in the same test.
        let d_run = Run::new(ds.id, doomed.id, "hello".to_string());
        let d_run_id = d_run.run_id;
        store.save_run(&d_run).unwrap();
        store
            .save_tool_call(
                d_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("echo".to_string()),
                    tool_id: Some("call_0".to_string()),
                    params: Some(r#"{"text":"hi"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: None,
                },
            )
            .unwrap();

        // Plus shared-DM-session rows so the orphan-by-agent-id audit
        // loop below has rows to bite against. DM sessions are owned by
        // `AgentId::nil()`, so step 1's `WHERE sessions.agent_id = ?1`
        // collection never picks up `dm.id` and step-2's per-session
        // loop won't sweep these rows. Only step-4's per-agent DELETEs
        // can clear them -- which is exactly what the audit loop tests.
        // Without this fixture, the orphan-by-agent-id loop would be
        // a no-op against this test (Tim's review on PR #1000).
        let dm = Session::new(AgentId::nil(), "dm:doomed:survivor");
        store.save_session(&dm).unwrap();
        let dm_run = Run::new(dm.id, doomed.id, "ping".to_string());
        let dm_run_id = dm_run.run_id;
        store.save_run(&dm_run).unwrap();
        store
            .save_tool_call(
                dm_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("send_message".to_string()),
                    tool_id: Some("call_dm0".to_string()),
                    params: Some(r#"{"to":"survivor","text":"hi"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(doomed.id.0.to_string()),
                },
            )
            .unwrap();
        store
            .upsert_session_summary(
                doomed.id,
                dm.id,
                "doomed perspective on DM",
                Some(dm_run_id),
                Some("DM with survivor"),
            )
            .unwrap();

        // Pre-delete sanity: each orphan-by-agent-id class has at least
        // one row keyed on the doomed agent that lives on the shared DM
        // session (and so survives step-1's session-id sweep). The audit
        // loop below would be vacuous without these rows -- only step-4's
        // per-agent-id DELETEs can clear them. (Tim's review on PR #1000.)
        {
            let conn = store.conn.lock();
            let agent_id_str = doomed.id.0.to_string();
            let dm_id_str = dm.id.0.to_string();

            // runs: by agent_id on the DM session
            let n_runs_dm: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE agent_id = ?1 AND session_id = ?2",
                    params![&agent_id_str, &dm_id_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                n_runs_dm >= 1,
                "pre-delete fixture must place at least one `runs` row \
                 for doomed agent on shared DM session"
            );

            // run_tool_calls: by from_agent on a run that lives on the DM session
            let n_calls_dm: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM run_tool_calls \
                     WHERE from_agent = ?1 AND run_id IN \
                     (SELECT run_id FROM runs WHERE session_id = ?2)",
                    params![&agent_id_str, &dm_id_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                n_calls_dm >= 1,
                "pre-delete fixture must place at least one `run_tool_calls` row \
                 for doomed agent on shared DM session"
            );

            // session_summaries: by agent_id on the DM session
            let n_summ_dm: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM session_summaries \
                     WHERE agent_id = ?1 AND session_id = ?2",
                    params![&agent_id_str, &dm_id_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                n_summ_dm >= 1,
                "pre-delete fixture must place at least one `session_summaries` row \
                 for doomed agent on shared DM session"
            );
        }

        // Delete the doomed agent -- must succeed without a FK error.
        assert!(store.delete_agent(doomed.id).unwrap());

        // Doomed agent's data is gone, including the episodic summary.
        assert!(store.load_agent_by_id(doomed.id).unwrap().is_none());
        assert!(store.load_sessions_by_agent(doomed.id).unwrap().is_empty());
        assert!(
            store
                .load_session_summary(doomed.id, ds.id)
                .unwrap()
                .is_none(),
            "doomed agent's session_summaries row should have been deleted"
        );
        assert!(
            store
                .load_session_summaries(doomed.id, 10, None)
                .unwrap()
                .is_empty(),
            "doomed agent should have no remaining session_summaries"
        );

        // Survivor's episodic summary is untouched.
        let survivor_summary = store
            .load_session_summary(survivor.id, ss.id)
            .unwrap()
            .expect("survivor's session_summaries row must remain");
        assert_eq!(survivor_summary.summary, "survivor session summary");

        // Generic audit loop: every table with a `REFERENCES sessions(id)` FK
        // declared in `crates/alms-session/src/sqlite/mod.rs` must have zero
        // rows pointing at the deleted agent's session IDs after `delete_agent`.
        // The point is future-proofing: when a new child table is added that
        // references `sessions(id)` and the author forgets to wire it into
        // `delete_agent`, this loop catches the cascade gap without anyone
        // having to update per-table assertions by hand. Add the new table
        // name to `fk_session_tables` and the test fails until the cascade
        // covers it. See PR #991 review for context.
        let fk_session_tables = ["messages", "context_summaries", "session_summaries"];
        let conn = store.conn.lock();
        for table in fk_session_tables {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                    params![ds.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                n, 0,
                "orphan row in `{table}` referencing deleted agent's session_id after delete_agent"
            );
        }

        // Orphan-by-agent-id audit loop (#992): tables that store the
        // agent's id directly (rather than a session_id FK) must also
        // be empty for the deleted agent. These tables don't carry FKs
        // against `agents`, so they wouldn't surface as a delete failure
        // -- the loop is the only thing that catches a missed cleanup.
        // Same future-proofing intent as the FK loop above: when a new
        // child table grows an `agent_id` / `from_agent` column, add
        // (table, column) here and the test fails until the cascade
        // covers it.
        //
        // The fixture above places one row per orphan class on a shared
        // DM session (`AgentId::nil()`-owned), so step-1's per-session
        // loop cannot reach them. The only path that clears them is
        // step-4's per-agent DELETEs in `delete_agent`. Without step 4,
        // every assertion in this loop fails. The dedicated
        // `test_delete_agent_clears_dm_orphan_rows` test is the
        // primary regression for the same fix; this loop additionally
        // future-proofs the cascade by failing closed when a new
        // (table, column) lands without a matching cleanup.
        let agent_id_str = doomed.id.0.to_string();
        let orphan_by_agent_tables = [
            ("runs", "agent_id"),
            ("run_tool_calls", "from_agent"),
            ("session_summaries", "agent_id"),
        ];
        for (table, column) in orphan_by_agent_tables {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    params![&agent_id_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                n, 0,
                "orphan row in `{table}.{column}` referencing deleted agent's id after delete_agent"
            );
        }

        // The shared DM session row itself must survive: it's owned by
        // `AgentId::nil()`, not by the deleted agent, and the surviving
        // partner still uses it. (Both-partners-deleted is tracked as a
        // separate v0.2.4 follow-up issue.)
        let dm_session_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dm_session_count, 1,
            "shared DM session row must survive delete_agent (not owned by deleted agent)"
        );
    }

    #[test]
    fn test_delete_agent_clears_dm_orphan_rows() {
        // Regression test for #992: rows the deleted agent created
        // *inside a shared DM session* are not picked up by the
        // `WHERE sessions.agent_id = ?1` collection step in
        // `delete_agent`, because DM sessions are owned by the
        // `AgentId::nil()` sentinel. Pre-fix, those rows accumulated
        // as dangling orphans in `runs`, `run_tool_calls`, and
        // `session_summaries`. Post-fix, the three direct DELETEs
        // added in step 4 of `delete_agent` clear them.
        //
        // The shared DM session row itself must NOT be deleted: the
        // surviving partner still owns half of the conversation.
        let store = SqliteStore::open_in_memory().unwrap();

        let agent_a = new_agent("agent-a");
        let agent_b = new_agent("agent-b");
        store.create_agent(&agent_a).unwrap();
        store.create_agent(&agent_b).unwrap();

        // Shared DM session under the nil-agent sentinel -- both
        // participants reference this single session row.
        let dm = Session::new(AgentId::nil(), "dm:agent-a:agent-b");
        store.save_session(&dm).unwrap();

        // Each participant has their own non-DM session too, so we
        // can assert step-1's per-session loop (clearing rows by
        // session_id) interacts cleanly with step-4's per-agent loop.
        let a_self = Session::new(agent_a.id, "user:agent-a");
        let b_self = Session::new(agent_b.id, "user:agent-b");
        store.save_session(&a_self).unwrap();
        store.save_session(&b_self).unwrap();

        // A's contributions inside the shared DM session: a run, a
        // tool call attributed to A via `from_agent`, and an
        // episodic summary the runtime would generate from A's
        // perspective (`session_summaries.agent_id = A`,
        // `session_id = dm`).
        let a_dm_run = Run::new(dm.id, agent_a.id, "ping".to_string());
        let a_dm_run_id = a_dm_run.run_id;
        store.save_run(&a_dm_run).unwrap();
        store
            .save_tool_call(
                a_dm_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("send_message".to_string()),
                    tool_id: Some("call_a0".to_string()),
                    params: Some(r#"{"to":"agent-b","text":"hi"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(agent_a.id.0.to_string()),
                },
            )
            .unwrap();
        store
            .upsert_session_summary(
                agent_a.id,
                dm.id,
                "A's perspective on DM with B",
                Some(a_dm_run_id),
                Some("DM with agent-b"),
            )
            .unwrap();

        // B's mirror-image contributions on the same shared DM
        // session, plus B's own non-DM session for completeness.
        let b_dm_run = Run::new(dm.id, agent_b.id, "pong".to_string());
        let b_dm_run_id = b_dm_run.run_id;
        store.save_run(&b_dm_run).unwrap();
        store
            .save_tool_call(
                b_dm_run_id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("send_message".to_string()),
                    tool_id: Some("call_b0".to_string()),
                    params: Some(r#"{"to":"agent-a","text":"hi back"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(agent_b.id.0.to_string()),
                },
            )
            .unwrap();
        store
            .upsert_session_summary(
                agent_b.id,
                dm.id,
                "B's perspective on DM with A",
                Some(b_dm_run_id),
                Some("DM with agent-a"),
            )
            .unwrap();

        // Sanity check: 6 orphan-class rows total before the delete --
        // 2 runs, 2 run_tool_calls, 2 session_summaries -- one of
        // each per agent, all keyed on the shared DM session.
        {
            let conn = store.conn.lock();
            let count_runs: i64 = conn
                .query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))
                .unwrap();
            let count_calls: i64 = conn
                .query_row("SELECT COUNT(*) FROM run_tool_calls", [], |r| r.get(0))
                .unwrap();
            let count_summaries: i64 = conn
                .query_row("SELECT COUNT(*) FROM session_summaries", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count_runs, 2, "pre-delete: 2 runs (one per agent)");
            assert_eq!(count_calls, 2, "pre-delete: 2 tool calls (one per agent)");
            assert_eq!(
                count_summaries, 2,
                "pre-delete: 2 session summaries (one per agent)"
            );
        }

        // Delete A. The shared DM session has agent_id = nil, so
        // step 1's `WHERE sessions.agent_id = ?1` query never sees
        // it -- the new step-4 cleanup is the only path that clears
        // A's rows on the DM session.
        assert!(store.delete_agent(agent_a.id).unwrap());

        // ── A's side: every orphan-class row is gone. ────────────────
        let a_id_str = agent_a.id.0.to_string();
        let conn = store.conn.lock();

        let n_runs_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                params![&a_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_runs_a, 0, "A's runs must be cleared (incl. DM run)");

        let n_calls_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tool_calls WHERE from_agent = ?1",
                params![&a_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n_calls_a, 0,
            "A's run_tool_calls must be cleared (incl. DM tool calls)"
        );

        let n_summaries_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_summaries WHERE agent_id = ?1",
                params![&a_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n_summaries_a, 0,
            "A's session_summaries must be cleared (incl. DM-perspective summary)"
        );

        // ── B's side: untouched. ──────────────────────────────────────
        let b_id_str = agent_b.id.0.to_string();

        let n_runs_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                params![&b_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_runs_b, 1, "B's DM run must remain intact");

        let n_calls_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tool_calls WHERE from_agent = ?1",
                params![&b_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_calls_b, 1, "B's DM tool call must remain intact");

        let n_summaries_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_summaries WHERE agent_id = ?1",
                params![&b_id_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n_summaries_b, 1,
            "B's DM-perspective summary must remain intact"
        );

        // ── Shared DM session row itself: untouched. ─────────────────
        // The DM session is owned by `AgentId::nil()` and shared by
        // both participants. Deleting A must not delete the session.
        let dm_session_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dm_session_count, 1,
            "shared DM session row must NOT be deleted -- B still uses it"
        );

        // B's own non-DM session is also untouched.
        let b_self_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![b_self.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_self_count, 1, "B's own session must remain");

        // A's own non-DM session is gone (covered by step-1's
        // `WHERE sessions.agent_id = ?1` collection).
        let a_self_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![a_self.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(a_self_count, 0, "A's own non-DM session must be deleted");
    }

    #[test]
    fn test_agent_telegram_token_roundtrip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("tg-agent");
        agent.telegram_token = Some("123456:ABC-DEF".to_string());
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.telegram_token.as_deref(), Some("123456:ABC-DEF"));

        // Update to remove token
        let mut updated = loaded;
        updated.telegram_token = None;
        store.update_agent(&updated).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(reloaded.telegram_token.is_none());
    }

    #[test]
    fn test_agent_reasoning_effort_roundtrip() {
        use alms_core::config::ReasoningEffort;
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("reasoner");
        agent.reasoning_effort = Some(ReasoningEffort::High);
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.reasoning_effort, Some(ReasoningEffort::High));

        // Update to a different value.
        let mut updated = loaded;
        updated.reasoning_effort = Some(ReasoningEffort::Low);
        store.update_agent(&updated).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(reloaded.reasoning_effort, Some(ReasoningEffort::Low));

        // Each supported variant must survive round-trip.
        for variant in [
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            let mut a = reloaded.clone();
            a.reasoning_effort = Some(variant);
            store.update_agent(&a).unwrap();
            let r = store.load_agent_by_id(agent.id).unwrap().unwrap();
            assert_eq!(
                r.reasoning_effort,
                Some(variant),
                "variant {variant:?} did not round-trip"
            );
        }
    }

    #[test]
    fn test_agent_gemini_thinking_budget_roundtrip() {
        // Issue #794: per-agent Gemini thinking budget survives round-trip.
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("gemini-thinker");
        agent.gemini_thinking_budget = Some(16384);
        store.create_agent(&agent).unwrap();

        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.gemini_thinking_budget, Some(16384));

        // Explicit disable via `Some(0)` must round-trip as `Some(0)`.
        let mut updated = loaded;
        updated.gemini_thinking_budget = Some(0);
        store.update_agent(&updated).unwrap();
        let reloaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(reloaded.gemini_thinking_budget, Some(0));

        // Clearing to `None` (inherit server default) must round-trip.
        let mut cleared = reloaded;
        cleared.gemini_thinking_budget = None;
        store.update_agent(&cleared).unwrap();
        let final_state = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(final_state.gemini_thinking_budget, None);
    }

    #[test]
    fn test_agent_summary_provider_model_roundtrip() {
        // Issue #872: per-agent summary_provider / summary_model survive
        // create → load → update → load → clear → load. Mirrors the
        // round-trip shape of test_agent_gemini_thinking_budget_roundtrip.
        // Both fields are NULL in the default new_agent fixture so the
        // freshly-created record exercises the both-None path; subsequent
        // update_agent calls exercise the both-Some path; the final
        // clear-back-to-None step verifies the SQL UPDATE handles NULL
        // for the new columns 9 / 10.
        let store = SqliteStore::open_in_memory().unwrap();
        let mut agent = new_agent("summary-cfg");
        store.create_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(loaded.summary_provider.is_none());
        assert!(loaded.summary_model.is_none());

        // Set both together — pair-only invariant holds.
        agent = loaded;
        agent.summary_provider = Some("openrouter".into());
        agent.summary_model = Some("minimax/minimax-m2.7".into());
        store.update_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            loaded.summary_model.as_deref(),
            Some("minimax/minimax-m2.7")
        );

        // Update both to a different pair — both fields update together.
        agent = loaded;
        agent.summary_provider = Some("anthropic".into());
        agent.summary_model = Some("claude-haiku-4".into());
        store.update_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert_eq!(loaded.summary_provider.as_deref(), Some("anthropic"));
        assert_eq!(loaded.summary_model.as_deref(), Some("claude-haiku-4"));

        // Clear both — back to inheriting the server-level setting.
        agent = loaded;
        agent.summary_provider = None;
        agent.summary_model = None;
        store.update_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(loaded.summary_provider.is_none());
        assert!(loaded.summary_model.is_none());
    }

    #[test]
    fn test_agents_with_telegram() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut a1 = new_agent("with-tg");
        a1.telegram_token = Some("token1".to_string());
        store.create_agent(&a1).unwrap();

        let a2 = new_agent("no-tg");
        store.create_agent(&a2).unwrap();

        let mut a3 = new_agent("also-tg");
        a3.telegram_token = Some("token2".to_string());
        store.create_agent(&a3).unwrap();

        let tg_agents = store.agents_with_telegram().unwrap();
        assert_eq!(tg_agents.len(), 2);
        assert_eq!(tg_agents[0].name, "with-tg");
        assert_eq!(tg_agents[1].name, "also-tg");
    }
}
