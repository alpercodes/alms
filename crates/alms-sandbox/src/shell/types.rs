//! Types for the redesigned shell tool.
//!
//! This module contains the input/output types, shell state, and background
//! task tracking types used by the `ShellTool`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Maximum output size in bytes before truncation kicks in.
pub const MAX_OUTPUT_BYTES: usize = 30_000;

/// Number of lines to keep from the beginning of truncated output.
pub const HEAD_LINES: usize = 200;

/// Number of lines to keep from the end of truncated output.
pub const TAIL_LINES: usize = 100;

/// Default command timeout in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Maximum allowed timeout in seconds.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// Maximum allowed timeout in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = MAX_TIMEOUT_SECS * 1000;

/// Default command timeout in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = DEFAULT_TIMEOUT_SECS * 1000;

/// Parsed input for the shell tool.
///
/// The command is a shell string executed via `bash -c` on all platforms.
/// The legacy `argv` mode has been removed for security reasons (it bypassed
/// the destructive command denylist and offered no sandboxing benefits).
#[derive(Debug, Clone)]
pub struct ShellInput {
    /// The command to execute via `bash -c`.
    pub command: String,

    /// Optional description of what the command does (for audit logging).
    pub description: Option<String>,

    /// Timeout in milliseconds. Clamped to `MAX_TIMEOUT_MS`.
    pub timeout_ms: u64,

    /// Whether to run the command in the background.
    pub run_in_background: bool,
}

/// Why a post-command working directory failed the containment check.
///
/// The check fails closed, so both variants keep the previous cwd and behave
/// identically. They differ only in what the daemon may *claim* about the
/// path — and the notice the agent reads is built from that claim, so the
/// two must not be conflated (issue #1262).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdRejection {
    /// Both the sandbox root and the candidate canonicalised successfully,
    /// and the candidate is genuinely not inside the root. The only case in
    /// which "outside the sandbox root" is an established fact.
    OutsideRoot,

    /// The candidate or the root could not be canonicalised, so
    /// `canonical_for_comparison` compared a raw path and the containment
    /// test could not decide anything. Rejecting is still correct — failing
    /// closed is the whole point — but *where* the path actually is remains
    /// unknown, and saying otherwise would be a guess dressed as a finding.
    ///
    /// Reached in practice whenever the shell reports a cwd this process
    /// cannot resolve: Windows Git Bash serving `%TEMP%` through its `/tmp`
    /// mount is the known case (#1266), where every command lands here.
    NotVerifiable,
}

/// A post-command working directory that did not pass containment and was
/// therefore not adopted as the persistent cwd (issue #1262).
///
/// Containment itself is enforced in [`super::exec`]; this type is purely the
/// *reporting* channel, so the tool layer can tell the agent that its `cd`
/// did not stick and where the next command will actually run. Without it the
/// revert is a daemon-side `warn!` only: the agent sees `exit_code: 0`,
/// concludes it moved, and misreads every relative path from that point on.
#[derive(Debug, Clone)]
pub struct CwdRevert {
    /// The rejected directory the command ended in, exactly as the shell
    /// engine reported it (MSYS form under Windows Git Bash) so it matches
    /// what the agent's own `pwd` printed.
    pub attempted: PathBuf,

    /// The cwd that was kept, and that the next command will run in.
    pub kept: PathBuf,

    /// What the containment check actually determined. Carried through so
    /// the notice states that and not more — a rejection is not by itself
    /// evidence the path was outside the root.
    pub reason: CwdRejection,
}

/// Output from a shell command execution.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    /// Process exit code (`-1` if the process was killed or timed out).
    pub exit_code: i32,

    /// Combined stdout content (possibly truncated).
    pub stdout: String,

    /// Combined stderr content (possibly truncated).
    pub stderr: String,

    /// Absolute path of the on-disk spill file written by the shell tool
    /// when stdout or stderr exceeded the head+tail truncation threshold
    /// (issue #756). `None` when spill was not triggered or was disabled
    /// via `[tools.shell.spill].enabled = false`. The caller is responsible
    /// for turning this into an agent-visible relative path (see
    /// `spill::relative_spill_path`).
    pub spill_path: Option<PathBuf>,

    /// Set when the command's final working directory was outside the
    /// sandbox root and the persistent cwd was kept at its previous value
    /// (issue #1262). `None` for unsandboxed runs (containment does not
    /// apply there) and for commands that ended inside the root. The caller
    /// turns this into the agent-visible `[cwd unchanged: ...]` notice — see
    /// `ShellTool::append_cwd_revert_notice`.
    pub cwd_revert: Option<CwdRevert>,
}

/// Result of a background task that has completed.
#[derive(Debug, Clone)]
pub struct BackgroundTaskResult {
    /// The task ID assigned when the command was submitted.
    pub task_id: String,

    /// The original command string.
    pub command: String,

    /// The execution output (available after completion).
    pub output: Option<ShellOutput>,

    /// Error message if the task failed to spawn or was cancelled.
    pub error: Option<String>,
}

/// Persistent state for a shell tool instance.
///
/// Tracks the current working directory across invocations and any
/// background tasks that have been submitted.
#[derive(Debug, Clone)]
pub struct ShellState {
    /// Current working directory, persisted across calls.
    /// Updated after each successful command execution by parsing `pwd` output.
    pub cwd: Arc<Mutex<PathBuf>>,

    /// Background tasks indexed by task ID.
    pub background_tasks: Arc<Mutex<HashMap<String, BackgroundTaskResult>>>,

    /// Counter for generating background task IDs.
    pub next_task_id: Arc<Mutex<u64>>,
}

impl ShellState {
    /// Create a new shell state with the given initial working directory.
    pub fn new(initial_cwd: PathBuf) -> Self {
        Self {
            cwd: Arc::new(Mutex::new(initial_cwd)),
            background_tasks: Arc::new(Mutex::new(HashMap::new())),
            next_task_id: Arc::new(Mutex::new(1)),
        }
    }

    /// Generate a new unique task ID.
    pub async fn next_id(&self) -> String {
        let mut counter = self.next_task_id.lock().await;
        let id = format!("bg_{}", *counter);
        *counter += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(MAX_OUTPUT_BYTES, 30_000);
        assert_eq!(HEAD_LINES, 200);
        assert_eq!(TAIL_LINES, 100);
        assert_eq!(DEFAULT_TIMEOUT_SECS, 120);
        assert_eq!(MAX_TIMEOUT_SECS, 600);
        assert_eq!(DEFAULT_TIMEOUT_MS, 120_000);
        assert_eq!(MAX_TIMEOUT_MS, 600_000);
    }

    #[tokio::test]
    async fn test_shell_state_new() {
        let state = ShellState::new(PathBuf::from("/tmp"));
        let cwd = state.cwd.lock().await;
        assert_eq!(*cwd, PathBuf::from("/tmp"));
    }

    #[tokio::test]
    async fn test_shell_state_next_id() {
        let state = ShellState::new(PathBuf::from("/tmp"));
        assert_eq!(state.next_id().await, "bg_1");
        assert_eq!(state.next_id().await, "bg_2");
        assert_eq!(state.next_id().await, "bg_3");
    }
}
