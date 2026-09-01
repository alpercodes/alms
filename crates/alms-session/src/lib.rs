pub mod job_store;
pub mod sqlite;
pub mod types;

pub use alms_core::AuditEvent;
pub use job_store::JobStore;
pub use sqlite::{SessionToolCall, SqliteStore, TimelineEvent, TimelinePage};
pub use types::{Content, ContextSummary, Message, Role, Session, SessionConfig, SessionSummary};

use alms_core::{AgentId, AlmsResult, RunId, SessionId, Timestamp};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

fn is_pending_input(message: &Message) -> bool {
    message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("pending_input"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

fn claimed_input_timestamp(message: &Message) -> Option<chrono::DateTime<chrono::Utc>> {
    message
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("input_claimed_at"))
        .and_then(serde_json::Value::as_str)
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
}

fn logical_message_timestamp(message: &Message) -> chrono::DateTime<chrono::Utc> {
    claimed_input_timestamp(message).unwrap_or(message.timestamp.0)
}

/// Session manager - owns all session state
#[derive(Debug)]
pub struct SessionManager {
    /// Active sessions: (agent_id, context_id) -> Session
    sessions: Arc<DashMap<(AgentId, String), Session>>,
    /// Reverse index: session_id -> (agent_id, context_id) for O(1) lookup by ID.
    session_by_id: Arc<DashMap<SessionId, (AgentId, String)>>,
    /// Session history: session_id -> Vec<Message>
    history: Arc<DashMap<SessionId, Vec<Message>>>,
    /// Audit events: session_id -> Vec<AuditEvent>
    audit: Arc<DashMap<SessionId, Vec<AuditEvent>>>,
    /// Rolling context summaries: session_id -> ContextSummary
    summaries: Arc<DashMap<SessionId, ContextSummary>>,
    /// Configuration
    config: SessionConfig,
    /// Optional SQLite write-through store
    store: Option<Arc<SqliteStore>>,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            session_by_id: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            summaries: Arc::new(DashMap::new()),
            config,
            store: None,
        }
    }

    /// Create a session manager backed by an existing `SqliteStore` (for tests).
    pub fn with_store(config: SessionConfig, store: SqliteStore) -> AlmsResult<Self> {
        let mut manager = Self::new(config);
        manager.store = Some(Arc::new(store));
        manager.load_from_store()?;
        Ok(manager)
    }

    /// Create a session manager backed by SQLite at `db_path`.
    ///
    /// Opens (or creates) the database, runs schema migrations, then loads all
    /// persisted sessions + messages + audit events into the in-memory maps.
    pub fn with_sqlite(config: SessionConfig, db_path: &str) -> AlmsResult<Self> {
        let store = SqliteStore::open(db_path)?;
        let mut manager = Self::new(config);
        manager.store = Some(Arc::new(store));
        manager.load_from_store()?;
        Ok(manager)
    }

    /// Populate in-memory maps from the SQLite store (called once on startup).
    fn load_from_store(&self) -> AlmsResult<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let sessions = store.load_all_sessions()?;
        let count = sessions.len();
        for session in sessions {
            let key = (session.agent_id, session.context_id.clone());
            let session_id = session.id;
            self.session_by_id.insert(session_id, key.clone());
            self.sessions.insert(key, session);
            self.history
                .insert(session_id, store.load_messages(session_id)?);
            self.audit.insert(session_id, store.load_audit(session_id)?);
            let summary = store.load_summary(session_id)?.unwrap_or_default();
            self.summaries.insert(session_id, summary);
        }
        if count > 0 {
            info!("Loaded {} session(s) from SQLite", count);
        }
        Ok(())
    }

    /// Check if a session exists for the given (agent_id, context_id) key.
    pub fn has_session(&self, key: &(AgentId, String)) -> bool {
        self.sessions.contains_key(key)
    }

    /// Look up an existing session's real `SessionId` by its
    /// `(agent_id, context_id)` key **without creating one**.
    ///
    /// Unlike [`get_or_create`](Self::get_or_create) this is a pure read with
    /// no side effects. It exists for callers that hold a well-known context
    /// handle (e.g. a job's hidden `job_{job_id}` session) and need the random
    /// `SessionId` behind it — the value `GET /session/{id}` (`Path<SessionId>`)
    /// can actually resolve, since the context id is not itself a session id
    /// (#1217).
    pub fn session_id_for_context(&self, agent_id: AgentId, context_id: &str) -> Option<SessionId> {
        let key = (agent_id, context_id.to_string());
        self.sessions.get(&key).map(|s| s.id)
    }

    /// Get or create a session.
    ///
    /// Uses `DashMap::entry()` to make the check-and-insert atomic, preventing
    /// a TOCTOU race where two threads could create duplicate sessions for the
    /// same `(agent_id, context_id)` key.
    pub fn get_or_create(&self, agent_id: AgentId, context_id: impl Into<String>) -> Session {
        let context_id = context_id.into();
        let key = (agent_id, context_id.clone());

        let session = self
            .sessions
            .entry(key.clone())
            .or_insert_with(|| {
                let session = Session::new(agent_id, context_id);
                self.session_by_id.insert(session.id, key);
                self.history.insert(session.id, Vec::new());
                self.audit.insert(session.id, Vec::new());
                self.summaries.entry(session.id).or_default();

                if let Some(store) = &self.store
                    && let Err(e) = store.save_session(&session)
                {
                    warn!("Failed to persist session {}: {}", session.id.0, e);
                }

                info!("Created new session: {:?}", session.id);
                session
            })
            .clone();

        debug!("get_or_create session: {:?}", session.id);
        session
    }

    /// Get or create a session with a specific `SessionId`.
    ///
    /// Like [`get_or_create`](Self::get_or_create), this is keyed on
    /// `(agent_id, context_id)` so subsequent calls to `get_or_create` with
    /// the same key will find the session created here. The difference is
    /// that when the session does not yet exist, the caller-provided
    /// `session_id` is used instead of generating a random UUID v4.
    ///
    /// This is needed when a `RunTrigger` carries a deterministic
    /// `SessionId` (e.g. `SessionId::deterministic("notifications:agent")`)
    /// but the run execution path goes through `runtime.run()` ->
    /// `get_or_create()`. Pre-creating the session here ensures the
    /// deterministic ID is preserved.
    pub fn get_or_create_with_id(
        &self,
        session_id: SessionId,
        agent_id: AgentId,
        context_id: impl Into<String>,
    ) -> AlmsResult<Session> {
        let context_id = context_id.into();
        let key = (agent_id, context_id.clone());

        let session = match self.sessions.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                let session = entry.get().clone();
                if session.id != session_id {
                    return Err(alms_core::AlmsError::Runtime(format!(
                        "Context {context_id} already resolves to session {}, not {}",
                        session.id.0, session_id.0
                    )));
                }
                session
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                if let Some(existing_key) = self.session_by_id.get(&session_id)
                    && existing_key.value() != &key
                {
                    return Err(alms_core::AlmsError::Runtime(format!(
                        "Session {} is already registered for another context",
                        session_id.0
                    )));
                }

                let now = alms_core::Timestamp::now();
                let session = Session {
                    id: session_id,
                    agent_id,
                    context_id,
                    created_at: now,
                    last_activity: now,
                    status: types::SessionStatus::Active,
                };

                // Persist before publishing any in-memory projection. A
                // deterministic first-use notification session must either be
                // authoritative on disk or not exist at all; logging and
                // continuing would let a run outlive its missing parent after
                // restart.
                if let Some(store) = &self.store {
                    store.save_session(&session)?;
                }

                self.session_by_id.insert(session_id, key);
                self.history.insert(session_id, Vec::new());
                self.audit.insert(session_id, Vec::new());
                self.summaries.entry(session_id).or_default();

                info!(
                    "Created new session with predetermined ID: {:?}",
                    session_id
                );
                entry.insert(session.clone());
                session
            }
        };

        debug!("get_or_create_with_id session: {:?}", session.id);
        Ok(session)
    }

    /// Get or create a shared session by a known `SessionId`.
    ///
    /// Shared sessions (DM, group) are not owned by a single agent. They use
    /// a deterministic `SessionId` derived from the participants. This method
    /// creates the session if it does not exist, keyed by `(sentinel_agent_id,
    /// context_id)` in the internal map so it integrates with the existing
    /// session infrastructure.
    ///
    /// Lock ordering: `sessions` (outer) -> `session_by_id` (inner), matching
    /// `get_or_create` to prevent AB/BA deadlocks.
    pub fn get_or_create_shared(
        &self,
        session_id: SessionId,
        context_id: impl Into<String>,
    ) -> Session {
        let context_id = context_id.into();
        let sentinel = AgentId(uuid::Uuid::nil());
        let key = (sentinel, context_id.clone());

        // Use `sessions.entry()` as the outer lock — same ordering as
        // `get_or_create` — then insert into `session_by_id` inside the
        // closure. This avoids the AB/BA deadlock that would occur if we
        // locked `session_by_id` first.
        let session = self
            .sessions
            .entry(key.clone())
            .or_insert_with(|| {
                let session = Session {
                    id: session_id,
                    agent_id: sentinel,
                    context_id,
                    created_at: alms_core::Timestamp::now(),
                    last_activity: alms_core::Timestamp::now(),
                    status: types::SessionStatus::Active,
                };

                self.session_by_id.insert(session_id, key);
                self.history.insert(session_id, Vec::new());
                self.audit.insert(session_id, Vec::new());
                self.summaries.entry(session_id).or_default();

                if let Some(store) = &self.store
                    && let Err(e) = store.save_session(&session)
                {
                    warn!("Failed to persist shared session {}: {}", session_id.0, e);
                }

                info!("Created new shared session: {:?}", session_id);
                session
            })
            .clone();

        debug!("get_or_create_shared session: {:?}", session_id);
        session
    }

    /// Check if a session with the given `SessionId` exists.
    pub fn has_session_by_id(&self, session_id: SessionId) -> bool {
        self.session_by_id.contains_key(&session_id)
    }

    /// Get a session by ID.
    ///
    /// Clones the key out of `session_by_id` (releasing its read lock) before
    /// looking up in `sessions`, so both maps are never locked simultaneously.
    /// This maintains the same lock ordering as `get_or_create` / `get_or_create_shared`.
    pub fn get(&self, session_id: SessionId) -> AlmsResult<Session> {
        let key = self
            .session_by_id
            .get(&session_id)
            .map(|r| r.value().clone());
        if let Some(key) = key
            && let Some(session) = self.sessions.get(&key)
        {
            return Ok(session.clone());
        }
        Err(alms_core::AlmsError::SessionNotFound(
            session_id.0.to_string(),
        ))
    }

    /// Append a message to a session.
    ///
    /// Returns `Err(SessionNotFound)` early if no history entry exists for the
    /// given `session_id` (stricter than the previous silent-success behavior,
    /// but all callers create sessions via `get_or_create` first, so this is
    /// always populated).
    ///
    /// Lock ordering: `history` is scoped and released before acquiring
    /// `sessions` / `session_by_id`, preventing AB/BA deadlocks with
    /// `get_or_create` which holds `sessions` while inserting into `history`.
    pub fn append_message(&self, session_id: SessionId, message: Message) -> AlmsResult<()> {
        // Scope the history write lock so it is released before we touch `sessions`.
        {
            let mut history = self
                .history
                .get_mut(&session_id)
                .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))?;
            if let Some(store) = &self.store
                && let Err(e) = store.save_message(session_id, &message)
            {
                warn!(
                    "Failed to persist message for session {}: {}",
                    session_id.0, e
                );
            }
            history.push(message);
        } // history lock released here

        // Update last_activity via the reverse index — O(1), no full scan.
        // Clone the key out of `session_by_id` (releasing its read lock) before
        // acquiring a write lock on `sessions`, avoiding cross-map lock nesting.
        let session_key = self
            .session_by_id
            .get(&session_id)
            .map(|r| r.value().clone());
        if let Some(key) = session_key
            && let Some(mut session) = self.sessions.get_mut(&key)
        {
            session.touch();
            // Write-through updated last_activity to SQLite
            if let Some(store) = &self.store
                && let Err(e) = store.save_session(&session)
            {
                warn!(
                    "Failed to persist last_activity for session {}: {}",
                    session_id.0, e
                );
            }
        }

        Ok(())
    }

    /// Update the in-memory projection after the run and initial message were
    /// already committed by `SqliteStore::save_run_with_initial_message`.
    pub fn append_persisted_message(
        &self,
        session_id: SessionId,
        message: Message,
        touched_at: Timestamp,
    ) -> AlmsResult<()> {
        let key = self
            .session_by_id
            .get(&session_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))?;

        {
            let mut history = self
                .history
                .get_mut(&session_id)
                .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))?;
            history.push(message);
        }

        let mut session = self
            .sessions
            .get_mut(&key)
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))?;
        if touched_at.0 > session.last_activity.0 {
            session.last_activity = touched_at;
        }
        Ok(())
    }
    /// Get session history
    pub fn get_history(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        self.history
            .get(&session_id)
            .map(|h| h.clone())
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
    }

    /// Mark this run's pre-persisted user input as active context.
    ///
    /// Admissions persist user messages before queueing so the UI never loses
    /// them. A queued prompt must not, however, enter an earlier run's LLM
    /// snapshot. The claim is written through before memory is updated and its
    /// timestamp records actual execution order without changing SQLite `seq`.
    pub fn claim_pending_input(&self, session_id: SessionId, run_id: RunId) -> AlmsResult<()> {
        let mut history = self
            .history
            .get_mut(&session_id)
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))?;
        let run_id = run_id.0.to_string();
        let position = history
            .iter()
            .position(|message| {
                message
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("run_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(run_id.as_str())
            })
            .ok_or_else(|| {
                alms_core::AlmsError::Runtime(format!(
                    "Pre-persisted input for run {run_id} was not found"
                ))
            })?;

        if !is_pending_input(&history[position]) {
            return Err(alms_core::AlmsError::Runtime(format!(
                "Pre-persisted input for run {run_id} was already claimed"
            )));
        }

        let latest_visible = history
            .iter()
            .filter(|message| !is_pending_input(message))
            .map(logical_message_timestamp)
            .max();
        let mut claimed_at = chrono::Utc::now();
        if let Some(latest_visible) = latest_visible
            && claimed_at <= latest_visible
        {
            claimed_at = latest_visible + chrono::Duration::nanoseconds(1);
        }

        let mut claimed = history[position].clone();
        let metadata = claimed
            .metadata
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                alms_core::AlmsError::Runtime(format!(
                    "Pre-persisted input for run {run_id} has invalid metadata"
                ))
            })?;
        metadata.insert("pending_input".to_string(), serde_json::Value::Bool(false));
        metadata.insert(
            "input_claimed_at".to_string(),
            serde_json::Value::String(claimed_at.to_rfc3339()),
        );

        if let Some(store) = &self.store {
            store.save_message(session_id, &claimed)?;
        }
        history[position] = claimed;
        Ok(())
    }

    /// Return only messages eligible for an LLM context snapshot.
    ///
    /// Pending admissions remain in normal history for UI/API visibility but
    /// are hidden here until their run starts. Once claimed inputs exist, the
    /// durable claim timestamps restore execution order around assistant
    /// responses even when several user prompts were pre-persisted first.
    pub fn get_context_history(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        let mut history = self.get_history(session_id)?;
        history.retain(|message| !is_pending_input(message));
        if history
            .iter()
            .any(|message| claimed_input_timestamp(message).is_some())
        {
            history.sort_by_key(logical_message_timestamp);
        }
        Ok(history)
    }

    /// Find the last message in a session that satisfies `predicate`.
    ///
    /// Performs a reverse scan inside the DashMap read guard and clones only the
    /// single matching [`Message`], avoiding a full `Vec<Message>` clone that
    /// [`get_history`] would require.
    ///
    /// Returns `None` if the session does not exist **or** no message matches.
    pub fn find_last_message<F>(&self, session_id: SessionId, mut predicate: F) -> Option<Message>
    where
        F: FnMut(&Message) -> bool,
    {
        self.history
            .get(&session_id)
            .and_then(|h| h.iter().rev().find(|m| predicate(m)).cloned())
    }

    /// Append audit event
    pub fn append_audit(&self, session_id: SessionId, event: AuditEvent) -> AlmsResult<()> {
        if let Some(mut audit) = self.audit.get_mut(&session_id) {
            if let Some(store) = &self.store
                && let Err(e) = store.save_audit(&event)
            {
                warn!(
                    "Failed to persist audit event for session {}: {}",
                    session_id.0, e
                );
            }
            audit.push(event);
            Ok(())
        } else {
            Err(alms_core::AlmsError::SessionNotFound(
                session_id.0.to_string(),
            ))
        }
    }

    /// Get audit events
    pub fn get_audit(&self, session_id: SessionId) -> AlmsResult<Vec<AuditEvent>> {
        self.audit
            .get(&session_id)
            .map(|a| a.clone())
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
    }

    /// List active sessions for an agent
    pub fn list_active(&self, agent_id: AgentId) -> Vec<Session> {
        self.sessions
            .iter()
            .filter(|e| e.key().0 == agent_id)
            .map(|e| e.value().clone())
            .collect()
    }

    /// List active sessions for an agent, including shared DM sessions where
    /// the agent is a participant.
    ///
    /// Shared DM sessions are stored under `AgentId::nil()` (sentinel) in the
    /// in-memory map, so [`list_active`] misses them.  This method supplements
    /// the normal list with DM sessions whose `context_id` matches
    /// `dm:...<agent_name>...`.
    pub fn list_active_with_dms(&self, agent_id: AgentId, agent_name: &str) -> Vec<Session> {
        let mut sessions = self.list_active(agent_id);

        // Also include shared DM sessions where this agent is a participant.
        // DM context_ids have the format "dm:{name1}:{name2}" (alphabetical).
        //
        // Participation is decided by `dm_peer` rather than by an inline
        // comparison so there is exactly ONE participant-matching rule in the
        // codebase — case-insensitive since #2, matching how agent names
        // resolve everywhere else. A second inline rule here is how a sidebar
        // silently loses a DM the tool layer can still see.
        let sentinel = AgentId(uuid::Uuid::nil());
        let dm_sessions: Vec<Session> = self
            .sessions
            .iter()
            .filter(|e| {
                e.key().0 == sentinel
                    && alms_core::dm_peer(&e.value().context_id, agent_name).is_some()
            })
            .map(|e| e.value().clone())
            .collect();

        sessions.extend(dm_sessions);
        sessions
    }

    /// List all sessions across all agents, sorted by last_activity descending.
    pub fn list_all(&self) -> Vec<Session> {
        let mut sessions: Vec<Session> = self.sessions.iter().map(|e| e.value().clone()).collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity.0));
        sessions
    }

    /// Delete a session
    pub fn delete(&self, agent_id: AgentId, context_id: impl AsRef<str>) -> AlmsResult<()> {
        let key = (agent_id, context_id.as_ref().to_string());

        if let Some((_, session)) = self.sessions.remove(&key) {
            self.session_by_id.remove(&session.id);
            self.history.remove(&session.id);
            self.audit.remove(&session.id);
            self.summaries.remove(&session.id);
            // Remove from SQLite
            if let Some(store) = &self.store
                && let Err(e) = store.delete_session(session.id)
            {
                warn!(
                    "Failed to delete session {} from SQLite: {}",
                    session.id.0, e
                );
            }
            info!("Deleted session: {:?}", session.id);
            Ok(())
        } else {
            Err(alms_core::AlmsError::SessionNotFound(key.1))
        }
    }

    /// Get the rolling context summary for a session.
    pub fn get_summary(&self, session_id: SessionId) -> AlmsResult<ContextSummary> {
        self.summaries
            .get(&session_id)
            .map(|s| s.clone())
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
    }

    /// Replace the rolling context summary for a session (write-through to SQLite).
    pub fn update_summary(&self, session_id: SessionId, summary: ContextSummary) -> AlmsResult<()> {
        if !self.summaries.contains_key(&session_id) {
            return Err(alms_core::AlmsError::SessionNotFound(
                session_id.0.to_string(),
            ));
        }
        if let Some(store) = &self.store
            && let Err(e) = store.save_summary(session_id, &summary)
        {
            warn!(
                "Failed to persist summary for session {}: {}",
                session_id.0, e
            );
        }
        self.summaries.insert(session_id, summary);
        Ok(())
    }

    // -- Episodic session summaries (cross-session memory) --------------------

    /// Insert or update the episodic summary for a `(agent_id, session_id)` pair.
    ///
    /// `source_label` is a human-readable label derived from the session's
    /// `context_id` (e.g. "User chat", "Telegram chat").
    ///
    /// No-op (with a warning) when no SQLite store is configured.
    pub fn upsert_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
        summary_text: &str,
        run_id: Option<RunId>,
        source_label: Option<&str>,
    ) -> AlmsResult<()> {
        if let Some(store) = &self.store {
            store.upsert_session_summary(
                agent_id,
                session_id,
                summary_text,
                run_id,
                source_label,
            )?;
        } else {
            warn!("upsert_session_summary called without SQLite store -- skipping");
        }
        Ok(())
    }

    /// Insert or update the episodic summary for a `(agent_id, session_id)` pair using optimistic locking.
    ///
    /// Checks `expected_last_run_id` to prevent concurrent overwrite races.
    /// Returns `Ok(true)` if successfully persisted, or `Ok(false)` if a conflict is detected.
    /// Returns `Ok(true)` (with a warning) when no SQLite store is configured.
    pub fn upsert_session_summary_optimistic(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
        summary_text: &str,
        run_id: Option<RunId>,
        source_label: Option<&str>,
        expected_last_run_id: Option<RunId>,
    ) -> AlmsResult<bool> {
        if let Some(store) = &self.store {
            store.upsert_session_summary_optimistic(
                agent_id,
                session_id,
                summary_text,
                run_id,
                source_label,
                expected_last_run_id,
            )
        } else {
            warn!("upsert_session_summary_optimistic called without SQLite store -- skipping");
            Ok(true)
        }
    }

    /// Load all episodic summaries for an agent, ordered by `updated_at DESC`,
    /// up to `limit`.
    ///
    /// When `exclude_session_id` is `Some`, that session's summary is omitted
    /// from results (useful for excluding the current session when injecting
    /// cross-session context).
    ///
    /// Returns an empty vec when no SQLite store is configured.
    pub fn load_session_summaries(
        &self,
        agent_id: AgentId,
        limit: usize,
        exclude_session_id: Option<&SessionId>,
    ) -> AlmsResult<Vec<SessionSummary>> {
        if let Some(store) = &self.store {
            store.load_session_summaries(agent_id, limit, exclude_session_id)
        } else {
            Ok(Vec::new())
        }
    }

    /// Load a single episodic summary by `(agent_id, session_id)`.
    ///
    /// Returns `None` when no SQLite store is configured.
    pub fn load_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
    ) -> AlmsResult<Option<SessionSummary>> {
        if let Some(store) = &self.store {
            store.load_session_summary(agent_id, session_id)
        } else {
            Ok(None)
        }
    }

    /// Delete the episodic summary for a `(agent_id, session_id)` pair.
    ///
    /// No-op when no SQLite store is configured.
    pub fn delete_session_summary(
        &self,
        agent_id: AgentId,
        session_id: SessionId,
    ) -> AlmsResult<()> {
        if let Some(store) = &self.store {
            store.delete_session_summary(agent_id, session_id)?;
        }
        Ok(())
    }

    /// Get config
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Get the underlying SQLite store (if any).
    pub fn store(&self) -> Option<&Arc<SqliteStore>> {
        self.store.as_ref()
    }

    /// Resolve the `(agent_id, context_id)` key a **named** subagent
    /// session is filed under (#1278).
    ///
    /// The single source of truth for that key. Two callers must agree on
    /// it exactly or a named subagent silently forks into two session rows:
    /// the coordinator's `derive_subagent_identity` (write) and
    /// `ReadSubagentSessionTool`'s by-name lookup (read).
    ///
    /// # Which agent id
    ///
    /// The invoked agent's **registry id**, when a registry record exists
    /// for `name`. Before #1278 the key was
    /// `AgentId::deterministic(parent_agent_id, name)`, which resolves
    /// against no registered agent — so a named subagent's transcript was
    /// filed under an id nothing in the product could look up, and the
    /// invoked agent's own work never appeared in its own timeline. The
    /// `context_id` is unchanged (see
    /// [`alms_core::named_subagent_context_id`]): the parent-ownership
    /// check reads its bytes.
    ///
    /// # When there is no registry record
    ///
    /// `invoke_agent` validates the *shape* of a subagent name but does not
    /// require it to be registered — an unregistered name runs with default
    /// config and a `WARN`. There is no registry id to file such a session
    /// under, so it keeps the derived one. This is the only surviving use
    /// of `AgentId::deterministic` for subagent keying, and it is a live
    /// case rather than a legacy accommodation.
    ///
    /// # This answer does not depend on what is already stored
    ///
    /// The key is a pure function of the registry *when the registry can be
    /// read*, deliberately: it never looks at whether a session already sits
    /// on either key. So a name invoked before its agent was registered
    /// lands on the derived id, and the invocation after the registration
    /// lands on the registry id and starts a fresh session — the earlier
    /// transcript stays in the database but is no longer reachable by name.
    ///
    /// The qualifier is load-bearing. A store error is *not* the same answer
    /// as "no such agent" even though both fall back to the derived id: an
    /// absent agent gives the same key on every invocation, whereas a
    /// transient read failure files one invocation on a different key than
    /// the invocation either side of it, forking the named subagent's
    /// session. That is exactly the order-dependence the paragraph above
    /// claims is excluded, so it is logged rather than swallowed — the
    /// #1241/#1246 rule that "absent" and "unreadable" must not collapse
    /// into one silent `None`. Nothing is corrupted (the fallback is the
    /// pre-#1278 derived id, not a fresh one) and failing the invocation
    /// instead would make a named subagent unusable for as long as the
    /// fault lasts, so the fork is the cheaper disposition — but it must be
    /// visible.
    ///
    /// Databases written before #1278 lose their named subagent history the
    /// same way, and there is no migration to re-home them. That is an
    /// accepted, deliberate break (ALMS has no production deployments):
    /// keying on "what happens to be stored" would make the identity of a
    /// named subagent depend on invocation order, which is a far worse
    /// thing to carry forward than one lost transcript.
    pub fn named_subagent_key(&self, parent_agent_id: AgentId, name: &str) -> (AgentId, String) {
        let context_id = alms_core::named_subagent_context_id(parent_agent_id, name);

        let registry_id = self.store.as_ref().and_then(|store| {
            match store.load_agent_by_name(name) {
                Ok(record) => record.map(|r| r.id),
                Err(e) => {
                    tracing::warn!(
                        subagent_name = %name,
                        parent_agent_id = %parent_agent_id.0,
                        error = %e,
                        "Agent registry unreadable while keying a named subagent; filing this \
                         invocation under the derived fallback id, which forks its session from \
                         invocations that resolved the registry id"
                    );
                    None
                }
            }
        });

        match registry_id {
            Some(agent_id) => (agent_id, context_id),
            None => (AgentId::deterministic(parent_agent_id, name), context_id),
        }
    }

    /// Flush the SQLite WAL to disk. No-op if no SQLite store is attached.
    pub fn flush_wal(&self) -> AlmsResult<()> {
        if let Some(store) = &self.store {
            store.flush_wal()?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl SessionManager {
    /// Archive idle sessions (test-only — not wired to any production code path).
    pub fn archive_idle(&self) -> usize {
        let mut count = 0;
        let timeout = std::time::Duration::from_secs(self.config.idle_timeout_secs);

        for mut entry in self.sessions.iter_mut() {
            let session = entry.value_mut();
            let idle = alms_core::Timestamp::now().0 - session.last_activity.0;

            if idle > chrono::Duration::from_std(timeout).unwrap_or_default()
                && session.status == types::SessionStatus::Active
            {
                session.status = types::SessionStatus::Idle;
                // Write-through updated status to SQLite
                if let Some(store) = &self.store
                    && let Err(e) = store.save_session(session)
                {
                    warn!(
                        "Failed to persist idle status for session {:?}: {}",
                        session.id, e
                    );
                }
                count += 1;
                info!("Archived idle session: {:?}", session.id);
            }
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Content, Message, Role, SessionStatus};

    fn make_manager() -> SessionManager {
        let store = SqliteStore::open_in_memory().unwrap();
        SessionManager::with_store(SessionConfig::default(), store).unwrap()
    }

    fn make_msg(text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: alms_core::Timestamp::now(),
            metadata: None,
        }
    }

    #[test]
    fn test_append_message_persists_last_activity() {
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "ctx1");
        let original_activity = session.last_activity;

        // Small delay so touch() produces a different timestamp
        std::thread::sleep(std::time::Duration::from_millis(10));
        mgr.append_message(session.id, make_msg("hello")).unwrap();

        // Reload from SQLite to verify write-through
        let store = mgr.store.as_ref().unwrap();
        let reloaded = store.load_all_sessions().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].last_activity.0 > original_activity.0);
    }

    #[test]
    fn persisted_message_projection_never_regresses_last_activity() {
        let mgr = make_manager();
        let session = mgr.get_or_create(AgentId::new(), "monotonic-activity");
        let newer = alms_core::Timestamp(session.last_activity.0 + chrono::Duration::minutes(2));
        let older = alms_core::Timestamp(session.last_activity.0 + chrono::Duration::minutes(1));

        mgr.append_persisted_message(session.id, make_msg("newer"), newer)
            .unwrap();
        mgr.append_persisted_message(session.id, make_msg("older"), older)
            .unwrap();

        assert_eq!(mgr.get(session.id).unwrap().last_activity, newer);
    }

    #[test]
    fn context_history_claims_pending_inputs_in_execution_order_across_restart() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("claimed-inputs.db");
        let db_path = db_path.to_str().unwrap();
        let mgr = SessionManager::with_sqlite(SessionConfig::default(), db_path).unwrap();
        let session = mgr.get_or_create(AgentId::new(), "claimed-inputs");
        let first_run = RunId::new();
        let second_run = RunId::new();

        let pending = |text: &str, run_id: RunId| Message {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: Content::Text(text.to_string()),
            timestamp: Timestamp::now(),
            metadata: Some(serde_json::json!({
                "pending_input": true,
                "run_id": run_id.0.to_string(),
            })),
        };
        mgr.append_message(session.id, pending("first prompt", first_run))
            .unwrap();
        mgr.append_message(session.id, pending("second prompt", second_run))
            .unwrap();

        mgr.claim_pending_input(session.id, first_run).unwrap();
        let first_context = mgr.get_context_history(session.id).unwrap();
        assert_eq!(first_context.len(), 1);
        assert!(matches!(
            &first_context[0].content,
            Content::Text(text) if text == "first prompt"
        ));
        let visible_history = mgr.get_history(session.id).unwrap();
        assert_eq!(visible_history.len(), 2, "queued input remains UI-visible");
        assert_eq!(
            visible_history[0].metadata.as_ref().unwrap()["pending_input"],
            false
        );
        assert!(
            visible_history[0].metadata.as_ref().unwrap()["input_claimed_at"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            visible_history[1].metadata.as_ref().unwrap()["pending_input"],
            true
        );

        let mut reply = make_msg("first reply");
        reply.role = Role::Assistant;
        mgr.append_message(session.id, reply).unwrap();
        mgr.claim_pending_input(session.id, second_run).unwrap();

        let context_text = |manager: &SessionManager| {
            manager
                .get_context_history(session.id)
                .unwrap()
                .into_iter()
                .filter_map(|message| match message.content {
                    Content::Text(text) => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            context_text(&mgr),
            vec!["first prompt", "first reply", "second prompt"]
        );

        drop(mgr);
        let reloaded = SessionManager::with_sqlite(SessionConfig::default(), db_path).unwrap();
        assert_eq!(
            context_text(&reloaded),
            vec!["first prompt", "first reply", "second prompt"],
            "claim metadata must reconstruct logical turn order after restart"
        );
    }

    #[test]
    fn test_archive_idle_persists_status() {
        let config = SessionConfig {
            idle_timeout_secs: 0, // immediate idle
            ..SessionConfig::default()
        };
        let store = SqliteStore::open_in_memory().unwrap();
        let mgr = SessionManager::with_store(config, store).unwrap();

        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "ctx-idle");
        assert_eq!(session.status, SessionStatus::Active);

        // Small delay to exceed 0s timeout
        std::thread::sleep(std::time::Duration::from_millis(10));
        let count = mgr.archive_idle();
        assert_eq!(count, 1);

        // Reload from SQLite
        let store = mgr.store.as_ref().unwrap();
        let reloaded = store.load_all_sessions().unwrap();
        assert_eq!(reloaded[0].status, SessionStatus::Idle);
    }

    #[test]
    fn test_session_summary_delegation_roundtrip() {
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "ctx-summary");

        let run_id = alms_core::RunId::new();
        mgr.upsert_session_summary(
            agent_id,
            session.id,
            "Debugged CORS headers.",
            Some(run_id),
            Some("User chat"),
        )
        .unwrap();

        // Single lookup
        let loaded = mgr
            .load_session_summary(agent_id, session.id)
            .unwrap()
            .expect("summary should exist");
        assert_eq!(loaded.summary, "Debugged CORS headers.");
        assert_eq!(loaded.last_run_id, Some(run_id));

        // Batch load
        let all = mgr.load_session_summaries(agent_id, 10, None).unwrap();
        assert_eq!(all.len(), 1);

        // Delete
        mgr.delete_session_summary(agent_id, session.id).unwrap();
        assert!(
            mgr.load_session_summary(agent_id, session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_session_summary_no_store_graceful() {
        // Without a SQLite store, delegation methods should succeed gracefully.
        let mgr = SessionManager::new(SessionConfig::default());
        let agent_id = AgentId::new();
        let session_id = SessionId::new();

        // upsert is a no-op
        mgr.upsert_session_summary(agent_id, session_id, "test", None, None)
            .unwrap();

        // load returns empty / None
        assert!(
            mgr.load_session_summaries(agent_id, 10, None)
                .unwrap()
                .is_empty()
        );
        assert!(
            mgr.load_session_summary(agent_id, session_id)
                .unwrap()
                .is_none()
        );

        // delete is a no-op
        mgr.delete_session_summary(agent_id, session_id).unwrap();
    }

    #[test]
    fn test_delete_removes_from_sqlite() {
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "ctx-del");
        mgr.append_message(session.id, make_msg("bye")).unwrap();

        mgr.delete(agent_id, "ctx-del").unwrap();

        // Verify SQLite is empty
        let store = mgr.store.as_ref().unwrap();
        let sessions = store.load_all_sessions().unwrap();
        assert!(sessions.is_empty());
        let messages = store.load_messages(session.id).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_list_active_with_dms_includes_dm_sessions() {
        let mgr = make_manager();
        let agent_id = AgentId::new();

        // Create a regular session under the agent's own ID.
        mgr.get_or_create(agent_id, "web-chat");

        // Create a shared DM session (stored under AgentId::nil sentinel).
        let dm_sid = SessionId::deterministic_dm("alice", "bob");
        mgr.get_or_create_shared(dm_sid, "dm:alice:bob");

        // list_active only finds sessions keyed under the agent's own ID.
        let active = mgr.list_active(agent_id);
        assert_eq!(active.len(), 1, "list_active should only find web-chat");

        // list_active_with_dms also includes DM sessions where agent is a participant.
        let with_dms = mgr.list_active_with_dms(agent_id, "alice");
        assert_eq!(
            with_dms.len(),
            2,
            "list_active_with_dms should find web-chat + DM"
        );
        let dm_count = with_dms
            .iter()
            .filter(|s| s.context_id.starts_with("dm:"))
            .count();
        assert_eq!(dm_count, 1, "Should include exactly one DM session");
    }

    #[test]
    fn test_list_active_with_dms_excludes_non_participant() {
        let mgr = make_manager();
        let agent_id = AgentId::new();

        mgr.get_or_create(agent_id, "web-chat");

        // DM between alice and bob.
        let dm_sid = SessionId::deterministic_dm("alice", "bob");
        mgr.get_or_create_shared(dm_sid, "dm:alice:bob");

        // "charlie" is not a participant -- should not see the DM.
        let with_dms = mgr.list_active_with_dms(agent_id, "charlie");
        assert_eq!(with_dms.len(), 1, "Charlie should only see web-chat");
        assert_eq!(with_dms[0].context_id, "web-chat");
    }

    #[test]
    fn test_list_active_with_dms_skips_prefix_segment() {
        // Regression: an agent hypothetically named "dm" should NOT match
        // every DM session via the "dm:" prefix segment.  The `.skip(1)`
        // in list_active_with_dms ensures we only match participant names.
        let mgr = make_manager();
        let agent_id = AgentId::new();

        mgr.get_or_create(agent_id, "web-chat");

        let dm_sid = SessionId::deterministic_dm("alice", "bob");
        mgr.get_or_create_shared(dm_sid, "dm:alice:bob");

        // Searching for "dm" as the agent name should not match "dm:alice:bob".
        let with_dms = mgr.list_active_with_dms(agent_id, "dm");
        assert_eq!(
            with_dms.len(),
            1,
            "Agent named 'dm' should not match DM prefix segment"
        );
        assert_eq!(with_dms[0].context_id, "web-chat");
    }

    #[test]
    fn test_get_or_create_with_id_preserves_session_id() {
        // Happy-path: calling get_or_create_with_id with a specific SessionId
        // should create a session that carries that exact ID and the correct
        // agent_id / context_id.
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let predetermined_id = SessionId::deterministic("notifications:test-agent");

        let session = mgr
            .get_or_create_with_id(predetermined_id, agent_id, "notifications:test-agent")
            .unwrap();

        assert_eq!(
            session.id, predetermined_id,
            "Session ID must match the caller-provided value"
        );
        assert_eq!(session.agent_id, agent_id);
        assert_eq!(session.context_id, "notifications:test-agent");
        assert_eq!(session.status, SessionStatus::Active);

        // Also verify it is retrievable via the standard get() path.
        let fetched = mgr
            .get(predetermined_id)
            .expect("session should be findable by ID");
        assert_eq!(fetched.id, predetermined_id);
    }

    #[test]
    fn failed_predetermined_session_persistence_is_never_published_or_reloaded() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("failed-predetermined-session.db");
        let db_path = db_path.to_str().unwrap();
        let agent_id = AgentId::new();
        let session_id = SessionId::deterministic("notifications:durable-test");

        {
            let mgr = SessionManager::with_sqlite(SessionConfig::default(), db_path).unwrap();
            mgr.store()
                .unwrap()
                .inject_session_insert_failure_for_test();
            let error = mgr
                .get_or_create_with_id(session_id, agent_id, "notifications:durable-test")
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected session persistence failure")
            );
            assert!(
                mgr.get(session_id).is_err(),
                "a failed durable insert must not publish an in-memory session"
            );
        }

        let reloaded = SessionManager::with_sqlite(SessionConfig::default(), db_path).unwrap();
        assert!(reloaded.get(session_id).is_err());
    }

    #[test]
    fn test_get_or_create_with_id_idempotent_with_get_or_create() {
        // Idempotency: pre-creating a session with get_or_create_with_id, then
        // calling the regular get_or_create with the same (agent_id, context_id)
        // key should return the SAME session (with the predetermined ID), not a
        // new one.
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let predetermined_id = SessionId::deterministic("notifications:test-agent");

        // Pre-create with a specific ID.
        let first = mgr
            .get_or_create_with_id(predetermined_id, agent_id, "notifications:test-agent")
            .unwrap();
        assert_eq!(first.id, predetermined_id);

        // Regular get_or_create with the same key should find the existing session.
        let second = mgr.get_or_create(agent_id, "notifications:test-agent");
        assert_eq!(
            second.id, predetermined_id,
            "get_or_create must return the pre-created session, not a new one"
        );
        assert_eq!(first.id, second.id);

        // Only one session should exist in total.
        let all = mgr.list_all();
        assert_eq!(all.len(), 1, "Only one session should exist");
    }

    #[test]
    fn test_find_last_message_returns_last_match() {
        let mgr = make_manager();
        let agent_id = AgentId::new();
        let session = mgr.get_or_create(agent_id, "ctx-find");

        mgr.append_message(session.id, make_msg("first")).unwrap();
        mgr.append_message(session.id, make_msg("second")).unwrap();
        mgr.append_message(session.id, make_msg("third")).unwrap();

        // Should find "third" — the last message matching the predicate.
        let found = mgr.find_last_message(
            session.id,
            |m| matches!(&m.content, Content::Text(t) if t.contains("ird")),
        );
        assert!(found.is_some());
        assert!(matches!(&found.unwrap().content, Content::Text(t) if t == "third"));

        // Should find "first" when only the first message matches.
        let found = mgr.find_last_message(
            session.id,
            |m| matches!(&m.content, Content::Text(t) if t == "first"),
        );
        assert!(found.is_some());
        assert!(matches!(&found.unwrap().content, Content::Text(t) if t == "first"));

        // Should return None when nothing matches.
        let found = mgr.find_last_message(session.id, |_| false);
        assert!(found.is_none());
    }

    #[test]
    fn test_find_last_message_nonexistent_session() {
        let mgr = make_manager();
        let bogus = SessionId::new();

        // Non-existent session should return None (not panic).
        let found = mgr.find_last_message(bogus, |_| true);
        assert!(found.is_none());
    }

    // -----------------------------------------------------------------------
    // named_subagent_key (#1278)
    // -----------------------------------------------------------------------

    fn register_agent(mgr: &SessionManager, name: &str) -> AgentId {
        let record = alms_core::AgentRecord {
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
        };
        mgr.store().unwrap().create_agent(&record).unwrap();
        record.id
    }

    /// The point of #1278: a named subagent's session is filed under the
    /// INVOKED agent's registry id, so it lands in that agent's own
    /// timeline instead of under an id nothing can look up.
    #[test]
    fn named_subagent_key_files_under_the_invoked_agents_registry_id() {
        let mgr = make_manager();
        let parent = AgentId::new();
        let reviewer = register_agent(&mgr, "reviewer");

        let (agent_id, context_id) = mgr.named_subagent_key(parent, "reviewer");

        assert_eq!(agent_id, reviewer);
        assert_ne!(
            agent_id,
            AgentId::deterministic(parent, "reviewer"),
            "the pre-#1278 derived id must no longer be the filing key"
        );
        assert_eq!(context_id, format!("subagent_{}_reviewer", parent.0));
    }

    /// The `context_id` is load-bearing for the parent-ownership check in
    /// `read_subagent_session` and for the sidebar's owner label, and #1278
    /// deliberately did not touch it. Two different parents invoking the
    /// same registered agent therefore share the agent id and are kept
    /// apart by the context — one row each, both in reviewer's timeline.
    #[test]
    fn named_subagent_key_keeps_parents_apart_by_context_not_by_agent_id() {
        let mgr = make_manager();
        let parent_a = AgentId::new();
        let parent_b = AgentId::new();
        let reviewer = register_agent(&mgr, "reviewer");

        let (id_a, ctx_a) = mgr.named_subagent_key(parent_a, "reviewer");
        let (id_b, ctx_b) = mgr.named_subagent_key(parent_b, "reviewer");

        assert_eq!(id_a, reviewer);
        assert_eq!(id_b, reviewer);
        assert_ne!(ctx_a, ctx_b);
        assert_eq!(
            alms_core::parse_subagent_parent(&ctx_a),
            Some(parent_a),
            "the ownership check reads the parent out of these bytes"
        );
        assert_eq!(alms_core::parse_subagent_parent(&ctx_b), Some(parent_b));
    }

    /// `invoke_agent` validates the SHAPE of a subagent name, not its
    /// presence in the registry — an unregistered name runs on defaults.
    /// There is no registry id to file it under, so the key stays on the
    /// pre-#1278 derived id rather than inventing one.
    #[test]
    fn named_subagent_key_falls_back_to_the_derived_id_for_an_unregistered_name() {
        let mgr = make_manager();
        let parent = AgentId::new();

        let (agent_id, context_id) = mgr.named_subagent_key(parent, "never-registered");

        assert_eq!(agent_id, AgentId::deterministic(parent, "never-registered"));
        assert_eq!(
            context_id,
            format!("subagent_{}_never-registered", parent.0)
        );
    }

    /// A store-less manager has no registry to consult at all. Same answer
    /// as an unregistered name — never a panic, never a fresh id.
    #[test]
    fn named_subagent_key_falls_back_without_a_store() {
        let mgr = SessionManager::new(SessionConfig::default());
        let parent = AgentId::new();

        assert_eq!(
            mgr.named_subagent_key(parent, "reviewer"),
            (
                AgentId::deterministic(parent, "reviewer"),
                format!("subagent_{}_reviewer", parent.0)
            )
        );
    }

    /// The key is a function of the REGISTRY, never of what is already
    /// stored. Pinned because the tempting alternative — "keep whichever
    /// key already has a session" — would make a named subagent's identity
    /// depend on invocation order, and it is exactly the shape a reader
    /// would reach for on seeing the orphan below.
    ///
    /// There is no migration re-homing pre-#1278 rows: a session filed
    /// under the derived id before its agent was registered is left where
    /// it is and stops being reachable by name. That break is deliberate
    /// and accepted (ALMS has no production deployments).
    #[test]
    fn named_subagent_key_follows_the_registry_even_past_an_existing_session() {
        let mgr = make_manager();
        let parent = AgentId::new();
        let derived_id = AgentId::deterministic(parent, "reviewer");
        let context_id = format!("subagent_{}_reviewer", parent.0);

        // Invoked while unregistered: the session lands on the derived key.
        let orphaned = mgr.get_or_create(derived_id, &context_id);

        // The agent is registered afterwards.
        let reviewer = register_agent(&mgr, "reviewer");
        assert_ne!(reviewer, derived_id);

        let key = mgr.named_subagent_key(parent, "reviewer");
        assert_eq!(key, (reviewer, context_id));
        assert_ne!(
            mgr.get_or_create(key.0, &key.1).id,
            orphaned.id,
            "the next invocation starts a fresh session under the registry id"
        );
    }

    /// The key is stable across repeated lookups — creating the session in
    /// between must not change the answer (the corollary of the rule above,
    /// stated from the direction callers actually hit it).
    #[test]
    fn named_subagent_key_is_stable_across_repeated_lookups() {
        let mgr = make_manager();
        let parent = AgentId::new();
        register_agent(&mgr, "reviewer");

        let first = mgr.named_subagent_key(parent, "reviewer");
        let session = mgr.get_or_create(first.0, &first.1);
        let second = mgr.named_subagent_key(parent, "reviewer");

        assert_eq!(first, second);
        assert_eq!(mgr.get_or_create(second.0, &second.1).id, session.id);
    }

    /// An unreadable registry is not the same event as an absent agent, and
    /// the two must not converge on one silent `None` (#1241/#1246). They do
    /// share a *disposition* — the derived fallback, which is why nothing is
    /// corrupted — and this pins that specifically: a store failure must
    /// yield the same key an unregistered name yields, never a fresh id.
    ///
    /// The part that is not pinnable here is the `warn!`: `alms-session` has
    /// no capture harness, and the fault it names is real but cheap — one
    /// invocation forks onto the fallback key while its neighbours resolve
    /// the registry id, so a named subagent's session splits in two for as
    /// long as the store is unwell.
    #[test]
    fn named_subagent_key_falls_back_to_the_derived_id_when_the_registry_is_unreadable() {
        let mgr = make_manager();
        let parent = AgentId::new();
        register_agent(&mgr, "reviewer");

        let healthy = mgr.named_subagent_key(parent, "reviewer");
        assert_ne!(
            healthy.0,
            AgentId::deterministic(parent, "reviewer"),
            "control: with a readable registry the key is the registry id"
        );

        // `load_agent_by_name` now returns Err, not Ok(None).
        mgr.store().unwrap().drop_agents_table_for_test().unwrap();

        let degraded = mgr.named_subagent_key(parent, "reviewer");
        assert_eq!(
            degraded,
            (
                AgentId::deterministic(parent, "reviewer"),
                format!("subagent_{}_reviewer", parent.0)
            ),
            "an unreadable registry falls back to the derived id — the same answer an \
             unregistered name gives, so the fork is recoverable rather than a fresh identity"
        );
        assert_eq!(
            degraded.1, healthy.1,
            "the context_id is independent of the registry, so the parent-ownership check \
             that reads it is unaffected by the degradation"
        );
    }
}
