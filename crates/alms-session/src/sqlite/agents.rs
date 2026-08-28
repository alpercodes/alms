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
              gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
              debug_mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
                agent.debug_mode as i32,
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
                  gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
                  debug_mode) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
                    agent.debug_mode as i32,
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
    ///
    /// **That immutability is load-bearing, not incidental.** DM `context_id`s
    /// embed the name at creation time, and the peer probe in
    /// [`Self::delete_agent`] reads "no agent by this name" as proof the peer
    /// is gone. Adding `name` here without migrating those `context_id`s in
    /// the same transaction turns that proof false and purges live peers' DM
    /// sessions -- see the precondition note on `classify_peer_presence`.
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
                 debug_mode = ?12, \
                 last_active = ?13 WHERE id = ?14",
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
                    agent.debug_mode as i32,
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
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
             debug_mode \
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
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
             debug_mode \
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
             gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
             debug_mode \
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
                 gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
                 debug_mode \
                 FROM agents ORDER BY created_at",
            )
            .map_err(|e| AlmsError::Runtime(format!("SQLite prepare agents: {e}")))?;

        let rows = stmt
            .query_map([], parse_agent_row)
            .map_err(|e| AlmsError::Runtime(format!("SQLite query agents: {e}")))?
            .filter_map(|r| match r {
                Ok(agent) => Some(agent),
                Err(e) => {
                    self.record_skipped_row(PersistenceTable::Agents, e);
                    None
                }
            })
            .collect();

        Ok(rows)
    }

    /// Test-only: drop the `agents` table so subsequent agent-CRUD calls
    /// fail at the `prepare()` step with `no such table: agents`.
    ///
    /// Used by `alms-gateway` integration tests to simulate the
    /// "SQLite temporarily failing" condition and verify the PATCH
    /// `/settings` fleet-budget validator fails closed (Codex P2 #1020).
    /// Marked `#[doc(hidden)]` because the method is a cross-crate test
    /// affordance — `#[cfg(test)]` would scope it to this crate's own
    /// test target only, and `alms-session` has no `test-helpers`
    /// feature surface to gate it behind.
    #[doc(hidden)]
    pub fn drop_agents_table_for_test(&self) -> AlmsResult<()> {
        let conn = self.conn.lock();
        conn.execute_batch("DROP TABLE agents;")
            .map_err(|e| AlmsError::Runtime(format!("SQLite drop agents (test): {e}")))?;
        Ok(())
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
                 gemini_thinking_budget, summary_provider, summary_model, worktree_mode, \
                 debug_mode \
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
                    self.record_skipped_row(PersistenceTable::Agents, e);
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

        // 0. Look up the agent's name so we can identify DM sessions this
        //    agent participated in (step 4b). DM sessions are owned by
        //    `AgentId::nil()` and identify participants only via the
        //    `context_id = "dm:<a>:<b>"` format, so we have to match by name
        //    rather than by foreign key.
        //
        //    #1246: this used to be a bare `.ok()`, which collapsed two
        //    unrelated outcomes into the same silent `None`. They are split
        //    here because only one of them is a fault:
        //
        //      - `QueryReturnedNoRows` — there is no such agent. Skipping the
        //        DM-cleanup branch is *correct*; step 6 reports `Ok(false)`
        //        and nothing is stranded. Not counted, not logged.
        //      - Any other error — the row exists but its `name` could not be
        //        read (NULL/non-text cell, or a SQL failure). Step 4b is
        //        skipped, so shared DM sessions whose participants are all
        //        gone survive as unreachable rows: nothing enumerates them and
        //        their `messages`/`audit_events`/`context_summaries` leak with
        //        no UI surface.
        //
        //    The second is the policy's third branch — the stranding is
        //    *additive*, nothing that should have survived is deleted — so it
        //    is quarantinable, but it must be counted and named rather than
        //    swallowed. Failing the delete instead would make the agent
        //    permanently undeletable, which is the #1236 pattern of a false
        //    belief disabling its own remedy. (The DM-cascade peer probe in
        //    step 4b rules the same way for the same reason.)
        //
        //    Note the tense in the `detail`. This fires inside the
        //    transaction, and steps 1-6 plus the commit can all still fail
        //    afterwards — in which case the counter has moved and nothing was
        //    deleted or stranded. The counter is a rate signal, not a ledger,
        //    and the row-skip sites below have the same property; the message
        //    must not claim more than that.
        let agent_name: Option<String> = match tx.query_row(
            "SELECT name FROM agents WHERE id = ?1",
            params![&id_str],
            |row| row.get::<_, String>(0),
        ) {
            Ok(name) => Some(name),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                self.record_degraded_field(
                    DegradedField::AgentsName,
                    format_args!(
                        "delete_agent {id_str}: unreadable agent name: {e}; skipping DM cleanup, \
                         so if this delete commits any shared DM session whose participants are \
                         all gone is left stranded unreachable"
                    ),
                );
                None
            }
        };

        // 1. Collect the sessions this delete owns.
        //
        // #1278 moved the `agent_id` half of a named subagent session's key
        // onto the *invoked* agent's registry id, which makes `WHERE agent_id
        // = ?1` the wrong ownership question for one session class. A row
        // `subagent_{P}_{R}` is agent P's history — P asked for the work, P's
        // `invoke_agent` result quotes it, and P's runs are its
        // `parent_run_id` — but since #1278 it is *filed* under R. A bare
        // `agent_id` cascade would therefore let "delete R" destroy P's
        // transcripts, runs and audit events. That is not the break #1278
        // accepted: that one was a single keying change at upgrade, whereas
        // `DELETE /agents/{id_or_name}` is a repeatable runtime operation,
        // and `docs/security-model.md` § 7 calls audit logging append-only.
        //
        // So ownership here reads out of the same place *authorization*
        // reads out of: the `context_id`'s embedded parent
        // (`parse_subagent_parent`, the same parse
        // `ReadSubagentSessionTool::check_subagent_session_access` uses).
        // The rule is one sentence — **a subagent session belongs to the
        // parent named in its `context_id`, never to the agent whose id it
        // happens to be filed under** — and it cuts both ways:
        //
        //   - 1a skips a subagent row filed under this agent but parented by
        //     someone else, and
        //   - 1b collects a subagent row parented by this agent whatever it
        //     is filed under.
        //
        // 1b also closes a leak that predates #1278: named subagent sessions
        // for an *unregistered* name (`AgentId::deterministic(P, name)`) and
        // ephemeral ones (`AgentId::new()`) are filed under ids no agent
        // holds, so no `delete_agent` call ever collected them and they
        // accumulated forever. They are P's history too, and now they go
        // when P goes.
        //
        // Self-invocation (`subagent_{A}_{A}`, filed under A and parented by
        // A) satisfies both halves; `seen` keeps the id set unique so step 2
        // does not run its child deletes twice.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut session_ids: Vec<String> = Vec::new();

        // 1a. Sessions filed under this agent, minus other parents' subagents.
        {
            let mut stmt = tx
                .prepare("SELECT id, context_id FROM sessions WHERE agent_id = ?1")
                .map_err(|e| AlmsError::Runtime(format!("SQLite prepare session query: {e}")))?;
            let rows = stmt
                .query_map(params![&id_str], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| AlmsError::Runtime(format!("SQLite query agent sessions: {e}")))?;
            for row in rows {
                // Third-branch site (see the reconciliation policy in
                // `docs/architecture.md`): a row dropped here is a session
                // that survives this delete whole — row, messages, runs and
                // tool-call rows — with no agent and no retry path:
                // orphaned, not lost. This is deliberately *more*
                // conservative than the pre-#1278 shape, which deleted the
                // session row by `agent_id` and left only its children
                // behind. An unreadable row is a row whose owner could not
                // be determined, and since #1278 "filed under this agent" no
                // longer implies "owned by this agent" — so the safe
                // disposition for an unknown owner is to keep all of it.
                // Failing the delete instead would make the agent
                // permanently undeletable, which is worse. Counted so the
                // leak is visible; see #1241.
                let (sid, ctx_id) = match row {
                    Ok(pair) => pair,
                    Err(e) => {
                        self.record_skipped_row(
                            PersistenceTable::Sessions,
                            format_args!("delete_agent {id_str}: unreadable session row: {e}"),
                        );
                        continue;
                    }
                };
                // A subagent context that names a *different* parent is that
                // parent's history; it merely ran on this agent.
                if let Some(parent) = alms_core::parse_subagent_parent(&ctx_id)
                    && parent != id
                {
                    continue;
                }
                if seen.insert(sid.clone()) {
                    session_ids.push(sid);
                }
            }
        }

        // 1b. Subagent sessions this agent spawned, wherever they are filed.
        //
        // `LIKE 'subagent%'` is a prefix filter only — the literal contains
        // no `_` or `%`, so it needs no ESCAPE clause — and it deliberately
        // over-matches: `parse_subagent_parent` below is the authority on
        // both the shape and the parent, so the context format is never
        // encoded a second time in SQL where it could drift. Unindexed, but
        // `delete_agent` is a rare operator action and the scan is bounded
        // by the subagent sessions in the table.
        {
            let mut stmt = tx
                .prepare("SELECT id, context_id FROM sessions WHERE context_id LIKE 'subagent%'")
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite prepare subagent session query: {e}"))
                })?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| AlmsError::Runtime(format!("SQLite query subagent sessions: {e}")))?;
            for row in rows {
                // Same third branch as 1a, and additive in the same way: a
                // subagent session this agent spawned survives the delete
                // with a dangling parent in its `context_id`. Nothing that
                // should have survived is deleted.
                let (sid, ctx_id) = match row {
                    Ok(pair) => pair,
                    Err(e) => {
                        self.record_skipped_row(
                            PersistenceTable::Sessions,
                            format_args!(
                                "delete_agent {id_str}: unreadable subagent session row: {e}"
                            ),
                        );
                        continue;
                    }
                };
                if alms_core::parse_subagent_parent(&ctx_id) == Some(id) && seen.insert(sid.clone())
                {
                    session_ids.push(sid);
                }
            }
        }

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

        // 3. Delete the sessions themselves.
        //
        // By id, not by `agent_id`: step 1 is now the single authority on
        // which sessions this delete owns, and a bulk `WHERE agent_id = ?1`
        // would silently re-delete the foreign-parent subagent rows 1a just
        // decided to spare. The set is exactly the one step 2 cleaned, so
        // every FK dependent is already gone.
        for sid in &session_ids {
            tx.execute("DELETE FROM sessions WHERE id = ?1", params![sid])
                .map_err(|e| AlmsError::Runtime(format!("SQLite delete agent sessions: {e}")))?;
        }

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
        //
        // #1278: each of these is a bare `agent_id`/`from_agent` sweep over
        // rows whose session survived step 3, and one class of surviving
        // session is now *another parent's* subagent transcript that this
        // agent merely executed (see step 1). Its `runs` rows carry
        // `agent_id = <this agent>` and a `parent_run_id` pointing into the
        // parent's own history, so an unqualified sweep would strip the run
        // and tool-call trail out of a transcript step 1 deliberately
        // spared — the same cross-agent destruction, one table down.
        //
        // The `NOT IN` subquery is exact rather than approximate, and
        // without re-parsing the context format: step 3 has already deleted
        // every subagent session this delete owns, so any `subagent%`
        // session still standing at this point is by construction parented
        // by somebody else. `session_id IS NULL` is admitted explicitly
        // because `run_tool_calls.session_id` is nullable and SQL's `NULL
        // NOT IN (...)` is NULL, which would otherwise start sparing
        // session-less tool calls that have nothing to do with subagents.
        const NOT_ON_A_FOREIGN_SUBAGENT_SESSION: &str = "session_id IS NULL OR session_id NOT IN \
             (SELECT id FROM sessions WHERE context_id LIKE 'subagent%')";
        tx.execute(
            &format!(
                "DELETE FROM run_tool_calls WHERE from_agent = ?1 \
                 AND ({NOT_ON_A_FOREIGN_SUBAGENT_SESSION})"
            ),
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete dm-orphan run_tool_calls: {e}")))?;
        tx.execute(
            &format!(
                "DELETE FROM session_summaries WHERE agent_id = ?1 \
                 AND ({NOT_ON_A_FOREIGN_SUBAGENT_SESSION})"
            ),
            params![&id_str],
        )
        .map_err(|e| {
            AlmsError::Runtime(format!("SQLite delete dm-orphan session_summaries: {e}"))
        })?;
        tx.execute(
            &format!(
                "DELETE FROM runs WHERE agent_id = ?1 \
                 AND ({NOT_ON_A_FOREIGN_SUBAGENT_SESSION})"
            ),
            params![&id_str],
        )
        .map_err(|e| AlmsError::Runtime(format!("SQLite delete dm-orphan runs: {e}")))?;

        // 4b. Cascade-delete shared DM sessions whose other participant is
        //     also gone (#1002).
        //
        //     Step 4 cleared this agent's *contribution* rows but deliberately
        //     left the shared `sessions` row alone so a surviving DM partner
        //     could keep using it. If neither partner remains, that row is
        //     unreachable: nothing enumerates it (no agent owns it) and
        //     non-orphan-class rows still keyed on it (`messages`,
        //     `audit_events`, `context_summaries`) leak with no UI surface.
        //
        //     Strategy: parse `context_id = "dm:<a>:<b>"` to find the peer
        //     name (one query against `sessions`, in-memory parse via
        //     `alms_core::dm_participants`). If the peer is *not* present in
        //     the `agents` table at this point in the transaction, the DM is
        //     unreachable and the row plus every dependent table must go.
        //     The agent row itself is still live (step 6 deletes it), so
        //     we look up the *peer*, not the deleting agent.
        if let Some(name) = &agent_name {
            // Pull every DM session that mentions this agent. The
            // `agent_id = nil` filter narrows to the shared-DM class;
            // the `LIKE 'dm:%'` keeps the SQL honest if a non-DM
            // session ever slips into the nil-owner bucket.
            //
            // FORWARD NOTE: this approach (parse `context_id` per
            // session-type, probe `agents` per participant) only scales
            // while DM is the *only* shared-session class. If group
            // chats / channels / multi-party rooms ever land under the
            // nil-owner bucket, the per-class parser branch here will
            // need a fan-out, and an aggregate scan over rows (option 2
            // from the #1002 design discussion) likely becomes the
            // simpler shape. Revisit when the second shared class lands.
            let dm_candidates: Vec<(String, String)> = {
                let nil_id_str = AgentId::nil().0.to_string();
                let mut stmt = tx
                    .prepare(
                        "SELECT id, context_id FROM sessions \
                         WHERE agent_id = ?1 AND context_id LIKE 'dm:%'",
                    )
                    .map_err(|e| {
                        AlmsError::Runtime(format!("SQLite prepare dm-candidate query: {e}"))
                    })?;
                stmt.query_map(params![&nil_id_str], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| AlmsError::Runtime(format!("SQLite query dm-candidates: {e}")))?
                // Third-branch site (see the reconciliation policy in
                // `docs/architecture.md`): a row dropped here is a DM session
                // this agent participated in that survives the delete with a
                // now-dangling participant in its `context_id` — orphaned, not
                // lost. Failing the delete instead would make the agent
                // permanently undeletable, which is worse. Counted so the leak
                // is visible; see #1241.
                .filter_map(|r| match r {
                    Ok(candidate) => Some(candidate),
                    Err(e) => {
                        self.record_skipped_row(
                            PersistenceTable::Sessions,
                            format_args!("delete_agent {id_str}: unreadable dm candidate: {e}"),
                        );
                        None
                    }
                })
                .collect()
            };

            // Classify each DM session as "unreachable after this delete"
            // by parsing the context_id, identifying the peer, and probing
            // `agents` for the peer's existence. The deleting agent is
            // still live in the `agents` table at this point in the
            // transaction (step 6 removes it), so the peer lookup is the
            // only thing that matters: peer-absent ⇒ both-gone ⇒ purge.
            //
            // #1246: the probe below is the one fallback in this function
            // that points at *deletion* rather than at stranding, so it gets
            // the strictest rule in the file: **only a peer proven absent may
            // purge.** `peer_exists = false` sends the session to `to_purge`,
            // and the purge deletes the session row plus every dependent
            // table keyed on it — so a wrong `false` is "delete a DM session
            // whose peer is alive and still using it": data that should have
            // survived, gone. The reconciliation policy puts that outside the
            // quarantinable class entirely ("durable garbage is tolerable
            // only because it is still repairable by hand, and data that is
            // gone is not").
            //
            // Two distinct outcomes look exactly like "absent" from the
            // probe's return value and are not, so [`classify_peer_presence`]
            // splits them out. Neither purges; both leave the DM session
            // stranded, which is additive and lands in the policy's third
            // branch precisely as the two sibling drop sites in this function
            // do. Deliberately *not* fatal: failing the delete would make
            // every agent that has ever had a DM undeletable for as long as
            // the fault lasts, which is the same #1236 pattern — a false
            // belief disabling its own remedy — that step 0's agent-name
            // lookup refuses forty lines up. Two safe dispositions were
            // available here and the cheaper one is also the consistent one.
            //
            // `agent_names_all_readable` is consulted only when the probe
            // found nothing, and is cached across candidates: it is a full
            // scan of `agents`, and the all-present happy path never pays it.
            let mut names_all_readable: Option<bool> = None;
            let mut to_purge: Vec<String> = Vec::new();
            for (sid, ctx_id) in &dm_candidates {
                // `dm_peer` returns `None` when:
                //   - `ctx_id` does not match the `"dm:<a>:<b>"` shape
                //     (some other pair shares the nil-owner bucket -- not
                //     our cleanup to do), or
                //   - neither participant matches `name` (same).
                // Either way we skip; only `Some(peer)` is actionable.
                let Some(peer) = alms_core::dm_peer(ctx_id, name) else {
                    continue;
                };

                let probe = tx.query_row(
                    "SELECT 1 FROM agents WHERE name = ?1",
                    params![peer],
                    |_| Ok(true),
                );
                let all_readable = probe.is_ok()
                    || *names_all_readable.get_or_insert_with(|| agent_names_all_readable(&tx));

                match classify_peer_presence(&probe, all_readable) {
                    PeerPresence::Present => {}
                    PeerPresence::ProvenAbsent => to_purge.push(sid.clone()),
                    PeerPresence::UnreadablePeerName => {
                        self.record_degraded_field(
                            DegradedField::AgentsName,
                            format_args!(
                                "delete_agent {id_str}: peer {peer:?} of DM session {sid} could \
                                 not be probed because at least one agents.name cell could not \
                                 be proven readable; keeping the session rather than risk \
                                 purging a live peer's DM, so it is stranded if the peer \
                                 really is gone"
                            ),
                        );
                    }
                    PeerPresence::ProbeFailed => {
                        // Not an `agents.name` degradation — that column was
                        // never reached. What could not be classified is the
                        // DM *session* row, and the consequence matches the
                        // two sibling skips in this function exactly: it
                        // survives the delete with no cleanup.
                        if let Err(e) = &probe {
                            self.record_skipped_row(
                                PersistenceTable::Sessions,
                                format_args!(
                                    "delete_agent {id_str}: dm-cascade peer probe for session \
                                     {sid} failed ({e}); keeping the session rather than purge on \
                                     an unproven absence"
                                ),
                            );
                        }
                    }
                }
            }

            // Cascade-delete each unreachable DM session and every
            // dependent table keyed on it. Same FK-respecting order as
            // step 2 above: child tables first, sessions row last.
            for sid in &to_purge {
                tx.execute(
                    "DELETE FROM context_summaries WHERE session_id = ?1",
                    params![sid],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete dm-cascade summaries: {e}"))
                })?;
                tx.execute(
                    "DELETE FROM session_summaries WHERE session_id = ?1",
                    params![sid],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete dm-cascade session_summaries: {e}"))
                })?;
                tx.execute(
                    "DELETE FROM audit_events WHERE session_id = ?1",
                    params![sid],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete dm-cascade audit_events: {e}"))
                })?;
                tx.execute("DELETE FROM messages WHERE session_id = ?1", params![sid])
                    .map_err(|e| {
                        AlmsError::Runtime(format!("SQLite delete dm-cascade messages: {e}"))
                    })?;
                // Defensive: in the normal flow this subquery is empty by
                // construction -- step 4 above already cleared every
                // `runs WHERE agent_id = <deleted>` row, and the peer's
                // runs on this DM session were cleared by step 4 of the
                // peer's own earlier `delete_agent` call (peer-absent is
                // the trigger for being in `to_purge`). So nothing matches
                // `runs.session_id = ?1` at this point. We keep the DELETE
                // anyway: cost is one empty subquery inside the same
                // transaction, and it remains correct if step 4's ordering
                // is ever rearranged or a future class of `run_tool_calls`
                // row lands here under a `from_agent` we did not enumerate.
                tx.execute(
                    "DELETE FROM run_tool_calls WHERE run_id IN \
                     (SELECT run_id FROM runs WHERE session_id = ?1)",
                    params![sid],
                )
                .map_err(|e| {
                    AlmsError::Runtime(format!("SQLite delete dm-cascade run_tool_calls: {e}"))
                })?;
                tx.execute("DELETE FROM runs WHERE session_id = ?1", params![sid])
                    .map_err(|e| {
                        AlmsError::Runtime(format!("SQLite delete dm-cascade runs: {e}"))
                    })?;
                tx.execute("DELETE FROM sessions WHERE id = ?1", params![sid])
                    .map_err(|e| {
                        AlmsError::Runtime(format!("SQLite delete dm-cascade sessions: {e}"))
                    })?;
            }
        }

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

/// What the DM-cascade peer probe in [`SqliteStore::delete_agent`] was able to
/// *prove* about the peer agent's existence (#1246).
///
/// The purge it feeds deletes a session row and every table keyed on it, so
/// the only answer allowed to reach `to_purge` is [`Self::ProvenAbsent`]. The
/// other two `Err` shapes are the ones a reader is likely to collapse back
/// into "absent"; they are named separately so that collapsing them requires
/// deleting a variant rather than deleting a `match` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerPresence {
    /// A row with that name exists. Keep the session.
    Present,
    /// The probe matched nothing *and* every `agents.name` cell in the table
    /// is readable text, so "no match" really does mean "no such agent".
    ProvenAbsent,
    /// The probe matched nothing, but at least one `agents.name` cell is not
    /// readable text. SQLite never compares a BLOB equal to a TEXT parameter
    /// (and column affinity does not convert an already-stored BLOB), and a
    /// NULL is equal to nothing at all — so a **live** peer whose own name
    /// cell is corrupt is indistinguishable from an absent one. Purging on
    /// that is the exact data loss this site exists to prevent, reached
    /// through the one branch that used to be trusted.
    UnreadablePeerName,
    /// The probe statement itself failed (I/O, corruption, a busy database).
    ProbeFailed,
}

/// Map a peer-probe result onto what it actually proves.
///
/// Split out as a pure function so the **polarity** is pinned by tests rather
/// than by the paragraph at the call site: the failure mode this guards is a
/// future edit collapsing every `Err` back to "absent", which no end-to-end
/// test can catch without fault injection.
///
/// `agent_names_all_readable` is the answer from [`agent_names_all_readable`],
/// and is only meaningful when the probe found nothing.
///
/// # Precondition: `agents.name` is write-once
///
/// [`PeerPresence::ProvenAbsent`] is a proof only while an agent's name never
/// changes. `dm:<a>:<b>` context ids embed the participants' names **at
/// creation time** and are never rewritten, so this probe really asks "is
/// there an agent still named what this peer was called back then?". Today
/// that is the same question as "does this peer still exist", because
/// [`SqliteStore::create_agent`] is the only writer of `agents.name` and
/// [`SqliteStore::update_agent`]'s `SET` list omits it.
///
/// Add a rename path and the two questions come apart: a renamed but **live**
/// peer stops matching its own DM context ids, every one of its DM sessions
/// classifies as `ProvenAbsent`, and the caller purges them along with every
/// message in them -- the exact loss this site exists to prevent, reached
/// through the one branch it trusts. A rename feature must therefore rewrite
/// the affected `context_id`s in the same transaction, or key DM sessions on
/// [`AgentId`] instead of on the name. Relaxing the `SET` list alone is not
/// enough.
fn classify_peer_presence(
    probe: &rusqlite::Result<bool>,
    agent_names_all_readable: bool,
) -> PeerPresence {
    match probe {
        Ok(_) => PeerPresence::Present,
        Err(rusqlite::Error::QueryReturnedNoRows) if agent_names_all_readable => {
            PeerPresence::ProvenAbsent
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => PeerPresence::UnreadablePeerName,
        Err(_) => PeerPresence::ProbeFailed,
    }
}

/// `true` when every `agents.name` cell is readable text, which is the
/// precondition that makes a name-keyed "no match" trustworthy as "no such
/// agent" (#1246).
///
/// **Fails closed.** If the check itself cannot run we report `false`: the
/// cost of a wrong `false` is a stranded DM session, additive and repairable
/// by hand; the cost of a wrong `true` is a deleted one, which is not.
///
/// This is deliberately table-wide rather than peer-specific — a corrupt name
/// cannot be looked up by name, which is the whole problem. One bad cell
/// therefore suppresses DM purging for the whole delete. That over-triggers in
/// the safe direction, and the condition is itself a counted, remediated fault.
fn agent_names_all_readable(tx: &rusqlite::Transaction<'_>) -> bool {
    match tx.query_row(
        "SELECT 1 FROM agents WHERE typeof(name) <> 'text' LIMIT 1",
        [],
        |_| Ok(()),
    ) {
        Ok(()) => false,
        Err(rusqlite::Error::QueryReturnedNoRows) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{corrupt_with_sql, new_message};
    use super::super::*;
    use super::{PeerPresence, agent_names_all_readable, classify_peer_presence};
    use crate::types::Session;
    use alms_core::job::{Job, JobSchedule};
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
            debug_mode: false,
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
        let doomed_job = Job::new(
            doomed.id,
            "doomed job".to_string(),
            JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            None,
        );
        let survivor_job = Job::new(
            survivor.id,
            "survivor job".to_string(),
            JobSchedule::Once {
                run_at: chrono::Utc::now(),
            },
            None,
        );
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
                ds.id,
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
                ss.id,
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
                ds.id,
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
                dm.id,
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
                dm.id,
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
                dm.id,
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
    fn test_delete_agent_preserves_dm_session_when_peer_still_alive() {
        // Regression guard for #1002: deleting one DM partner while the
        // other is still alive must NOT touch the shared DM session row.
        // This preserves the post-#1000 single-partner-delete behaviour
        // and rules out the #1002 cascade from over-firing.
        let store = SqliteStore::open_in_memory().unwrap();

        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();

        // Seed each side's #1000 orphan-class rows so we can also assert
        // alice's contributions are gone (existing behaviour preserved).
        let a_run = Run::new(dm.id, alice.id, "ping".to_string());
        let a_run_id = a_run.run_id;
        store.save_run(&a_run).unwrap();
        store
            .save_tool_call(
                a_run_id,
                dm.id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("send_message".to_string()),
                    tool_id: Some("call_a0".to_string()),
                    params: Some(r#"{"to":"bob","text":"hi"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(alice.id.0.to_string()),
                },
            )
            .unwrap();
        store
            .upsert_session_summary(alice.id, dm.id, "alice's view", Some(a_run_id), None)
            .unwrap();

        let b_run = Run::new(dm.id, bob.id, "pong".to_string());
        store.save_run(&b_run).unwrap();
        store
            .upsert_session_summary(bob.id, dm.id, "bob's view", Some(b_run.run_id), None)
            .unwrap();

        // Delete alice. Bob is still alive -- the DM session must survive
        // and bob's rows on it must remain intact.
        assert!(store.delete_agent(alice.id).unwrap());

        // Pull every assertion off the connection lock in a single scope
        // so the subsequent `load_session_summary` call (which also takes
        // the lock) does not deadlock.
        {
            let conn = store.conn.lock();
            let dm_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![dm.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                dm_count, 1,
                "DM session must survive while bob is still alive (post-#1000 behaviour)"
            );

            // Alice's #1000 orphan-class rows on the DM session are gone.
            let alice_runs: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                    params![alice.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(alice_runs, 0, "alice's runs cleared by #1000 step 4");

            // Bob's rows on the DM session are intact.
            let bob_runs: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                    params![bob.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bob_runs, 1, "bob's DM run must remain");
        }

        // Lock dropped -- safe to call store helpers that take it again.
        let bob_summary = store
            .load_session_summary(bob.id, dm.id)
            .unwrap()
            .expect("bob's session_summary on the DM must remain");
        assert_eq!(bob_summary.summary, "bob's view");
    }

    #[test]
    fn test_delete_agent_cascades_dm_session_when_both_partners_gone() {
        // Issue #1002: when both DM partners are deleted in turn, the
        // shared DM session row leaks because it's owned by
        // `AgentId::nil()` and neither agent's delete enumerates it via
        // `WHERE sessions.agent_id = ?1`. Pre-fix, deleting alice then
        // bob left a dangling `sessions` row plus every non-orphan-class
        // table keyed on it (`messages`, `audit_events`,
        // `context_summaries`).
        //
        // Post-fix: when the second partner is deleted, the cascade in
        // step 4b notices the first partner is already gone (peer
        // lookup in the agents table fails) and purges the DM session
        // plus every dependent row.
        let store = SqliteStore::open_in_memory().unwrap();

        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        // Shared DM session under the nil-agent sentinel.
        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();

        // Seed every dependent table keyed on the DM session: a
        // `messages` row (non-orphan-class; only the per-session sweep
        // can clear it), an `audit_events` row, a `context_summaries`
        // row, alice's run + tool call + session_summary, bob's
        // run + session_summary.
        store
            .save_message(dm.id, &new_message("hello bob"))
            .unwrap();
        let dm_audit = AuditEvent::allow(
            dm.id,
            "send_message",
            serde_json::json!({"to": "bob"}),
            serde_json::json!("ok"),
        );
        store.save_audit(&dm_audit).unwrap();
        let dm_summary = ContextSummary {
            text: "DM context summary".to_string(),
            messages_covered: 1,
            updated_at: Some(Timestamp::now()),
        };
        store.save_summary(dm.id, &dm_summary).unwrap();

        let a_run = Run::new(dm.id, alice.id, "ping".to_string());
        let a_run_id = a_run.run_id;
        store.save_run(&a_run).unwrap();
        store
            .save_tool_call(
                a_run_id,
                dm.id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("send_message".to_string()),
                    tool_id: Some("call_a0".to_string()),
                    params: Some(r#"{"to":"bob","text":"hi"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(alice.id.0.to_string()),
                },
            )
            .unwrap();
        store
            .upsert_session_summary(alice.id, dm.id, "alice's view", Some(a_run_id), None)
            .unwrap();

        let b_run = Run::new(dm.id, bob.id, "pong".to_string());
        store.save_run(&b_run).unwrap();
        store
            .upsert_session_summary(bob.id, dm.id, "bob's view", Some(b_run.run_id), None)
            .unwrap();

        // Step 1: delete alice. DM session must still exist (bob alive).
        assert!(store.delete_agent(alice.id).unwrap());
        {
            let conn = store.conn.lock();
            let dm_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![dm.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                dm_count, 1,
                "after deleting alice, DM session must survive while bob is still alive"
            );
        }

        // Step 2: delete bob. Now both participants are gone. The DM
        // session row AND every dependent row keyed on it must be gone.
        assert!(store.delete_agent(bob.id).unwrap());

        let conn = store.conn.lock();
        let dm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dm_count, 0,
            "after both partners deleted, the shared DM session row must be gone (#1002)"
        );

        // Every dependent table keyed on the DM session_id must be empty.
        let dm_id_str = dm.id.0.to_string();
        for table in [
            "messages",
            "audit_events",
            "context_summaries",
            "session_summaries",
            "runs",
        ] {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                    params![&dm_id_str],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                n, 0,
                "after #1002 cascade, no rows in `{table}` may reference the deleted DM session_id"
            );
        }

        // Tool calls (keyed on run_id, not session_id) are gone too --
        // both runs lived on the DM session and were cleared with it.
        let n_calls: i64 = conn
            .query_row("SELECT COUNT(*) FROM run_tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n_calls, 0,
            "all run_tool_calls keyed on DM runs must be gone after #1002 cascade"
        );
    }

    #[test]
    fn test_delete_agent_only_cascades_dms_where_peer_is_also_gone() {
        // Issue #1002 edge case: alice has DMs with both bob and
        // charlie. Deleting alice while bob and charlie are both alive
        // must NOT cascade-delete either DM session -- both peers are
        // still live participants. Only alice's per-agent #1000 orphan
        // rows go.
        //
        // This guards against the cascade over-firing on
        // multi-partner agents.
        let store = SqliteStore::open_in_memory().unwrap();

        let alice = new_agent("alice");
        let bob = new_agent("bob");
        let charlie = new_agent("charlie");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();
        store.create_agent(&charlie).unwrap();

        let dm_ab = Session::new(AgentId::nil(), "dm:alice:bob");
        let dm_ac = Session::new(AgentId::nil(), "dm:alice:charlie");
        store.save_session(&dm_ab).unwrap();
        store.save_session(&dm_ac).unwrap();

        // Seed alice's per-perspective rows on both DMs so step 4 has
        // something to clear and we can confirm alice-only cleanup ran.
        let a_ab_run = Run::new(dm_ab.id, alice.id, "hi bob".to_string());
        store.save_run(&a_ab_run).unwrap();
        let a_ac_run = Run::new(dm_ac.id, alice.id, "hi charlie".to_string());
        store.save_run(&a_ac_run).unwrap();

        // Seed bob's and charlie's rows on their respective DMs so we
        // can confirm they survive untouched.
        let b_run = Run::new(dm_ab.id, bob.id, "hi alice".to_string());
        store.save_run(&b_run).unwrap();
        let c_run = Run::new(dm_ac.id, charlie.id, "hi alice".to_string());
        store.save_run(&c_run).unwrap();

        // Delete alice. Both bob and charlie still exist as DM peers.
        assert!(store.delete_agent(alice.id).unwrap());

        let conn = store.conn.lock();

        // Both DM session rows must survive -- the peers are still live.
        for sid in [dm_ab.id, dm_ac.id] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![sid.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "DM session {sid} must survive: peer is still alive");
        }

        // Alice's per-agent rows are gone (existing #1000 behaviour).
        let alice_runs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                params![alice.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alice_runs, 0, "alice's runs cleared by step 4");

        // Bob's and charlie's DM runs remain intact.
        for (peer_id, peer_name) in [(bob.id, "bob"), (charlie.id, "charlie")] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE agent_id = ?1",
                    params![peer_id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{peer_name}'s DM run must remain intact");
        }
    }

    #[test]
    fn test_delete_agent_recreating_deleted_agent_does_not_resurrect_old_dm() {
        // Issue #1002 edge case: after both alice and bob are deleted,
        // the DM session is purged. Creating a fresh agent also named
        // `alice` must NOT inherit the old DM session row -- there is
        // nothing to inherit, by construction. This is the intended
        // semantics: a recreated agent starts with a clean slate, and
        // any DM with the recreated alice would create a new session
        // row on next contact.
        let store = SqliteStore::open_in_memory().unwrap();

        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();
        store
            .save_message(dm.id, &new_message("history we don't want to resurrect"))
            .unwrap();

        // Delete alice then bob -- the DM session goes away (#1002).
        assert!(store.delete_agent(alice.id).unwrap());
        assert!(store.delete_agent(bob.id).unwrap());

        {
            let conn = store.conn.lock();
            let dm_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![dm.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(dm_count, 0, "DM session purged after both partners deleted");
        }

        // Recreate `alice` with a fresh AgentId. The old DM row is
        // already gone; the new alice does not see the old DM history.
        let new_alice = new_agent("alice");
        assert_ne!(
            new_alice.id, alice.id,
            "freshly created agent must have a distinct UUID"
        );
        store.create_agent(&new_alice).unwrap();

        let conn = store.conn.lock();
        let still_zero: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            still_zero, 0,
            "recreating alice must not resurrect the old DM session row"
        );

        // And no stale messages keyed on the old DM session_id remain.
        let stale_msgs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale_msgs, 0, "old DM messages must not survive recreate");
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
    fn test_agent_debug_mode_roundtrip() {
        // Issue #1003: per-agent debug_mode survives create -> load ->
        // update -> load -> update -> load. Default for fresh agents is
        // `false` (matches the schema's `NOT NULL DEFAULT 0`); flipping
        // to `true` and back to `false` both round-trip cleanly through
        // the SQLite INTEGER column.
        let store = SqliteStore::open_in_memory().unwrap();

        let mut agent = new_agent("debugger");
        store.create_agent(&agent).unwrap();

        // Default for fresh agents is `false`.
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            !loaded.debug_mode,
            "fresh agents must default to debug_mode = false"
        );

        // Flip to `true` and verify it round-trips.
        agent = loaded;
        agent.debug_mode = true;
        store.update_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            loaded.debug_mode,
            "debug_mode = true must round-trip through update_agent"
        );

        // Flip back to `false`.
        agent = loaded;
        agent.debug_mode = false;
        store.update_agent(&agent).unwrap();
        let loaded = store.load_agent_by_id(agent.id).unwrap().unwrap();
        assert!(
            !loaded.debug_mode,
            "debug_mode = false must round-trip through update_agent"
        );
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

    /// #1241: a row the loader cannot parse is dropped — the pre-existing and
    /// correct policy — but the drop must be counted, not just logged.
    #[test]
    fn corrupt_agent_row_is_dropped_and_counted() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_agent(&new_agent("atlas")).unwrap();
        assert_eq!(store.list_agents().unwrap().len(), 1);
        assert_eq!(store.rows_skipped_total(), 0);

        // `created_at` is parsed as RFC-3339 by `parse_agent_row`, so this is
        // a row that exists on disk and cannot become an `AgentRecord`.
        corrupt_with_sql(&store, "UPDATE agents SET created_at = 'not-a-timestamp'");

        assert!(
            store.list_agents().unwrap().is_empty(),
            "an unparseable agent must not be projected into the registry"
        );
        assert_eq!(store.rows_skipped_for(PersistenceTable::Agents), 1);
        assert_eq!(store.rows_skipped_total(), 1);
        let by_table = store.rows_skipped_by_table();
        assert_eq!(by_table["agents"], 1);
        assert_eq!(by_table["sessions"], 0, "skips are attributed per table");

        // These loaders run on every read, not once at startup, so the same
        // corrupt row is counted again by the next caller.
        assert!(store.list_agents().unwrap().is_empty());
        assert_eq!(store.rows_skipped_for(PersistenceTable::Agents), 2);

        // The counters describe the database, not the handle: clones of the
        // store share the connection and must share the accounting.
        assert_eq!(store.clone().rows_skipped_total(), 2);
    }

    /// #1246: `delete_agent`'s name lookup used to be a bare `.ok()`, so an
    /// unreadable `agents.name` silently skipped the whole DM-cleanup branch.
    /// The delete still has to succeed — failing it would make the agent
    /// permanently undeletable — but the stranding it causes must be counted
    /// and logged rather than swallowed.
    #[test]
    fn delete_agent_counts_an_unreadable_name_and_strands_the_dm_session() {
        let store = SqliteStore::open_in_memory().unwrap();
        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();

        // Delete alice normally: bob is still alive, so the DM survives.
        assert!(store.delete_agent(alice.id).unwrap());
        assert_eq!(store.fields_degraded_total(), 0);

        // Now make bob's name unreadable as text. `name` is `TEXT NOT NULL`,
        // but SQLite is dynamically typed, so a BLOB sits happily in the cell
        // and `row.get::<_, String>` cannot convert it. This is the shape a
        // hand-edited or partially-corrupted database produces.
        corrupt_with_sql(
            &store,
            &format!(
                "UPDATE agents SET name = X'DEADBEEF' WHERE id = '{}'",
                bob.id.0
            ),
        );

        // The delete still succeeds...
        assert!(
            store.delete_agent(bob.id).unwrap(),
            "an unreadable name must not make the agent undeletable"
        );

        // ...but step 4b was skipped, so the DM session both of whose
        // participants are now gone survives as an unreachable row. That is
        // the leak the counter exists to make visible.
        {
            let conn = store.conn.lock();
            let dm_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![dm.id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                dm_count, 1,
                "the DM session is stranded — this is the degradation, not a bug in the test"
            );
        }

        assert_eq!(store.fields_degraded_for(DegradedField::AgentsName), 1);
        assert_eq!(store.fields_degraded_total(), 1);
        assert_eq!(
            store.rows_skipped_total(),
            0,
            "a skipped cleanup branch is not a skipped row"
        );
    }

    /// The other half of the split #1246 introduced: "there is no such agent"
    /// is `QueryReturnedNoRows`, not a fault. Skipping DM cleanup is *correct*
    /// there — there is nothing to clean up — so it must not be counted or it
    /// would make `persistence_fields_degraded_total` climb on every delete of
    /// an already-deleted agent.
    #[test]
    fn delete_agent_does_not_count_a_missing_agent_as_a_degradation() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        assert!(store.delete_agent(agent.id).unwrap());
        // Second delete: the row is already gone.
        assert!(!store.delete_agent(agent.id).unwrap());
        // And an id that never existed.
        assert!(!store.delete_agent(AgentId::new()).unwrap());

        assert_eq!(
            store.fields_degraded_total(),
            0,
            "an absent agent is the normal path, not a degraded field"
        );
    }

    /// The DM-cascade peer probe must not treat an error as "peer absent":
    /// that fallback *deletes* a live DM session, which the reconciliation
    /// policy explicitly puts outside the quarantinable class. This test pins
    /// the benign half of the split — a peer that genuinely does not exist is
    /// still `QueryReturnedNoRows` and must still purge — so the stricter
    /// error handling cannot silently disable the #1002 cascade.
    #[test]
    fn dm_cascade_still_purges_when_the_peer_is_genuinely_absent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();
        store.save_message(dm.id, &new_message("hi bob")).unwrap();

        assert!(store.delete_agent(alice.id).unwrap());
        assert!(store.delete_agent(bob.id).unwrap());

        let conn = store.conn.lock();
        let dm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dm_count, 0,
            "both participants gone: the DM session must still cascade"
        );
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 0);
    }

    /// The residual `QueryReturnedNoRows` does not cover, and the reason
    /// "only `QueryReturnedNoRows` may mean absent" is not by itself a safety
    /// guarantee for a probe keyed on `name`.
    ///
    /// If the **peer's** own `agents.name` cell is a BLOB — the exact
    /// corruption `delete_agent_counts_an_unreadable_name_and_strands_the_dm_session`
    /// builds, one row over — then `SELECT 1 FROM agents WHERE name = ?1`
    /// with a TEXT parameter matches nothing: SQLite never compares a BLOB
    /// equal to a TEXT value, and TEXT column affinity does not convert an
    /// already-stored BLOB. The probe therefore reports `QueryReturnedNoRows`
    /// for a peer that is alive and still using the session.
    ///
    /// Before #1246's follow-up that purged the DM session and every message
    /// in it. The session must survive.
    #[test]
    fn dm_cascade_does_not_purge_when_the_peer_name_is_unreadable() {
        let store = SqliteStore::open_in_memory().unwrap();
        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();

        let dm = Session::new(AgentId::nil(), "dm:alice:bob");
        store.save_session(&dm).unwrap();
        store.save_message(dm.id, &new_message("hi bob")).unwrap();

        // Corrupt the *peer's* name, not the deleted agent's. Alice's own
        // name still reads fine, so step 0 succeeds and the DM-cleanup branch
        // runs in full — this exercises the probe, not the step-0 skip.
        corrupt_with_sql(
            &store,
            &format!(
                "UPDATE agents SET name = X'DEADBEEF' WHERE id = '{}'",
                bob.id.0
            ),
        );

        assert!(store.delete_agent(alice.id).unwrap());

        let conn = store.conn.lock();
        let dm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dm_count, 1,
            "bob is alive: an unreadable peer name must never be read as an absent peer"
        );
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![dm.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 1, "the DM's messages must survive with it");
        drop(conn);

        assert_eq!(
            store.fields_degraded_for(DegradedField::AgentsName),
            1,
            "the unprovable probe is counted as the agents.name fault it is"
        );
        assert_eq!(
            store.rows_skipped_total(),
            0,
            "nothing failed to parse -- this is a degradation, not a row skip"
        );
    }

    /// Failing the delete is *not* the disposition here. An unprovable peer
    /// leaves the DM session alone and lets the delete finish, so a corrupt
    /// `agents` table cannot make every agent that has ever had a DM
    /// undeletable — the same #1236 argument step 0 makes for its own lookup.
    #[test]
    fn an_unprovable_peer_does_not_make_the_agent_undeletable() {
        let store = SqliteStore::open_in_memory().unwrap();
        let alice = new_agent("alice");
        let bob = new_agent("bob");
        store.create_agent(&alice).unwrap();
        store.create_agent(&bob).unwrap();
        store
            .save_session(&Session::new(AgentId::nil(), "dm:alice:bob"))
            .unwrap();
        corrupt_with_sql(
            &store,
            &format!(
                "UPDATE agents SET name = X'DEADBEEF' WHERE id = '{}'",
                bob.id.0
            ),
        );

        assert!(
            store.delete_agent(alice.id).unwrap(),
            "the delete must still commit"
        );
        assert!(
            store.load_agent_by_id(alice.id).unwrap().is_none(),
            "and alice must actually be gone, not rolled back"
        );
    }

    /// #1246: the polarity of the peer probe is the thing that must not
    /// regress, and the destructive arm is not reachable from a unit test
    /// without fault injection. So the mapping is pinned directly instead of
    /// by prose: restoring `.unwrap_or(false)`, or folding any `Err` back into
    /// "absent", has to break one of these.
    #[test]
    fn only_a_proven_absent_peer_may_purge() {
        assert_eq!(
            classify_peer_presence(&Ok(true), true),
            PeerPresence::Present
        );
        // A readable-names table makes "no match" trustworthy.
        assert_eq!(
            classify_peer_presence(&Err(rusqlite::Error::QueryReturnedNoRows), true),
            PeerPresence::ProvenAbsent
        );
        // ...and an unreadable name anywhere in the table takes that away.
        assert_eq!(
            classify_peer_presence(&Err(rusqlite::Error::QueryReturnedNoRows), false),
            PeerPresence::UnreadablePeerName
        );
        // Any other error proves nothing, whatever the names look like.
        for names_readable in [true, false] {
            assert_eq!(
                classify_peer_presence(
                    &Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                        Some("database disk image is malformed".into()),
                    )),
                    names_readable
                ),
                PeerPresence::ProbeFailed
            );
        }

        // The property the whole site turns on, stated as an assertion:
        // exactly one classification is allowed to delete data.
        let purges = |p: PeerPresence| matches!(p, PeerPresence::ProvenAbsent);
        assert!(purges(PeerPresence::ProvenAbsent));
        assert!(!purges(PeerPresence::Present));
        assert!(!purges(PeerPresence::UnreadablePeerName));
        assert!(!purges(PeerPresence::ProbeFailed));
    }

    /// The precondition behind `ProvenAbsent`, and its fail-closed direction.
    #[test]
    fn agent_names_readability_probe_reports_the_corrupt_cell() {
        let store = SqliteStore::open_in_memory().unwrap();
        let agent = new_agent("atlas");
        store.create_agent(&agent).unwrap();

        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            assert!(
                agent_names_all_readable(&tx),
                "a healthy table makes a name-keyed miss trustworthy"
            );
        }

        corrupt_with_sql(&store, "UPDATE agents SET name = X'DEADBEEF'");

        let mut conn = store.conn.lock();
        let tx = conn.transaction().unwrap();
        assert!(
            !agent_names_all_readable(&tx),
            "one non-text name cell is enough to make every miss unprovable"
        );
    }

    // ── #1278: subagent sessions cascade with their PARENT, not their filer ──
    //
    // Since #1278 a named subagent session is filed under the *invoked*
    // agent's registry id while its `context_id` still names the invoking
    // parent. The four tests below pin both directions of the resulting
    // ownership rule, because a happy-path delete test passes either way —
    // the failure mode is entirely about whose rows go with whom.

    /// Seed `subagent_{parent}_{name}` filed under `filer`, with one row in
    /// every table the cascade touches. Returns the session and its run id.
    fn seed_subagent_session(
        store: &SqliteStore,
        parent: AgentId,
        filer: AgentId,
        context_id: &str,
    ) -> (Session, alms_core::RunId) {
        let session = Session::new(filer, context_id);
        store.save_session(&session).unwrap();
        store
            .save_message(session.id, &new_message("subagent transcript"))
            .unwrap();
        store
            .save_audit(&AuditEvent::allow(
                session.id,
                "shell",
                serde_json::json!({"cmd": "cargo test"}),
                serde_json::json!("ok"),
            ))
            .unwrap();

        // The run is registered under the FILER (`Run::for_subagent` is
        // called with `sub_agent_id`), and its `parent_run_id` points into
        // the PARENT's history — which is what makes destroying it
        // cross-agent data loss rather than a tidy-up.
        let parent_run = Run::new(session.id, parent, "delegate".to_string());
        let run = Run::for_subagent(
            session.id,
            filer,
            "do the work".to_string(),
            parent_run.run_id,
        );
        let run_id = run.run_id;
        store.save_run(&run).unwrap();
        store
            .save_tool_call(
                run_id,
                session.id,
                &ToolCallRecord {
                    seq: 0,
                    role: ToolCallRole::Assistant,
                    tool_name: Some("shell".to_string()),
                    tool_id: Some("call_0".to_string()),
                    params: Some(r#"{"cmd":"cargo test"}"#.to_string()),
                    result: None,
                    timestamp: chrono::Utc::now(),
                    from_agent: Some(filer.0.to_string()),
                },
            )
            .unwrap();

        (session, run_id)
    }

    fn count_where_session(store: &SqliteStore, table: &str, session_id: SessionId) -> i64 {
        let conn = store.conn.lock();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
            params![session_id.0.to_string()],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn assert_subagent_rows(store: &SqliteStore, session_id: SessionId, expected: i64, why: &str) {
        for table in ["sessions", "messages", "audit_events", "runs"] {
            let n: i64 = {
                let conn = store.conn.lock();
                let column = if table == "sessions" {
                    "id"
                } else {
                    "session_id"
                };
                conn.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                    params![session_id.0.to_string()],
                    |r| r.get(0),
                )
                .unwrap()
            };
            assert_eq!(
                n, expected,
                "`{table}` rows for the subagent session: {why}"
            );
        }
        assert_eq!(
            count_where_session(store, "run_tool_calls", session_id),
            expected,
            "`run_tool_calls` rows for the subagent session: {why}"
        );
    }

    #[test]
    fn delete_agent_spares_a_subagent_transcript_belonging_to_another_parent() {
        // The #1278 regression. `parent` invoked `reviewer` as a named
        // subagent, so the transcript is filed under reviewer's registry id
        // but is parent's history. Deleting reviewer must not touch it.
        //
        // Pre-fix this failed on every table at once: step 1's
        // `WHERE agent_id = ?1` selected the session, step 2 hard-deleted
        // its messages/audit/runs/tool-calls, and step 3 deleted the row.
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = new_agent("parent");
        let reviewer = new_agent("reviewer");
        store.create_agent(&parent).unwrap();
        store.create_agent(&reviewer).unwrap();

        let ctx = alms_core::named_subagent_context_id(parent.id, "reviewer");
        let (subagent, sub_run_id) = seed_subagent_session(&store, parent.id, reviewer.id, &ctx);

        // Reviewer's own chat session — this one SHOULD go.
        let own = Session::new(reviewer.id, "chat-reviewer");
        store.save_session(&own).unwrap();
        store
            .save_run(&Run::new(own.id, reviewer.id, "hi".into()))
            .unwrap();

        assert!(store.delete_agent(reviewer.id).unwrap());

        assert_subagent_rows(
            &store,
            subagent.id,
            1,
            "deleting the invoked agent must not destroy the invoking parent's transcript",
        );
        // Named specifically: the run carries `parent_run_id` into the
        // parent's own history, and step 4's `DELETE FROM runs WHERE
        // agent_id = ?1` reaches rows step 1 spared.
        assert!(
            store.load_run(sub_run_id).unwrap().is_some(),
            "the subagent run is filed under the deleted agent but belongs to the parent's \
             audit trail — step 4's per-agent sweep must skip it"
        );
        assert_subagent_rows(
            &store,
            own.id,
            0,
            "the deleted agent's own session still goes",
        );
    }

    #[test]
    fn delete_agent_takes_the_subagent_sessions_it_spawned() {
        // The other direction, and the reason the fix is not just an
        // exclusion: when the PARENT is deleted its subagent transcripts
        // must go with it, even though they are filed under a different
        // agent's id and so are invisible to `WHERE agent_id = ?1`.
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = new_agent("parent");
        let reviewer = new_agent("reviewer");
        store.create_agent(&parent).unwrap();
        store.create_agent(&reviewer).unwrap();

        let ctx = alms_core::named_subagent_context_id(parent.id, "reviewer");
        let (subagent, sub_run_id) = seed_subagent_session(&store, parent.id, reviewer.id, &ctx);

        // Reviewer's own chat session must survive the parent's delete.
        let own = Session::new(reviewer.id, "chat-reviewer");
        store.save_session(&own).unwrap();

        assert!(store.delete_agent(parent.id).unwrap());

        assert_subagent_rows(
            &store,
            subagent.id,
            0,
            "a subagent transcript belongs to the parent that spawned it, wherever it is filed",
        );
        assert!(store.load_run(sub_run_id).unwrap().is_none());
        let survives: i64 = {
            let conn = store.conn.lock();
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![own.id.0.to_string()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            survives, 1,
            "deleting the parent must not reach the invoked agent's own sessions"
        );
    }

    #[test]
    fn delete_agent_takes_unfiled_subagent_sessions_it_spawned() {
        // Ephemeral subagents (`AgentId::new()`) and named subagents whose
        // name was never registered (`AgentId::deterministic`) are filed
        // under ids no agent holds, so before #1278's parent-side sweep no
        // `delete_agent` call ever enumerated them and they accumulated
        // forever. They are the parent's history and go when the parent does.
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = new_agent("parent");
        store.create_agent(&parent).unwrap();

        let task_id = uuid::Uuid::new_v4();
        let (ephemeral, _) = seed_subagent_session(
            &store,
            parent.id,
            AgentId::new(),
            &format!("subagent_{}_{}", parent.id.0, task_id),
        );
        let (unregistered, _) = seed_subagent_session(
            &store,
            parent.id,
            AgentId::deterministic(parent.id, "ghost"),
            &alms_core::named_subagent_context_id(parent.id, "ghost"),
        );

        assert!(store.delete_agent(parent.id).unwrap());

        assert_subagent_rows(
            &store,
            ephemeral.id,
            0,
            "ephemeral subagent session of this parent",
        );
        assert_subagent_rows(
            &store,
            unregistered.id,
            0,
            "unregistered-name subagent session of this parent",
        );
    }

    #[test]
    fn delete_agent_removes_a_self_invoked_subagent_session() {
        // Tim N1: nothing forbids `invoke_agent { name: "atlas" }` from
        // atlas, which yields `subagent_{atlas}_atlas` filed under atlas.
        // That row matches BOTH halves of step 1 — filed under the agent
        // and parented by it — and must still be deleted rather than
        // falling between them.
        //
        // What this does NOT pin is the `seen` dedupe: every delete in
        // step 2/3 is idempotent, so collecting the id twice produces the
        // same end state and removing `seen` fails nothing here. `seen` is
        // there to keep the work linear and the id set honest, not to
        // protect a behaviour — do not read this test as covering it.
        let store = SqliteStore::open_in_memory().unwrap();
        let atlas = new_agent("atlas");
        store.create_agent(&atlas).unwrap();

        let ctx = alms_core::named_subagent_context_id(atlas.id, "atlas");
        let (self_invoked, run_id) = seed_subagent_session(&store, atlas.id, atlas.id, &ctx);

        assert!(store.delete_agent(atlas.id).unwrap());

        assert_subagent_rows(
            &store,
            self_invoked.id,
            0,
            "a self-invoked subagent session is owned by the agent on both counts",
        );
        assert!(store.load_run(run_id).unwrap().is_none());
    }
}
