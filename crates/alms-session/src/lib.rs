pub mod job_store;
pub mod sqlite;
pub mod store;
pub mod types;

pub use alms_core::AuditEvent;
pub use job_store::JobStore;
pub use sqlite::SqliteStore;
pub use store::{MemoryStore, SessionStore};
pub use types::{Content, Message, Role, Session, SessionConfig, SessionStatus};

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
            config,
            store: None,
        }
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
        }
        if count > 0 {
            info!("Loaded {} session(s) from SQLite", count);
        }
        Ok(())
    }

    /// Get or create a session
    pub fn get_or_create(&self, agent_id: AgentId, context_id: impl Into<String>) -> Session {
        let context_id = context_id.into();
        let key = (agent_id, context_id.clone());

        if let Some(entry) = self.sessions.get(&key) {
            let session = entry.clone();
            debug!("Found existing session: {:?}", session.id);
            return session;
        }

        let session = Session::new(agent_id, context_id);
        self.session_by_id.insert(session.id, key.clone());
        self.sessions.insert(key, session.clone());
        self.history.insert(session.id, Vec::new());
        self.audit.insert(session.id, Vec::new());

        if let Some(store) = &self.store
            && let Err(e) = store.save_session(&session)
        {
            warn!("Failed to persist session {}: {}", session.id.0, e);
        }

        info!("Created new session: {:?}", session.id);
        session
    }

    /// Get a session by ID
    pub fn get(&self, session_id: SessionId) -> AlmsResult<Session> {
        if let Some(key) = self.session_by_id.get(&session_id)
            && let Some(session) = self.sessions.get(key.value())
        {
            return Ok(session.clone());
        }
        Err(alms_core::AlmsError::SessionNotFound(
            session_id.0.to_string(),
        ))
    }

    /// Append a message to a session
    pub fn append_message(&self, session_id: SessionId, message: Message) -> AlmsResult<()> {
        if let Some(mut history) = self.history.get_mut(&session_id) {
            if let Some(store) = &self.store
                && let Err(e) = store.save_message(session_id, &message)
            {
                warn!(
                    "Failed to persist message for session {}: {}",
                    session_id.0, e
                );
            }

            history.push(message);

            // Update last_activity via the reverse index — O(1), no full scan.
            if let Some(key) = self.session_by_id.get(&session_id)
                && let Some(mut session) = self.sessions.get_mut(key.value())
            {
                session.touch();
            }

            Ok(())
        } else {
            Err(alms_core::AlmsError::SessionNotFound(
                session_id.0.to_string(),
            ))
        }
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
            info!("Deleted session: {:?}", session.id);
            Ok(())
        } else {
            Err(alms_core::AlmsError::SessionNotFound(key.1))
        }
    }

    /// Get config
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }
}
