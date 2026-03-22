pub mod job_store;
pub mod sqlite;
pub mod store;
pub mod types;

pub use alms_core::AuditEvent;
pub use job_store::JobStore;
pub use sqlite::SqliteStore;
pub use store::{MemoryStore, SessionStore};
pub use types::{Content, ContextSummary, Message, Role, Session, SessionConfig, SessionStatus};

use alms_core::{AgentId, AlmsResult, SessionId};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

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

    /// Get session history
    pub fn get_history(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        self.history
            .get(&session_id)
            .map(|h| h.clone())
            .ok_or_else(|| alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
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

    /// List all sessions across all agents, sorted by last_activity descending.
    pub fn list_all(&self) -> Vec<Session> {
        let mut sessions: Vec<Session> = self.sessions.iter().map(|e| e.value().clone()).collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity.0));
        sessions
    }

    /// Archive idle sessions
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

    /// Get config
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Get the underlying SQLite store (if any).
    pub fn store(&self) -> Option<&Arc<SqliteStore>> {
        self.store.as_ref()
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
mod tests {
    use super::*;
    use crate::types::{Content, Message, Role};

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
}
