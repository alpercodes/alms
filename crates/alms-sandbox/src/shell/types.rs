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
/// Supports two invocation styles:
/// - `command` (primary): a shell command string executed via `bash -c` / `cmd /c`
/// - `argv` (legacy): an argv array executed directly (no shell)
#[derive(Debug, Clone)]
pub struct ShellInput {
    /// The command to execute (primary interface).
    pub command: Option<String>,

    /// Legacy argv-style invocation (fallback when `command` is absent).
    pub argv: Option<Vec<String>>,

    /// Optional description of what the command does (for audit logging).
    pub description: Option<String>,

    /// Timeout in milliseconds. Clamped to `MAX_TIMEOUT_MS`.
    pub timeout_ms: u64,

    /// Whether to run the command in the background.
    pub run_in_background: bool,
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

/// A background task being tracked by the shell tool.
#[derive(Debug)]
pub struct BackgroundTask {
    /// Unique task identifier.
    pub task_id: String,

    /// The command that was submitted.
    pub command: String,

    /// Tokio join handle for the spawned task.
    pub handle: tokio::task::JoinHandle<Result<ShellOutput, String>>,
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
