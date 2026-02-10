/// Persistent store for sessions
/// 
/// For MVP: in-memory only with periodic snapshots
/// Future: Redis, PostgreSQL, or S3 backends

use alms_core::{AlmsResult, SessionId};
use alms_session::{Message, Session};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Store trait for session persistence
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    async fn save_session(&self, session: &Session) -> AlmsResult<()>;
    async fn load_session(&self, id: SessionId) -> AlmsResult<Option<Session>>;
    async fn save_messages(&self, session_id: SessionId, messages: &[Message]) -> AlmsResult<()>;
    async fn load_messages(&self, session_id: SessionId) -> AlmsResult<Vec<Message>>;
}

#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    sessions: HashMap<SessionId, Session>,
    messages: HashMap<SessionId, Vec<Message>>,
}

/// In-memory store with snapshot persistence
#[derive(Debug)]
pub struct MemoryStore {
    snapshot_path: Option<PathBuf>,
    sessions: RwLock<HashMap<SessionId, Session>>,
    messages: RwLock<HashMap<SessionId, Vec<Message>>>,
    loaded: AtomicBool,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            snapshot_path: None,
            sessions: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }
    
    pub fn with_snapshot<P: AsRef<Path>>(path: P) -> Self {
        Self {
            snapshot_path: Some(path.as_ref().to_path_buf()),
            sessions: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    fn load_snapshot(&self) -> AlmsResult<()> {
        if self.loaded.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let Some(path) = &self.snapshot_path else {
            return Ok(());
        };

        if !path.exists() {
            return Ok(());
        }

        let data = std::fs::read_to_string(path)?;
        let snapshot: Snapshot = serde_json::from_str(&data)?;

        *self.sessions.write() = snapshot.sessions;
        *self.messages.write() = snapshot.messages;

        Ok(())
    }

    fn persist_snapshot(&self) -> AlmsResult<()> {
        let Some(path) = &self.snapshot_path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let snapshot = Snapshot {
            sessions: self.sessions.read().clone(),
            messages: self.messages.read().clone(),
        };

        let data = serde_json::to_string_pretty(&snapshot)?;
        std::fs::write(path, data)?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl SessionStore for MemoryStore {
    async fn save_session(&self, session: &Session) -> AlmsResult<()> {
        self.load_snapshot()?;
        self.sessions.write().insert(session.id, session.clone());
        self.persist_snapshot()?;
        Ok(())
    }

    async fn load_session(&self, id: SessionId) -> AlmsResult<Option<Session>> {
        self.load_snapshot()?;
        Ok(self.sessions.read().get(&id).cloned())
    }

    async fn save_messages(&self, session_id: SessionId, messages: &[Message]) -> AlmsResult<()> {
        self.load_snapshot()?;
        self.messages
            .write()
            .insert(session_id, messages.to_vec());
        self.persist_snapshot()?;
        Ok(())
    }

    async fn load_messages(&self, session_id: SessionId) -> AlmsResult<Vec<Message>> {
        self.load_snapshot()?;
        Ok(self
            .messages
            .read()
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }
}
