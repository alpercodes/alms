//! Read-before-write/edit guard for filesystem tools.
//!
//! `FileStateCache` tracks which files have been read during a tool execution
//! session. `FsWriteTool` and `FsEditTool` consult the cache before mutating
//! a file, rejecting operations on files that have not been inspected first.
//!
//! This prevents agents from blindly overwriting files they have not read,
//! reducing the risk of data loss from hallucinated content.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Metadata snapshot captured at the time of a successful `fs_read`.
#[derive(Debug, Clone)]
pub struct FileStateEntry {
    /// Fast hash of the file content at read time (for change detection).
    pub content_hash: u64,
    /// File mtime at the moment it was read.
    pub mtime: SystemTime,
    /// Whether the read used offset/limit (partial read).
    pub is_partial: bool,
    /// Wall-clock instant when the read occurred.
    pub read_at: Instant,
}

/// Per-run file state cache.
///
/// Created once per agent run and shared (via `Arc<FileStateCache>`) by all
/// filesystem tools executing within that run. Not persisted across runs.
#[derive(Debug)]
pub struct FileStateCache {
    entries: Mutex<HashMap<PathBuf, FileStateEntry>>,
    max_entries: usize,
}

impl FileStateCache {
    /// Create a new cache with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::with_capacity(max_entries.min(128))),
            max_entries,
        }
    }

    /// Record a successful file read.
    ///
    /// If the cache is at capacity, the oldest entry (by `read_at`) is evicted
    /// before inserting.
    pub fn record_read(
        &self,
        path: PathBuf,
        content_hash: u64,
        mtime: SystemTime,
        is_partial: bool,
    ) {
        let mut map = self.entries.lock();

        // Evict the oldest entry when we are at capacity and this is a *new* key.
        if map.len() >= self.max_entries
            && !map.contains_key(&path)
            && let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, entry)| entry.read_at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_key);
        }

        map.insert(
            path,
            FileStateEntry {
                content_hash,
                mtime,
                is_partial,
                read_at: Instant::now(),
            },
        );
    }

    /// Look up the cached state for a file.
    pub fn get(&self, path: &Path) -> Option<FileStateEntry> {
        self.entries.lock().get(path).cloned()
    }

    /// Return the number of entries currently in the cache.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

impl Default for FileStateCache {
    fn default() -> Self {
        Self::new(100)
    }
}

// ── Hashing helper ──────────────────────────────────────────────────────────

/// Compute a fast, non-cryptographic hash of `data`.
///
/// Uses the standard library's `DefaultHasher` (currently SipHash-1-3) which
/// is fast enough for our purposes and already available without extra deps.
pub fn content_hash(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

// ── Guard logic ─────────────────────────────────────────────────────────────

/// Outcome of a read-before-write guard check.
#[derive(Debug)]
pub enum GuardOutcome {
    /// The file was read and the guard is satisfied.
    Allowed,
    /// The file was not read — the operation should be rejected.
    NotRead,
    /// The file was modified since the last read — the operation should be
    /// rejected so the agent re-reads the current version.
    StaleRead {
        /// Human-readable explanation.
        reason: String,
    },
}

/// Check whether a write/edit to `path` should be allowed.
///
/// Returns [`GuardOutcome::Allowed`] when the file was read and has not been
/// externally modified since. When the file does not exist on disk (new file
/// creation), the caller should bypass this function entirely.
pub async fn check_guard(cache: &FileStateCache, resolved_path: &Path) -> GuardOutcome {
    let entry = match cache.get(resolved_path) {
        Some(e) => e,
        None => return GuardOutcome::NotRead,
    };

    // Stat the file to detect external modifications.
    let current_mtime = match tokio::fs::metadata(resolved_path).await {
        Ok(m) => match m.modified() {
            Ok(t) => t,
            Err(_) => return GuardOutcome::Allowed,
        },
        Err(_) => {
            // File was deleted since the read — still allow the write so
            // the agent can re-create it.
            return GuardOutcome::Allowed;
        }
    };

    check_guard_mtime(cache, resolved_path, &entry, current_mtime).await
}

/// Like [`check_guard`], but accepts a pre-fetched `current_mtime` to avoid a
/// redundant `metadata()` call when the caller already stat-ed the file.
pub async fn check_guard_with_mtime(
    cache: &FileStateCache,
    resolved_path: &Path,
    current_mtime: std::time::SystemTime,
) -> GuardOutcome {
    let entry = match cache.get(resolved_path) {
        Some(e) => e,
        None => return GuardOutcome::NotRead,
    };

    check_guard_mtime(cache, resolved_path, &entry, current_mtime).await
}

/// Shared mtime comparison + content-hash fallback logic.
async fn check_guard_mtime(
    _cache: &FileStateCache,
    resolved_path: &Path,
    entry: &FileStateEntry,
    current_mtime: std::time::SystemTime,
) -> GuardOutcome {
    if current_mtime == entry.mtime {
        return GuardOutcome::Allowed;
    }

    // Mtime changed — perform content-hash fallback (Windows AV / cloud sync
    // can update mtime without changing content).
    match tokio::fs::read(resolved_path).await {
        Ok(data) => {
            if content_hash(&data) == entry.content_hash {
                // Content is identical despite mtime change — allow.
                GuardOutcome::Allowed
            } else {
                GuardOutcome::StaleRead {
                    reason: "File has been modified since it was last read \
                             (possibly by another process or tool). Read it again \
                             before writing or editing."
                        .to_string(),
                }
            }
        }
        Err(_) => {
            // Cannot re-read for hash comparison — be conservative.
            GuardOutcome::StaleRead {
                reason: "File mtime changed since last read and content \
                         could not be verified. Read it again before writing or editing."
                    .to_string(),
            }
        }
    }
}

/// Update the cache entry for a file after a successful write or edit.
///
/// Re-stats the file and computes a fresh content hash so that subsequent
/// writes/edits to the same file pass the guard without requiring a re-read.
pub async fn update_cache_after_write(cache: &FileStateCache, resolved_path: &Path) {
    let Ok(raw_bytes) = tokio::fs::read(resolved_path).await else {
        return;
    };
    let mtime = tokio::fs::metadata(resolved_path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    cache.record_read(
        resolved_path.to_path_buf(),
        content_hash(&raw_bytes),
        mtime,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_record_and_get() {
        let cache = FileStateCache::new(10);
        let path = PathBuf::from("/tmp/test_file.txt");
        let hash = content_hash(b"hello world");
        let mtime = SystemTime::now();

        cache.record_read(path.clone(), hash, mtime, false);

        let entry = cache.get(&path).expect("entry should exist");
        assert_eq!(entry.content_hash, hash);
        assert_eq!(entry.mtime, mtime);
        assert!(!entry.is_partial);
    }

    #[test]
    fn test_get_missing_returns_none() {
        let cache = FileStateCache::new(10);
        assert!(cache.get(Path::new("/nonexistent")).is_none());
    }

    #[test]
    fn test_eviction_when_full() {
        let cache = FileStateCache::new(2);
        let mtime = SystemTime::now();

        cache.record_read(PathBuf::from("/a"), 1, mtime, false);
        // Small delay so read_at differs.
        std::thread::sleep(std::time::Duration::from_millis(5));
        cache.record_read(PathBuf::from("/b"), 2, mtime, false);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // This should evict /a (oldest by read_at).
        cache.record_read(PathBuf::from("/c"), 3, mtime, false);

        assert_eq!(cache.len(), 2);
        assert!(cache.get(Path::new("/a")).is_none(), "/a should be evicted");
        assert!(cache.get(Path::new("/b")).is_some());
        assert!(cache.get(Path::new("/c")).is_some());
    }

    #[test]
    fn test_update_existing_entry_does_not_evict() {
        let cache = FileStateCache::new(2);
        let mtime = SystemTime::now();

        cache.record_read(PathBuf::from("/a"), 1, mtime, false);
        cache.record_read(PathBuf::from("/b"), 2, mtime, false);

        // Update /a — should NOT evict anything since /a already exists.
        cache.record_read(PathBuf::from("/a"), 10, mtime, true);
        assert_eq!(cache.len(), 2);
        let entry = cache.get(Path::new("/a")).unwrap();
        assert_eq!(entry.content_hash, 10);
        assert!(entry.is_partial);
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash(b"hello");
        let h2 = content_hash(b"hello");
        assert_eq!(h1, h2);

        let h3 = content_hash(b"world");
        assert_ne!(h1, h3);
    }

    #[tokio::test]
    async fn test_guard_not_read() {
        let cache = FileStateCache::new(10);
        let outcome = check_guard(&cache, Path::new("/some/path")).await;
        assert!(matches!(outcome, GuardOutcome::NotRead));
    }

    #[tokio::test]
    async fn test_guard_allowed_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mtime = meta.modified().unwrap();
        let hash = content_hash(b"hello");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, mtime, false);

        let outcome = check_guard(&cache, &file_path).await;
        assert!(matches!(outcome, GuardOutcome::Allowed));
    }

    #[tokio::test]
    async fn test_guard_stale_when_content_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"original").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mtime = meta.modified().unwrap();
        let hash = content_hash(b"original");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, mtime, false);

        // Modify file content AND mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file_path)
                .unwrap();
            f.write_all(b"modified").unwrap();
        }

        let outcome = check_guard(&cache, &file_path).await;
        assert!(
            matches!(outcome, GuardOutcome::StaleRead { .. }),
            "expected StaleRead, got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn test_guard_allowed_when_mtime_changed_but_content_same() {
        // Simulates the Windows AV / cloud sync case where mtime is bumped
        // but file content is unchanged.
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"same content").unwrap();

        // Record with an artificially old mtime so the guard sees a difference.
        let old_mtime = SystemTime::UNIX_EPOCH;
        let hash = content_hash(b"same content");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, old_mtime, false);

        let outcome = check_guard(&cache, &file_path).await;
        assert!(
            matches!(outcome, GuardOutcome::Allowed),
            "expected Allowed (content hash match), got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn test_guard_allowed_when_file_deleted() {
        // If the file was deleted after reading, allow the write (re-creation).
        let cache = FileStateCache::new(10);
        let path = PathBuf::from("/tmp/this_file_does_not_exist_12345.txt");
        cache.record_read(path.clone(), 42, SystemTime::now(), false);

        let outcome = check_guard(&cache, &path).await;
        assert!(matches!(outcome, GuardOutcome::Allowed));
    }

    #[test]
    fn test_partial_read_recorded() {
        let cache = FileStateCache::new(10);
        let path = PathBuf::from("/tmp/partial.txt");
        cache.record_read(path.clone(), 99, SystemTime::now(), true);

        let entry = cache.get(&path).unwrap();
        assert!(entry.is_partial);
    }

    // ── check_guard_with_mtime tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_guard_with_mtime_not_read() {
        // File has no cache entry — should return NotRead.
        let cache = FileStateCache::new(10);
        let mtime = SystemTime::now();
        let outcome =
            check_guard_with_mtime(&cache, Path::new("/some/nonexistent/path"), mtime).await;
        assert!(
            matches!(outcome, GuardOutcome::NotRead),
            "expected NotRead for uncached path, got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn test_guard_with_mtime_allowed_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mtime = meta.modified().unwrap();
        let hash = content_hash(b"hello");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, mtime, false);

        // Pass the same mtime the file was recorded with.
        let outcome = check_guard_with_mtime(&cache, &file_path, mtime).await;
        assert!(
            matches!(outcome, GuardOutcome::Allowed),
            "expected Allowed when mtime matches, got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn test_guard_with_mtime_stale_when_content_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"original").unwrap();

        let meta = std::fs::metadata(&file_path).unwrap();
        let mtime = meta.modified().unwrap();
        let hash = content_hash(b"original");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, mtime, false);

        // Modify the file, then pass the new mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file_path)
                .unwrap();
            f.write_all(b"modified").unwrap();
        }
        let new_meta = std::fs::metadata(&file_path).unwrap();
        let new_mtime = new_meta.modified().unwrap();

        let outcome = check_guard_with_mtime(&cache, &file_path, new_mtime).await;
        assert!(
            matches!(outcome, GuardOutcome::StaleRead { .. }),
            "expected StaleRead when content differs, got {:?}",
            outcome
        );
    }

    #[tokio::test]
    async fn test_guard_with_mtime_allowed_when_mtime_differs_but_content_same() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"same content").unwrap();

        // Record with an artificially old mtime.
        let old_mtime = SystemTime::UNIX_EPOCH;
        let hash = content_hash(b"same content");

        let cache = FileStateCache::new(10);
        cache.record_read(file_path.clone(), hash, old_mtime, false);

        // Pass the real (different) mtime — content-hash fallback should allow.
        let real_meta = std::fs::metadata(&file_path).unwrap();
        let real_mtime = real_meta.modified().unwrap();

        let outcome = check_guard_with_mtime(&cache, &file_path, real_mtime).await;
        assert!(
            matches!(outcome, GuardOutcome::Allowed),
            "expected Allowed (content hash match), got {:?}",
            outcome
        );
    }
}
