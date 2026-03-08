//! Persistent store for sessions
//!
//! For MVP: in-memory only with periodic snapshots.
//! Future: Redis, PostgreSQL, or S3 backends.

use crate::{Message, Session};
use alms_core::{AlmsResult, SessionId};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotEnvelope {
    version: u32,
    checksum: String,
    snapshot: Snapshot,
}

/// In-memory store with snapshot persistence
#[derive(Debug)]
pub struct MemoryStore {
    snapshot_path: Option<PathBuf>,
    sessions: RwLock<HashMap<SessionId, Session>>,
    messages: RwLock<HashMap<SessionId, Vec<Message>>>,
    /// Mutex guards the one-time snapshot load — holds the lock during I/O to
    /// prevent two concurrent callers from both seeing `false` and double-loading.
    loaded: Mutex<bool>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            snapshot_path: None,
            sessions: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            loaded: Mutex::new(false),
        }
    }

    pub fn with_snapshot<P: AsRef<Path>>(path: P) -> Self {
        Self {
            snapshot_path: Some(path.as_ref().to_path_buf()),
            sessions: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
            loaded: Mutex::new(false),
        }
    }

    fn load_snapshot(&self) -> AlmsResult<()> {
        let mut loaded = self.loaded.lock();
        if *loaded {
            return Ok(());
        }
        *loaded = true;

        let Some(path) = &self.snapshot_path else {
            return Ok(());
        };

        let candidates = Self::snapshot_candidates(path);
        for candidate in candidates {
            if let Ok(snapshot) = Self::read_snapshot(&candidate) {
                *self.sessions.write() = snapshot.sessions;
                *self.messages.write() = snapshot.messages;
                return Ok(());
            }
        }

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

        let snapshot_bytes = serde_json::to_vec(&snapshot)?;
        let checksum = Self::checksum(&snapshot_bytes);
        let envelope = SnapshotEnvelope {
            version: 1,
            checksum,
            snapshot,
        };

        let data = serde_json::to_vec_pretty(&envelope)?;
        Self::rotate_snapshots(path)?;
        Self::atomic_write(path, &data)?;

        Ok(())
    }

    fn snapshot_candidates(path: &Path) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        candidates.push(path.to_path_buf());
        for i in 1..=3 {
            let mut rotated = path.to_path_buf();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                rotated.set_file_name(format!("{}.{}", name, i));
                candidates.push(rotated);
            }
        }
        candidates
    }

    fn read_snapshot(path: &Path) -> AlmsResult<Snapshot> {
        if !path.exists() {
            return Err(alms_core::AlmsError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "snapshot not found",
            )));
        }

        let data = std::fs::read(path)?;
        let envelope: SnapshotEnvelope = serde_json::from_slice(&data)?;
        let snapshot_bytes = serde_json::to_vec(&envelope.snapshot)?;
        let checksum = Self::checksum(&snapshot_bytes);
        if checksum != envelope.checksum {
            return Err(alms_core::AlmsError::InvalidConfig(
                "corrupt snapshot (checksum mismatch)".to_string(),
            ));
        }

        Ok(envelope.snapshot)
    }

    fn checksum(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hex::encode(hasher.finalize())
    }

    fn rotate_snapshots(path: &Path) -> AlmsResult<()> {
        for i in (1..=3).rev() {
            let mut src = path.to_path_buf();
            let mut dst = path.to_path_buf();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("snapshot.json");
            if i == 1 {
                src.set_file_name(name);
            } else {
                src.set_file_name(format!("{}.{}", name, i - 1));
            }
            dst.set_file_name(format!("{}.{}", name, i));
            if src.exists() {
                let _ = std::fs::rename(&src, &dst);
            }
        }
        Ok(())
    }

    fn atomic_write(path: &Path, data: &[u8]) -> AlmsResult<()> {
        let tmp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(data)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        // Best-effort directory fsync for durability (no-op on Windows where
        // FlushFileBuffers on a directory requires write access).
        if let Some(parent) = path.parent()
            && let Ok(dir) = std::fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
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
        self.messages.write().insert(session_id, messages.to_vec());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_snapshot_path(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("alms-test-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        dir.push(name);
        dir
    }

    #[tokio::test]
    async fn test_snapshot_roundtrip() {
        let path = temp_snapshot_path("snapshot.json");
        let store = MemoryStore::with_snapshot(&path);
        let session = Session::new(alms_core::AgentId::new(), "ctx");
        store.save_session(&session).await.unwrap();

        let store2 = MemoryStore::with_snapshot(&path);
        let loaded = store2.load_session(session.id).await.unwrap();
        assert!(loaded.is_some());
    }

    #[tokio::test]
    async fn test_snapshot_fallback_rotation() {
        let path = temp_snapshot_path("snapshot.json");
        let store = MemoryStore::with_snapshot(&path);
        let session1 = Session::new(alms_core::AgentId::new(), "ctx1");
        store.save_session(&session1).await.unwrap();

        let session2 = Session::new(alms_core::AgentId::new(), "ctx2");
        store.save_session(&session2).await.unwrap();

        // Corrupt the main snapshot
        fs::write(&path, b"corrupt").unwrap();

        let store2 = MemoryStore::with_snapshot(&path);
        let loaded1 = store2.load_session(session1.id).await.unwrap();
        assert!(loaded1.is_some());
    }
}
