pub mod store;
pub mod types;

pub use store::{MemoryStore, SessionStore};
pub use types::{Content, Message, Role, Session, SessionConfig, SessionStatus};

use alms_core::{AgentId, AlmsResult, SessionId};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};
use types::SessionConfig;

/// Session manager - owns all session state
#[derive(Debug)]
pub struct SessionManager {
    /// Active sessions: (agent_id, context_id) -> Session
    sessions: Arc<DashMap<(AgentId, String), Session>>,
    /// Session history: session_id -> Vec<Message>
    history: Arc<DashMap<SessionId, Vec<Message>>>,
    /// Configuration
    config: SessionConfig,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            config,
        }
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
        self.sessions.insert(key, session.clone());
        self.history.insert(session.id, Vec::new());
        info!("Created new session: {:?}", session.id);
        
        session
    }
    
    /// Get a session by ID
    pub fn get(&self, session_id: SessionId) -> AlmsResult<Session> {
        for entry in self.sessions.iter() {
            if entry.value().id == session_id {
                return Ok(entry.value().clone());
            }
        }
        Err(alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
    }
    
    /// Append a message to a session
    pub fn append_message(&self, session_id: SessionId, message: Message) -> AlmsResult<()> {
        if let Some(mut history) = self.history.get_mut(&session_id) {
            history.push(message);
            
            // Update last activity
            for mut entry in self.sessions.iter_mut() {
                if entry.value().id == session_id {
                    entry.value_mut().touch();
                    break;
                }
            }
            
            Ok(())
        } else {
            Err(alms_core::AlmsError::SessionNotFound(session_id.0.to_string()))
        }
    }
    
    /// Get session history
    pub fn get_history(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        self.history
            .get(&session_id)
            .map(|h| h.clone())
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
            let idle = types::Timestamp::now().0 - session.last_activity.0;
            
            if idle > chrono::Duration::from_std(timeout).unwrap_or_default() 
                && session.status == types::SessionStatus::Active {
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