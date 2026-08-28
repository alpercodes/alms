//! Agent workspace — persistent identity files.
//!
//! Each agent has a workspace directory containing:
//! - personality.md — tone, style, constraints (describes the *agent*)
//! - goals.md — current objectives (agent + user editable)
//! - memories.md — learned facts, domain knowledge (agent + user editable)
//! - user.md — who the user is: name, preferences, background (agent + user editable)
//!
//! These are read at the start of each run and injected into the system prompt.
//! The agent can update goals.md, memories.md, and user.md via the workspace_write tool.

use alms_core::{AlmsError, AlmsResult, truncate_to_char_boundary};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Agent workspace — reads and manages persistent agent identity files.
#[derive(Debug, Clone)]
pub struct AgentWorkspace {
    /// Resolved workspace directory for this agent.
    dir: PathBuf,
}

/// Files that can be read/written in the workspace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFile {
    Personality,
    Goals,
    Memories,
    User,
}

impl WorkspaceFile {
    pub fn filename(&self) -> &str {
        match self {
            WorkspaceFile::Personality => "personality.md",
            WorkspaceFile::Goals => "goals.md",
            WorkspaceFile::Memories => "memories.md",
            WorkspaceFile::User => "user.md",
        }
    }

    /// Whether the agent is allowed to write this file
    pub fn agent_writable(&self) -> bool {
        match self {
            WorkspaceFile::Personality => true,
            WorkspaceFile::Goals => true,
            WorkspaceFile::Memories => true,
            WorkspaceFile::User => true,
        }
    }

    pub fn all() -> &'static [WorkspaceFile] {
        &[
            WorkspaceFile::Personality,
            WorkspaceFile::Goals,
            WorkspaceFile::Memories,
            WorkspaceFile::User,
        ]
    }
}

impl AgentWorkspace {
    /// Create a workspace at `{base_dir}/{agent_name}/`.
    ///
    /// Standard constructor for top-level agents. Agent names are unique
    /// slug-safe identifiers, giving human-readable workspace paths.
    pub fn new(base_dir: impl Into<PathBuf>, agent_name: &str) -> Self {
        Self {
            dir: base_dir.into().join(agent_name),
        }
    }

    /// Create a workspace that uses `dir` directly as the workspace path.
    ///
    /// Used for subagents whose workspace path is already fully resolved.
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Get the workspace directory for this agent.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Ensure the workspace directory exists
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir())
    }

    /// Read a workspace file. Returns None if file doesn't exist or is empty.
    pub fn read_file(&self, file: WorkspaceFile) -> Option<String> {
        let path = self.dir().join(file.filename());
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                debug!("Read workspace file: {}", path.display());
                Some(content)
            }
            Ok(_) => None,  // empty file
            Err(_) => None, // doesn't exist
        }
    }

    /// Write a workspace file. Checks `agent_writable()` before writing.
    pub fn write_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        if !file.agent_writable() {
            return Err(AlmsError::InvalidConfig(format!(
                "{} is not agent-writable (edit it manually)",
                file.filename()
            )));
        }

        self.ensure_dir()
            .map_err(|e| AlmsError::Runtime(format!("Cannot create workspace dir: {}", e)))?;

        let path = self.dir().join(file.filename());
        std::fs::write(&path, content).map_err(|e| {
            AlmsError::Runtime(format!("Failed to write {}: {}", path.display(), e))
        })?;

        info!("Updated workspace file: {}", path.display());
        Ok(())
    }

    /// Path of the sidecar advisory lock that guards one workspace file.
    ///
    /// The lock is deliberately NOT taken on the data file itself. Windows
    /// file locks are mandatory, not advisory: while an exclusive lock is
    /// held on `memories.md`, every other handle's read of it fails with
    /// `os error 33`. [`Self::read_file`] maps any read error to `None`, so
    /// locking the data file would make an agent's memories silently
    /// disappear from the system prompt of any run that happened to build
    /// its context during an append — a worse bug than the one being fixed.
    fn lock_path(dir: &Path, file: WorkspaceFile) -> PathBuf {
        dir.join(format!(".{}.lock", file.filename()))
    }

    /// Append to a workspace file (for memories).
    ///
    /// Concurrency (#1280): a named subagent and its registered agent resolve
    /// to byte-identical workspace directories, and the coordinator's
    /// `active_named` guard deliberately allows several parents to run the
    /// same named subagent at once. Several writers can therefore be live on
    /// one `memories.md`, so this must not be a read-modify-write — an
    /// interleaved one silently drops whichever append lost the race, and the
    /// file still looks well-formed afterwards.
    ///
    /// Two independent mechanisms keep an append whole:
    ///
    /// 1. An exclusive advisory lock on a sidecar `.{file}.lock` serialises
    ///    the whole observe-then-write cycle across threads AND processes
    ///    (`flock` on Unix, `LockFileEx` on Windows; both conflict between
    ///    two separate handles in one process).
    /// 2. The write itself goes to a handle opened in append mode, so it
    ///    lands at the file's *current* end even if the file grew after this
    ///    call started. Nothing already on disk is ever rewritten, so a lock
    ///    that could not be taken degrades to a possibly-misplaced separator
    ///    rather than a lost entry.
    ///
    /// One deliberate formatting difference from the old implementation:
    /// trailing blank lines already in the file are preserved rather than
    /// collapsed. Collapsing them means rewriting bytes this function is no
    /// longer allowed to touch.
    pub fn append_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        if !file.agent_writable() {
            return Err(AlmsError::InvalidConfig(format!(
                "{} is not agent-writable",
                file.filename()
            )));
        }

        self.ensure_dir()
            .map_err(|e| AlmsError::Runtime(format!("Cannot create workspace dir: {}", e)))?;

        let dir = self.dir();
        let path = dir.join(file.filename());

        // Held for the rest of the function; released when the handle drops.
        // A lock that cannot be taken (exotic filesystem, permissions) is
        // reported and stepped over rather than failing the append: the
        // append-mode write below is still non-destructive on its own.
        let _lock = match Self::acquire_lock(&dir, file) {
            Ok(handle) => Some(handle),
            Err(e) => {
                warn!(
                    "Could not lock {} for append ({}); appending unserialised",
                    path.display(),
                    e
                );
                None
            }
        };

        let mut handle = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|e| AlmsError::Runtime(format!("Failed to open {}: {}", path.display(), e)))?;

        // Separator decision, taken under the lock from the file's real tail:
        // entries are joined by exactly one newline, and a file that already
        // ends in one is not given a second.
        //
        // Failing here must not fail the append. That would drop the very
        // memory this function exists to persist, over a decision that is
        // only cosmetic — so this takes the same arm as the lock above:
        // report it, step over it, and assume a separator is needed. Worst
        // case is one spurious blank line.
        let needs_separator = handle
            .metadata()
            .map(|meta| meta.len())
            .and_then(|len| Self::needs_separator(&mut handle, len))
            .unwrap_or_else(|e| {
                warn!(
                    "Could not inspect the tail of {} ({}); appending with a separator",
                    path.display(),
                    e
                );
                true
            });

        let payload = if needs_separator {
            format!("\n{}", content)
        } else {
            content.to_string()
        };

        // Test-only interleaving seam — see `tests::run_append_interleave_hook`.
        // Sits exactly where the old read-modify-write went stale: the file
        // has been observed, the bytes have not been written yet.
        #[cfg(test)]
        tests::run_append_interleave_hook();

        handle.write_all(payload.as_bytes()).map_err(|e| {
            AlmsError::Runtime(format!("Failed to append to {}: {}", path.display(), e))
        })?;

        info!("Appended to workspace file: {}", path.display());
        Ok(())
    }

    /// Take the exclusive sidecar lock for `file`, blocking until it is free.
    fn acquire_lock(dir: &Path, file: WorkspaceFile) -> std::io::Result<std::fs::File> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(Self::lock_path(dir, file))?;
        lock.lock()?;
        Ok(lock)
    }

    /// Whether an appended entry needs a leading newline: true when the file
    /// has content that does not already end in one.
    ///
    /// `len` is a parameter rather than a local so that the gap between
    /// observing the length and reading the tail is visible to the caller —
    /// and reachable from a test. A concurrent *truncating* writer
    /// (`write_file`, or `PUT /agents/{id}/workspace/{file}` — neither takes
    /// the lock yet, #1294) can land in that gap, leaving the seek past the
    /// new end. A short read there is not an error, it means "the tail we
    /// were told about is gone": answered with a separator, which costs a
    /// blank line, rather than with an error, which costs the memory.
    fn needs_separator(handle: &mut std::fs::File, len: u64) -> std::io::Result<bool> {
        if len == 0 {
            return Ok(false);
        }
        handle.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        match handle.read(&mut last)? {
            0 => Ok(true),
            _ => Ok(last[0] != b'\n'),
        }
    }

    /// Check if this is a fresh agent (no workspace files exist)
    pub fn needs_bootstrap(&self) -> bool {
        // Bootstrap if personality.md doesn't exist
        self.read_file(WorkspaceFile::Personality).is_none()
    }

    /// Build system prompt prefix from workspace files.
    ///
    /// When `include_user` is false, `user.md` is omitted from the prefix.
    /// This saves tokens and avoids confusion in non-user-facing contexts
    /// (DM sessions, subagent runs, scheduled jobs).
    pub fn build_system_prompt_prefix(&self, include_user: bool) -> String {
        let mut parts = Vec::new();

        if let Some(personality) = self.read_file(WorkspaceFile::Personality) {
            parts.push(personality);
        }

        if let Some(goals) = self.read_file(WorkspaceFile::Goals) {
            parts.push(format!("## Current Goals\n{}", goals));
        }

        if include_user && let Some(user) = self.read_file(WorkspaceFile::User) {
            parts.push(format!("## About the User\n{}", user));
        }

        if let Some(memories) = self.read_file(WorkspaceFile::Memories) {
            // Truncate memories if too long (will be properly budgeted by ContextBuilder)
            let memories = if memories.len() > 4000 {
                format!(
                    "{}...\n[memories truncated, {} chars total]",
                    truncate_to_char_boundary(&memories, 4000),
                    memories.len()
                )
            } else {
                memories
            };
            parts.push(format!("## Memories\n{}", memories));
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n\n")
        }
    }

    /// Get the bootstrap system prompt for first-time agent setup
    pub fn bootstrap_prompt() -> &'static str {
        include_str!("../prompts/bootstrap.md").trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    fn test_workspace() -> (TempDir, AgentWorkspace) {
        let dir = TempDir::new().unwrap();
        let ws = AgentWorkspace::new(dir.path(), "test-agent");
        (dir, ws)
    }

    thread_local! {
        /// Fires once, inside [`AgentWorkspace::append_file`], between
        /// observing the target file and writing the new entry — the exact
        /// window in which the old read-modify-write went stale. Thread-local
        /// so parallel tests in this binary cannot see each other's hook, and
        /// so the seam is inert on every thread that did not arm it.
        ///
        /// A hook must not call `append_file` itself: the sidecar lock is
        /// held at that point and conflicts per open handle, not per process.
        static APPEND_INTERLEAVE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
            const { RefCell::new(None) };
    }

    /// Arm the interleaving seam for this thread's next append.
    fn on_next_append(hook: impl FnOnce() + 'static) {
        APPEND_INTERLEAVE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// Called by `append_file`. Takes the hook out before running it, so it
    /// fires once and the `RefCell` borrow is not held across the callback.
    pub(super) fn run_append_interleave_hook() {
        let hook = APPEND_INTERLEAVE_HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    #[test]
    fn test_needs_bootstrap_fresh() {
        let (_dir, ws) = test_workspace();
        assert!(ws.needs_bootstrap());
    }

    #[test]
    fn test_needs_bootstrap_with_personality() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am a helpful coding assistant.",
        )
        .unwrap();
        assert!(!ws.needs_bootstrap());
    }

    #[test]
    fn test_write_and_read() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Goals, "Build the thing")
            .unwrap();
        assert_eq!(
            ws.read_file(WorkspaceFile::Goals).unwrap(),
            "Build the thing"
        );
    }

    #[test]
    fn test_personality_writable() {
        // personality.md is agent-writable so the bootstrap interview can save it.
        let (_dir, ws) = test_workspace();
        let result = ws.write_file(
            WorkspaceFile::Personality,
            "I am a concise coding assistant.",
        );
        assert!(result.is_ok());
        assert_eq!(
            ws.read_file(WorkspaceFile::Personality).unwrap(),
            "I am a concise coding assistant."
        );
    }

    #[test]
    fn test_append() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "Fact 1").unwrap();
        ws.append_file(WorkspaceFile::Memories, "Fact 2").unwrap();
        let content = ws.read_file(WorkspaceFile::Memories).unwrap();
        assert!(content.contains("Fact 1"));
        assert!(content.contains("Fact 2"));
    }

    #[test]
    fn test_build_system_prompt_prefix_empty() {
        let (_dir, ws) = test_workspace();
        assert!(ws.build_system_prompt_prefix(true).is_empty());
    }

    #[test]
    fn test_build_system_prompt_prefix_with_files() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am concise and technical.",
        )
        .unwrap();
        ws.write_file(WorkspaceFile::Goals, "Help with Rust")
            .unwrap();
        ws.write_file(WorkspaceFile::User, "Name: Alper. Prefers concise answers.")
            .unwrap();

        let prefix = ws.build_system_prompt_prefix(true);
        assert!(prefix.contains("concise and technical"));
        assert!(prefix.contains("Help with Rust"));
        assert!(prefix.contains("About the User"));
        assert!(prefix.contains("Alper"));
    }

    #[test]
    fn test_build_system_prompt_prefix_skip_user() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am concise and technical.",
        )
        .unwrap();
        ws.write_file(WorkspaceFile::Goals, "Help with Rust")
            .unwrap();
        ws.write_file(WorkspaceFile::User, "Name: Alper. Prefers concise answers.")
            .unwrap();

        let prefix = ws.build_system_prompt_prefix(false);
        assert!(prefix.contains("concise and technical"));
        assert!(prefix.contains("Help with Rust"));
        // user.md should be omitted for non-user-facing sessions
        assert!(!prefix.contains("About the User"));
        assert!(!prefix.contains("Alper"));
    }

    #[test]
    fn test_with_dir_uses_path_directly() {
        let dir = TempDir::new().unwrap();
        let ws_dir = dir.path().join("reviewer");
        let ws = AgentWorkspace::with_dir(&ws_dir);
        // dir() should return the exact path, no UUID appended
        assert_eq!(ws.dir(), ws_dir);
        ws.write_file(WorkspaceFile::Goals, "Review code").unwrap();
        // File should be at {ws_dir}/goals.md, not {ws_dir}/{uuid}/goals.md
        assert!(ws_dir.join("goals.md").exists());
        assert_eq!(ws.read_file(WorkspaceFile::Goals).unwrap(), "Review code");
    }

    #[test]
    fn test_write_and_read_user() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::User, "Name: Alper\nStyle: concise")
            .unwrap();
        let content = ws.read_file(WorkspaceFile::User).unwrap();
        assert!(content.contains("Alper"));
    }

    // ---- #1280: append_file is atomic against concurrent writers ----------

    /// The lost update, reproduced deterministically and without a sleep: the
    /// seam fires inside `append_file` once the file has been observed but
    /// before the entry is written, and a competing writer appends there.
    ///
    /// Under the old read-modify-write, the pending whole-file `fs::write`
    /// rewound the file to the stale snapshot and the competing entry was
    /// gone — no error, and a still well-formed `memories.md`.
    #[test]
    fn append_does_not_clobber_a_write_that_lands_mid_append() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- known before either run")
            .unwrap();

        let competitor_path = ws.dir().join("memories.md");
        on_next_append(move || {
            // Stands in for a writer that does NOT hold the sidecar lock, so
            // this pins the append-mode write on its own, independently of
            // the lock. Another `append_file` blocks on the lock and can
            // never land in this window; the writers that can are the two
            // unlocked ones in-tree today — `write_file` and
            // `PUT /agents/{id}/workspace/{file}` (#1294).
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&competitor_path)
                .unwrap();
            f.write_all(b"\n- learned by the other run").unwrap();
        });

        ws.append_file(WorkspaceFile::Memories, "- learned by this run")
            .unwrap();

        let content = ws.read_file(WorkspaceFile::Memories).unwrap();
        assert!(
            content.contains("- known before either run"),
            "pre-existing memories must survive: {content:?}"
        );
        assert!(
            content.contains("- learned by the other run"),
            "an append that landed mid-call must not be rewound away: {content:?}"
        );
        assert!(
            content.contains("- learned by this run"),
            "this call's own entry must be written: {content:?}"
        );
    }

    /// The observe-then-write cycle runs under the file's sidecar lock, so a
    /// second writer cannot start its own cycle while one is in flight.
    /// Probed from inside the seam, so no thread and no sleep is involved:
    /// the lock is either held at that instant or it is not.
    #[test]
    fn append_holds_the_sidecar_lock_across_the_write() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let lock_path = AgentWorkspace::lock_path(&ws.dir(), WorkspaceFile::Memories);
        let contended = Arc::new(AtomicBool::new(false));
        let flag = contended.clone();
        on_next_append(move || {
            // A separate handle, exactly as a second writer would open it.
            // File locks conflict per open handle on both `flock` and
            // `LockFileEx`, so this is a faithful probe even in-process.
            let probe = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap();
            flag.store(
                matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                Ordering::SeqCst,
            );
        });

        ws.append_file(WorkspaceFile::Memories, "- second").unwrap();

        assert!(
            contended.load(Ordering::SeqCst),
            "append_file must hold the workspace file's lock while it writes"
        );
    }

    /// The lock lives on a sidecar, never on `memories.md` itself. Windows
    /// file locks are mandatory: an exclusive lock on the data file makes
    /// every other handle's read fail with `os error 33`, and `read_file`
    /// maps a read error to `None` — so an agent's memories would silently
    /// drop out of the system prompt of any run that built its context while
    /// an append was in flight.
    #[test]
    fn memories_stay_readable_while_an_append_is_in_flight() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let reader = ws.clone();
        let seen = Arc::new(std::sync::Mutex::new(None));
        let sink = seen.clone();
        on_next_append(move || {
            *sink.lock().unwrap() = Some(reader.read_file(WorkspaceFile::Memories));
        });

        ws.append_file(WorkspaceFile::Memories, "- second").unwrap();

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            Some(Some("- first".to_string())),
            "a concurrent reader must still see the memories mid-append"
        );
    }

    /// The portable half of the test above: whatever else is true, the DATA
    /// file must carry no lock at all while an append is in flight. Advisory
    /// on Linux and mandatory on Windows, but a held lock is *observable* on
    /// both — `append_holds_the_sidecar_lock_across_the_write` demonstrates
    /// that same probe going green on ubuntu. So unlike the test above, this
    /// one kills "lock the data file instead of the sidecar" on CI too.
    #[test]
    fn the_data_file_itself_is_never_locked_during_an_append() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let data_path = ws.dir().join("memories.md");
        let unlocked = Arc::new(AtomicBool::new(false));
        let flag = unlocked.clone();
        on_next_append(move || {
            // `File::open` succeeds against a held `LockFileEx` range on
            // Windows — a lock blocks I/O, not opening — and a shared lock
            // needs only read access, so this is sound on both platforms.
            let probe = std::fs::File::open(&data_path).unwrap();
            flag.store(probe.try_lock_shared().is_ok(), Ordering::SeqCst);
        });

        ws.append_file(WorkspaceFile::Memories, "- second").unwrap();

        assert!(
            unlocked.load(Ordering::SeqCst),
            "memories.md must never be locked during an append: read_file maps \
             a read error to None, so a locked data file silently empties an \
             agent's memories out of the system prompt"
        );
    }

    /// A truncating writer that lands between the length observation and the
    /// tail read leaves the seek past the new end of the file. The short read
    /// that follows must not fail the append: dropping a memory over a
    /// cosmetic separator decision is the exact failure mode this change
    /// exists to prevent. It answers "separator needed" instead.
    #[test]
    fn a_tail_that_vanished_under_us_is_not_an_error() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();
        let mut handle = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(ws.dir().join("memories.md"))
            .unwrap();

        // The length a concurrent truncation has already invalidated.
        let stale_len = 4096;

        assert!(
            AgentWorkspace::needs_separator(&mut handle, stale_len)
                .expect("a tail that vanished under us must not be an error"),
            "a vanished tail must be answered with a separator, not a run-on line"
        );
    }

    /// Acceptance for #1280: several live writers on ONE workspace directory
    /// — the shape the coordinator actually produces, since a named subagent
    /// and its registered agent resolve to byte-identical paths and
    /// `active_named` deliberately lets different parents run the same named
    /// subagent at once. Every append must survive, exactly once.
    #[test]
    fn concurrent_writers_on_one_workspace_dir_lose_nothing() {
        const WRITERS: usize = 6;
        const PER_WRITER: usize = 25;

        let dir = TempDir::new().unwrap();
        let start = Arc::new(std::sync::Barrier::new(WRITERS));

        let handles: Vec<_> = (0..WRITERS)
            .map(|writer| {
                // Each writer resolves the workspace independently and lands
                // on the same directory — the collision from the issue.
                let ws = AgentWorkspace::new(dir.path(), "reviewer");
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for entry in 0..PER_WRITER {
                        ws.append_file(
                            WorkspaceFile::Memories,
                            &format!("- writer {writer} entry {entry}"),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = AgentWorkspace::new(dir.path(), "reviewer")
            .read_file(WorkspaceFile::Memories)
            .unwrap();
        let lines: std::collections::HashSet<&str> = content.lines().collect();
        for writer in 0..WRITERS {
            for entry in 0..PER_WRITER {
                let expected = format!("- writer {writer} entry {entry}");
                assert!(
                    lines.contains(expected.as_str()),
                    "lost or run-together append: {expected:?} ({} entries on \
                     {} lines \u{2014} equal means one was lost, fewer means two \
                     were merged onto one line)",
                    WRITERS * PER_WRITER,
                    content.lines().count(),
                );
            }
        }
        assert_eq!(
            content.lines().count(),
            WRITERS * PER_WRITER,
            "no entry may be duplicated, split, or run together with another"
        );
    }

    /// Entries are joined by exactly one newline: none in front of the first
    /// entry in a fresh file, one in front of an entry that follows content
    /// which does not end in a newline, and none added to content that
    /// already does.
    #[test]
    fn append_joins_entries_with_exactly_one_newline() {
        let (_dir, ws) = test_workspace();
        let path = ws.dir().join("memories.md");

        ws.append_file(WorkspaceFile::Memories, "- one").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "- one");

        ws.append_file(WorkspaceFile::Memories, "- two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "- one\n- two");

        ws.write_file(WorkspaceFile::Memories, "- one\n- two\n")
            .unwrap();
        ws.append_file(WorkspaceFile::Memories, "- three").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- one\n- two\n- three"
        );
    }
}
