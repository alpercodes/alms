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
use std::sync::atomic::{AtomicU64, Ordering};
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

    /// What `workspace_write` does when the model omits `mode` — the
    /// recorded answer to #1305, in place of a bare `.unwrap_or("write")`.
    ///
    /// Returns one of exactly the two strings the tool accepts, so it can be
    /// the `unwrap_or` for the parameter and be echoed back as the
    /// *effective* mode.
    ///
    /// **The decision.** `memories.md` defaults to `"append"`; the other
    /// three default to `"write"`.
    ///
    /// Why the split, and why it is not more locking: an agent's read of its
    /// memories is the context build, which `AgentRuntime` runs **once per
    /// run**, before `agent_loop` — the assembled system prompt is then
    /// carried through every tool iteration. So the snapshot a
    /// `workspace_write` replaces is a *run-start* snapshot, not a turn-old
    /// one, and the window is the whole run. Anything appended inside it — by
    /// another live instance of the same named agent, which the coordinator's
    /// `active_named` guard deliberately permits, **or by this very run's own
    /// earlier `workspace_write` calls** — is not in the snapshot the model is
    /// editing, so a whole-file replacement silently erases it. No lock can
    /// bracket that; there is no critical section, only a stale snapshot
    /// (#1305, the residual #1280/#1294 structurally could not reach). What
    /// *can* be changed is where an omitted `mode` lands. The three identity
    /// files each describe one settled thing and are meant to be restated;
    /// `memories.md` is a list of learned facts that accumulates, so an
    /// omitted `mode` there almost always means "add this", and the
    /// destructive reading is the wrong one to guess.
    ///
    /// **The cost, accepted deliberately.** A model that omits `mode` while
    /// genuinely intending to rewrite `memories.md` wholesale — pruning or
    /// reorganising — now appends its rewrite after the old content instead
    /// of replacing it, duplicating entries. What it replaces was an
    /// invisible, unrecoverable loss, so preserving data wins the tie either
    /// way; but the duplication is only *visible and repairable* under a
    /// precondition worth stating, because this default is what drives the
    /// file towards breaking it:
    ///
    /// > **While `memories.md` stays under the 4000-char injection cap.**
    /// > [`AgentWorkspace::build_system_prompt_prefix`] injects
    /// > `truncate_to_char_boundary(&memories, 4000)`, which is
    /// > **head-anchored** — it returns the *oldest* 4000 chars. Under an
    /// > append default the file grows at the tail while the read window
    /// > stays put, so past the cap the duplicate lands in the part that is
    /// > cut: the agent does not see it next context build, and `mode:
    /// > "write"` cannot repair it either, because the model holds only the
    /// > head and resending that is itself a destructive truncation. Past
    /// > the cap, newly appended memories also stop being injected at all.
    /// > The bytes are still on disk and still recoverable (UI, `PUT`,
    /// > `fs_read`, an operator) where a lost update is not, which is why
    /// > this does not change the decision — but head-anchored truncation is
    /// > the wrong end to cut for an append-default file, and is tracked
    /// > separately rather than solved here.
    ///
    /// The tool's parameter description tells the model both halves of the
    /// trade, and the result echoes the effective mode, so a model that
    /// guessed wrong can find out in the same turn.
    ///
    /// Rejecting the stale replacement instead — a compare-and-swap against
    /// the file's state at context-build time — was considered and not done,
    /// as the cheaper fix rather than the impossible one. A **bare** rejection
    /// would be unusable: the agent would see it (a failed tool call is
    /// persisted as its own `Error: ...` tool result and the loop continues,
    /// so it is answerable in-turn) but has no way to re-read its workspace —
    /// there is no `workspace_read` tool, only the system-prompt injection it
    /// has already consumed. A rejection **carrying the current file
    /// contents** would be usable, and is the shape any future fix should
    /// take: that payload travels on the channel above, `WorkspaceWriteTool`
    /// is constructed per run one hop from the context build, and no protocol
    /// or schema change is needed. Two things such a fix must get right — the
    /// base has to be what the *context build* read, not what tool
    /// construction saw, and the returned contents inherit the same 4000-char
    /// cap described above.
    ///
    /// Independent of [`Self::agent_writable`] (#1303), which answers
    /// *whether* the agent may write a file, not what an omitted `mode` means
    /// for one it may. A file made non-agent-writable there is rejected
    /// before this is ever consulted.
    pub fn default_write_mode(&self) -> &'static str {
        match self {
            WorkspaceFile::Memories => "append",
            WorkspaceFile::Personality | WorkspaceFile::Goals | WorkspaceFile::User => "write",
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

    /// Write a workspace file, replacing whatever was there. Checks
    /// `agent_writable()` before writing.
    ///
    /// This is the branch an agent takes by default for `personality.md`,
    /// `goals.md` and `user.md` — the three files
    /// [`WorkspaceFile::default_write_mode`] answers `"write"` for, so an LLM
    /// that omits `mode` on one of them lands here rather than in
    /// [`Self::append_file`] (#1294). `memories.md` no longer does: it
    /// defaults to append, because the content an omitted `mode` replaces
    /// there is the context build's snapshot, which is as old as the run
    /// (#1305). Reaching this function for `memories.md` now takes an
    /// explicit `mode: "write"` — which is still the right thing for an
    /// agent deliberately compacting its memories, and still carries the
    /// staleness, now knowingly.
    pub fn write_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        if !file.agent_writable() {
            return Err(AlmsError::InvalidConfig(format!(
                "{} is not agent-writable (edit it manually)",
                file.filename()
            )));
        }

        self.replace_file(file, content)
    }

    /// Write a workspace file on the operator's authority, skipping the
    /// `agent_writable()` check.
    ///
    /// `PUT /agents/{id}/workspace/{file}` is allowed to write every
    /// workspace file, including any the agent itself may not — the operator
    /// is the authority on their own workspace. That exemption is about
    /// **permission** and nothing else: the write still takes the same lock
    /// and lands the same way as [`Self::write_file`]. Which is the point of
    /// this method existing — before #1294 the handler waived the check by
    /// going around `AgentWorkspace` entirely, and waived the atomicity with
    /// it.
    ///
    /// Caveat worth knowing before reading too much into the split:
    /// `agent_writable()` returns `true` for all four files today, so this
    /// and [`Self::write_file`] are behaviourally identical and no test can
    /// tell them apart (#1303). The split is structural, and it is the
    /// conservative direction: if `personality.md` ever does become
    /// non-agent-writable, an operator route that had been calling
    /// `write_file` would start returning 500 on every personality edit made
    /// from the UI.
    pub fn write_file_as_operator(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        self.replace_file(file, content)
    }

    /// Replace a workspace file's contents: serialised against every other
    /// writer, and never observable half-done by a reader (#1294).
    ///
    /// Two properties, both needed:
    ///
    /// 1. The file's sidecar lock is held across the whole call — the same
    ///    lock [`Self::append_file`] takes, so the two serialise. Without it
    ///    a replacement landing inside an append's observe-then-write cycle
    ///    truncates the file under it: the append then lands at offset 0 and
    ///    the replacement overwrites it, leaving a well-formed file with the
    ///    memory silently gone. That is the #1280 failure mode, reached
    ///    through the tool's replacing branch — which was `memories.md`'s
    ///    default until #1305 moved it to append, and is still one explicit
    ///    `mode: "write"` away.
    /// 2. The new content is staged beside the target and moved into place
    ///    with a rename, instead of being written into a truncated target.
    ///    `std::fs::write` opens with `O_TRUNC`, so the file is *empty* for
    ///    the length of the write, and [`Self::read_file`] maps both a read
    ///    error and an empty file to "no memories" — an agent's whole
    ///    memory silently missing from the system prompt of any run that
    ///    built its context in that window. A rename swaps the directory
    ///    entry, so a concurrent reader sees the whole old content or the
    ///    whole new content and never anything in between. This is also why
    ///    the lock alone would not be enough: readers do not take it, by the
    ///    same reasoning that put the lock on a sidecar in the first place
    ///    (see [`Self::lock_path`]).
    ///
    /// The rename buys visibility, not crash durability: nothing is fsynced,
    /// so power loss mid-call can still lose the new content — but it cannot
    /// leave a torn file, and a crash between the two steps leaves nothing
    /// behind but a stray staging file.
    ///
    /// A lock that cannot be taken **fails** this write, where the same
    /// failure only warns in [`Self::append_file`]. The asymmetry is the
    /// point, not an oversight: #1292 could step over a missing lock because
    /// an append-mode write is non-destructive on its own, so the degraded
    /// path provably cost at most a misplaced separator. No such proof
    /// exists here — an unserialised replacement can rename the file out
    /// from under an append that had already opened its handle (the lock is
    /// taken, then the handle is opened, then `needs_separator` seeks and
    /// reads: a real window, several syscalls wide). The old inode is
    /// unlinked, the appender writes into it, and those bytes are freed when
    /// the handle closes. **Both calls return `Ok`** — the `write_all`
    /// succeeded, the rename succeeded, nothing warns — and one of them
    /// wrote where nobody can ever read. Not overwritten: gone. That is the
    /// defect this function exists to prevent, so it cannot also be its
    /// degraded mode. Failing costs a retry and nothing else: the old
    /// content is untouched and the caller still holds the new content.
    /// Which is the same trade the paragraph below makes about the rename,
    /// decided the same way.
    ///
    /// One accepted regression, on Windows only: `MoveFileEx` needs delete
    /// access to the target, which an outside process holding it open
    /// without `FILE_SHARE_DELETE` denies, so a rename can fail where the
    /// truncating write would have succeeded. That is returned as an error
    /// rather than falling back to a truncating write — a visible failure
    /// the caller can retry beats an invisible torn read.
    fn replace_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        self.ensure_dir()
            .map_err(|e| AlmsError::Runtime(format!("Cannot create workspace dir: {}", e)))?;

        let dir = self.dir();
        let path = dir.join(file.filename());

        // Held for the rest of the function, and a hard precondition —
        // deliberately NOT `append_file`'s warn-and-step-over. See the
        // asymmetry note on this function.
        let _lock = Self::acquire_lock(&dir, file).map_err(|e| {
            AlmsError::Runtime(format!(
                "Refusing to replace {} without its lock: {}",
                path.display(),
                e
            ))
        })?;

        let staging = Self::staging_path(&dir, file);
        std::fs::write(&staging, content).map_err(|e| {
            let _ = std::fs::remove_file(&staging);
            AlmsError::Runtime(format!(
                "Failed to stage a replacement for {}: {}",
                path.display(),
                e
            ))
        })?;

        // Test-only interleaving seam — see `tests::run_replace_interleave_hook`.
        // The replacement is fully staged and the target has not been touched
        // yet: the instant at which a truncate-first writer would already
        // have emptied it.
        #[cfg(test)]
        tests::run_replace_interleave_hook();

        std::fs::rename(&staging, &path).map_err(|e| {
            let _ = std::fs::remove_file(&staging);
            AlmsError::Runtime(format!("Failed to write {}: {}", path.display(), e))
        })?;

        info!("Updated workspace file: {}", path.display());
        Ok(())
    }

    /// Path of the scratch file a replacement is staged in before being
    /// renamed over the target.
    ///
    /// In the same directory, because a rename is only atomic within one
    /// filesystem. Unique per call rather than a fixed `.{file}.tmp` as a
    /// second line of defence: a lock that *fails* to be taken now aborts the
    /// write, but a lock that silently does not work (a filesystem where the
    /// call succeeds without conflicting, some NFS mounts) leaves two
    /// replacements staging at once, and on a shared path they would splice
    /// into one file and rename the splice into place — corruption worse
    /// than the lost update this is fixing. Pid reuse after a crash can pick
    /// a name a dead process left behind, which costs nothing: the staging
    /// write truncates it.
    ///
    /// It is only a second line of defence, and a narrow one: on a
    /// filesystem whose locking silently no-ops, the orphaned-inode append
    /// loss described on [`Self::replace_file`] is still reachable, and
    /// unique staging paths do nothing about it — they stop two
    /// replacements splicing, not a replacement outrunning an append.
    /// Nothing here should try to close that. No lock discipline fixes a
    /// lock that lies; #1292's append path carries the same residual for the
    /// same reason.
    fn staging_path(dir: &Path, file: WorkspaceFile) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        dir.join(format!(
            ".{}.{}.{}.tmp",
            file.filename(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
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
    /// and reachable from a test. Every in-tree writer now takes the sidecar
    /// lock (#1294), so none of them can land in that gap — but a writer
    /// that failed to take the lock, or an operator editing the file by hand
    /// while a run is live, still can, leaving the seek past the new end. A
    /// short read there is not an error, it means "the tail we were told
    /// about is gone": answered with a separator, which costs a blank line,
    /// rather than with an error, which costs the memory.
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

    thread_local! {
        /// The same seam as [`APPEND_INTERLEAVE_HOOK`], for
        /// [`AgentWorkspace::replace_file`]: fires once, with the lock held
        /// and the replacement fully staged, immediately before the rename
        /// puts it in place. Same constraint — a hook must not call back
        /// into a workspace writer for the same file, because the sidecar
        /// lock conflicts per open handle, not per process.
        static REPLACE_INTERLEAVE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
            const { RefCell::new(None) };
    }

    /// Arm the interleaving seam for this thread's next replacing write.
    fn on_next_replace(hook: impl FnOnce() + 'static) {
        REPLACE_INTERLEAVE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// Called by `replace_file`. See [`run_append_interleave_hook`].
    pub(super) fn run_replace_interleave_hook() {
        let hook = REPLACE_INTERLEAVE_HOOK.with(|slot| slot.borrow_mut().take());
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
            // Stands in for a writer that does NOT hold the sidecar lock,
            // so this pins the append-mode write on its own, independently
            // of the lock. Every in-tree writer takes the lock as of #1294
            // and so blocks here instead; what is left is a writer whose own
            // lock acquisition failed and stepped over it, or something
            // outside the daemon appending to the same file.
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

    // ---- #1294: the replacing writers are locked and atomic --------------

    /// The replacing write runs under the *same* sidecar lock `append_file`
    /// takes — the probe resolves the path through `lock_path`, so a write
    /// that locked nothing, or locked some other file, leaves this free.
    ///
    /// Together with `append_holds_the_sidecar_lock_across_the_write` this is
    /// the mutual exclusion the fix rests on: both writers hold that one lock
    /// across their whole observe-or-stage-then-write cycle, so neither can
    /// land inside the other's. Probed from inside the seam, so no thread and
    /// no sleep is involved — the lock is either held at that instant or
    /// it is not.
    #[test]
    fn a_replacing_write_holds_the_sidecar_lock_across_the_rename() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let lock_path = AgentWorkspace::lock_path(&ws.dir(), WorkspaceFile::Memories);
        let contended = Arc::new(AtomicBool::new(false));
        let flag = contended.clone();
        on_next_replace(move || {
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

        ws.write_file(WorkspaceFile::Memories, "- second").unwrap();

        assert!(
            contended.load(Ordering::SeqCst),
            "write_file must hold the workspace file's lock while it replaces it"
        );
    }

    /// The lock is on the sidecar, never on the data file — same reason as
    /// for appends: `read_file` maps a read error to `None`, and on Windows a
    /// held lock makes every other handle's read fail. So a reader must still
    /// see the *old* content in full while a replacement is in flight, not an
    /// error and not a truncated file.
    #[test]
    fn memories_stay_readable_while_a_replacement_is_in_flight() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let reader = ws.clone();
        let seen = Arc::new(std::sync::Mutex::new(None));
        let sink = seen.clone();
        on_next_replace(move || {
            *sink.lock().unwrap() = Some(reader.read_file(WorkspaceFile::Memories));
        });

        ws.write_file(WorkspaceFile::Memories, "- second").unwrap();

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            Some(Some("- first".to_string())),
            "the old content must still be readable in full until the \
             replacement is renamed into place"
        );
        assert_eq!(
            ws.read_file(WorkspaceFile::Memories).unwrap(),
            "- second",
            "and the new content must be there once the call returns"
        );
    }

    /// A replacement swaps the directory entry; it does not rewrite the file
    /// in place. A handle opened before the write therefore still reads the
    /// old content afterwards — which is exactly why a concurrent reader
    /// can never see a half-written file. Under a truncating `fs::write` that
    /// same handle would see the new content, because it is the same inode.
    ///
    /// Portable: an open handle keeps the replaced file alive under both
    /// `rename` and `MoveFileEx` (Rust opens files with `FILE_SHARE_DELETE`).
    #[test]
    fn a_replacement_swaps_the_file_rather_than_rewriting_it_in_place() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- before").unwrap();

        let mut held = std::fs::File::open(ws.dir().join("memories.md")).unwrap();

        ws.write_file(WorkspaceFile::Memories, "- after").unwrap();

        let mut seen = String::new();
        held.read_to_string(&mut seen).unwrap();
        assert_eq!(
            seen, "- before",
            "a handle opened before the write must still see the old content: \
             the replacement is a rename, not an in-place rewrite"
        );
        assert_eq!(ws.read_file(WorkspaceFile::Memories).unwrap(), "- after");
    }

    /// Nothing touches the target until the replacement is whole: at the
    /// instant the new content is fully staged, the file on disk is still the
    /// complete old content. Kills a "truncate the target, then stream into
    /// it" implementation, which is the shape that makes an empty file
    /// observable.
    ///
    /// It does *not* kill a single-call `std::fs::write` — the seam fires
    /// before that call, so the target is legitimately intact at the instant
    /// this looks. That one is killed by
    /// `a_replacement_swaps_the_file_rather_than_rewriting_it_in_place` and
    /// by `a_concurrent_reader_never_observes_a_partial_replacement`.
    #[test]
    fn the_target_is_untouched_until_the_replacement_is_whole() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- before").unwrap();

        let path = ws.dir().join("memories.md");
        let observed = Arc::new(std::sync::Mutex::new(None));
        let sink = observed.clone();
        on_next_replace(move || {
            *sink.lock().unwrap() = Some(std::fs::read_to_string(&path).unwrap());
        });

        ws.write_file(WorkspaceFile::Memories, "- after").unwrap();

        assert_eq!(
            observed.lock().unwrap().clone(),
            Some("- before".to_string()),
            "the target must still hold the whole old content while the \
             replacement is staged"
        );
    }

    /// Acceptance for the torn read (#1294): a reader racing a stream of
    /// replacements must only ever observe a *whole* document. Under the old
    /// `std::fs::write` the file is empty for the length of the write, and
    /// `read_file` reports that as `None` — an agent's memories silently
    /// gone from the system prompt of whichever run was building context.
    ///
    /// The documents are large so that a truncate-and-write window would be
    /// wide enough to catch; with the rename there is no window to catch at
    /// all, so this cannot fail spuriously.
    #[test]
    fn a_concurrent_reader_never_observes_a_partial_replacement() {
        const ROUNDS: usize = 60;
        const LINES: usize = 8000;

        let (_dir, ws) = test_workspace();
        let doc_a: String = (0..LINES).map(|i| format!("- alpha {i}\n")).collect();
        let doc_b: String = (0..LINES).map(|i| format!("- bravo {i}\n")).collect();
        ws.write_file(WorkspaceFile::Memories, &doc_a).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let reader_ws = ws.clone();
        let reader_stop = stop.clone();
        let reader_started = started.clone();
        let (want_a, want_b) = (doc_a.clone(), doc_b.clone());
        let reader = std::thread::spawn(move || {
            let mut reads = 0usize;
            // Reads first and checks the flag after, so a reader that only
            // gets scheduled once still reports a read and this test cannot
            // go red for having lost a race for the CPU.
            loop {
                match reader_ws.read_file(WorkspaceFile::Memories) {
                    Some(seen) if seen == want_a || seen == want_b => reads += 1,
                    Some(seen) => panic!(
                        "torn read: {} bytes, neither whole document ({} bytes)",
                        seen.len(),
                        want_a.len()
                    ),
                    None => panic!("torn read: memories.md read back empty or missing"),
                }
                reader_started.store(true, Ordering::Relaxed);
                if reader_stop.load(Ordering::Relaxed) {
                    return reads;
                }
            }
        });

        // Give the reader its first read before the writes start, so the two
        // actually overlap on a busy box. Bounded, because a test that hangs
        // is worse than one that covers less than it hoped.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !started.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }

        for round in 0..ROUNDS {
            let doc = if round % 2 == 0 { &doc_b } else { &doc_a };
            ws.write_file(WorkspaceFile::Memories, doc).unwrap();
        }
        stop.store(true, Ordering::Relaxed);

        let reads = reader
            .join()
            .expect("the reader must never observe a partial file");
        assert!(reads > 0, "the reader never actually read the file");
    }

    /// The operator route writes every workspace file without consulting
    /// `agent_writable()`, and lands each one through the same locked
    /// replacement — the sidecar it leaves behind is the evidence it did
    /// not go around the write path.
    #[test]
    fn the_operator_route_writes_every_file_through_the_locked_replacement() {
        let (_dir, ws) = test_workspace();

        for file in WorkspaceFile::all() {
            ws.write_file_as_operator(*file, "operator content")
                .unwrap();
            assert_eq!(
                ws.read_file(*file).as_deref(),
                Some("operator content"),
                "the operator must be able to write {}",
                file.filename()
            );
            assert!(
                AgentWorkspace::lock_path(&ws.dir(), *file).exists(),
                "the operator write of {} must have taken the sidecar lock",
                file.filename()
            );
        }
    }

    /// The portable half of `memories_stay_readable_while_a_replacement_is_in
    /// _flight`: whatever else is true, the DATA file must carry no lock while
    /// a replacement is in flight. A held lock is *observable* on both
    /// platforms even though only Windows lets it break reads, so unlike the
    /// test above this one kills "lock the data file instead of the sidecar"
    /// on ubuntu CI too.
    #[test]
    fn the_data_file_itself_is_never_locked_during_a_replacement() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- first").unwrap();

        let data_path = ws.dir().join("memories.md");
        let unlocked = Arc::new(AtomicBool::new(false));
        let flag = unlocked.clone();
        on_next_replace(move || {
            let probe = std::fs::File::open(&data_path).unwrap();
            flag.store(probe.try_lock_shared().is_ok(), Ordering::SeqCst);
        });

        ws.write_file(WorkspaceFile::Memories, "- second").unwrap();

        assert!(
            unlocked.load(Ordering::SeqCst),
            "memories.md must never be locked during a replacement: read_file \
             maps a read error to None, so a locked data file silently empties \
             an agent's memories out of the system prompt"
        );
    }

    /// A replacement that cannot land reports the failure and takes its
    /// scratch file with it. Forced by making the target a directory, which
    /// no rename can replace on either platform.
    #[test]
    fn a_replacement_that_cannot_land_leaves_no_scratch_behind() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::create_dir(ws.dir().join("memories.md")).unwrap();

        let err = ws
            .write_file(WorkspaceFile::Memories, "- doomed")
            .expect_err("a replacement that cannot be renamed into place must fail");
        assert!(
            err.to_string().contains("memories.md"),
            "the error must name the file it could not write: {err}"
        );

        let strays: Vec<_> = std::fs::read_dir(ws.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "a failed replacement left its scratch file behind: {strays:?}"
        );
    }

    /// A replacement that cannot take the lock does not happen. Unlike an
    /// append, which is non-destructive with or without the lock, an
    /// unserialised replacement can rename the file out from under an append
    /// that already holds a handle — so the lock is a precondition here,
    /// not a best effort, and the old content must survive the refusal
    /// intact for the caller to retry against.
    ///
    /// Forced by making the sidecar path a directory, which no
    /// `OpenOptions::open` can open for writing on either platform.
    #[test]
    fn a_replacement_refuses_to_run_without_its_lock() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        // Seeded outside the workspace API so no earlier write creates the
        // sidecar as a file first.
        std::fs::write(ws.dir().join("memories.md"), "- before").unwrap();
        std::fs::create_dir(AgentWorkspace::lock_path(
            &ws.dir(),
            WorkspaceFile::Memories,
        ))
        .unwrap();

        let err = ws
            .write_file(WorkspaceFile::Memories, "- after")
            .expect_err("a replacement that cannot be serialised must not happen");
        assert!(
            err.to_string().contains("memories.md"),
            "the error must name the file it refused to write: {err}"
        );

        assert_eq!(
            ws.read_file(WorkspaceFile::Memories).as_deref(),
            Some("- before"),
            "a refused replacement must leave the old content intact"
        );
        let strays: Vec<_> = std::fs::read_dir(ws.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "a refused replacement must not stage anything: {strays:?}"
        );
    }

    /// A replacement leaves no scratch file behind.
    #[test]
    fn a_replacement_cleans_up_after_itself() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "- one").unwrap();
        ws.write_file(WorkspaceFile::Memories, "- two").unwrap();

        let strays: Vec<_> = std::fs::read_dir(ws.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "staging files left behind: {strays:?}");
    }
}
