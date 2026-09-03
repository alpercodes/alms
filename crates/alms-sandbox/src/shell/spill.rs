// SPDX-License-Identifier: Apache-2.0

//! Spill full shell output to disk when head+tail truncation fires.
//!
//! When a command produces more than [`MAX_OUTPUT_BYTES`][super::types::MAX_OUTPUT_BYTES]
//! of combined stdout+stderr, the head+tail truncation in
//! [`super::output`] keeps the most useful context for the LLM but drops the
//! middle of the stream — which is frequently where the actual failure reason
//! hides (build logs, test runs, etc.). When spill is enabled, the full
//! captured bytes are written to `{run_dir}/shell_{tool_call_id}.log` and a
//! `[full output spilled to: <relative path>]` marker is appended to the
//! tool result body. The agent can then `fs_read` the spill path with an
//! offset/limit to inspect the middle that truncation dropped.
//!
//! ## Retention
//!
//! Spill files are retained for a configurable window (default 7 days).
//! [`sweep_expired`] walks `{data_dir}/shell_output/*/*.log` once at gateway
//! startup and removes files whose filesystem `mtime` is older than
//! `retention_days`. There is no background ticker — keeping the sweep
//! startup-only keeps the feature simple and surfaces any disk-usage issues
//! on the next restart rather than silently growing forever.
//!
//! ## Sandbox interaction
//!
//! The spill file is written from inside the shell tool's privileged code
//! path (just after the process completes), so the write does not need to
//! traverse the fs_write sandbox. For the *read* side, the runtime adds the
//! per-run spill directory to `fs_read`/`fs_list`/`fs_grep`/`fs_glob`'s
//! `extra_read_roots` so the agent can `fs_read` the path without operators
//! having to grant extra filesystem permissions.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// The fixed base directory name under `{data_dir}/` where spill files live.
///
/// Exposed as a constant so the gateway (startup sweep) and the runtime
/// (per-run directory plumbing) agree on the same layout.
pub const SPILL_DIR_NAME: &str = "shell_output";

/// Per-[`ShellTool`][super::ShellTool] spill policy, baked in at construction.
///
/// `run_dir` is the absolute path of the per-run directory
/// (`{data_dir}/shell_output/{run_id}/`). When `None`, spill is effectively
/// disabled for this tool instance even if `enabled` is true — the runtime
/// constructs this value only when a run_id is known.
#[derive(Debug, Clone)]
pub struct ShellSpillPolicy {
    /// Whether spill-to-disk is enabled. `false` is a hard opt-out: no
    /// directory is created, no marker is appended to tool output.
    pub enabled: bool,

    /// Absolute path of the per-run spill directory. Created lazily on first
    /// spill. `None` when the runtime couldn't resolve a run_id (e.g. unit
    /// tests, early construction paths).
    pub run_dir: Option<PathBuf>,
}

impl ShellSpillPolicy {
    /// Return a disabled policy. Useful as an explicit "no spill" marker in
    /// tests and in construction paths that don't yet have a run_id.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            run_dir: None,
        }
    }

    /// Return a policy that writes spills to `run_dir`.
    pub fn with_run_dir(run_dir: PathBuf) -> Self {
        Self {
            enabled: true,
            run_dir: Some(run_dir),
        }
    }

    /// Whether this policy will spill for the current tool invocation.
    /// Both the enable flag and the run directory must be set.
    pub fn is_active(&self) -> bool {
        self.enabled && self.run_dir.is_some()
    }
}

impl Default for ShellSpillPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Write the full captured stdout+stderr for a single shell invocation to
/// `{run_dir}/shell_{tool_call_id}.log`.
///
/// The file format is plain bytes: stdout first, then stderr prefixed with a
/// single separator line. The separator makes the file self-describing when
/// the agent or a human opens it, without requiring a structured format
/// (out of scope per the issue).
///
/// Returns the absolute path of the spill file on success.
pub fn write_spill(
    run_dir: &Path,
    tool_call_id: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> io::Result<PathBuf> {
    std::fs::create_dir_all(run_dir)?;

    let filename = format!("shell_{}.log", sanitize_tool_call_id(tool_call_id));
    let path = run_dir.join(filename);

    // Build the full buffer in memory first so the write is atomic enough for
    // our purposes — no consumer will read the spill file concurrently with
    // the write, but bailing out with a partial file on an I/O error is
    // avoidable here.
    let mut buf: Vec<u8> = Vec::with_capacity(stdout.len() + stderr.len() + 32);
    buf.extend_from_slice(stdout);
    if !stderr.is_empty() {
        // Always insert the separator *before* stderr so a reader always sees
        // "everything before the separator is stdout, everything after is
        // stderr" regardless of whether stdout ended with a newline.
        if !stdout.is_empty() && !stdout.ends_with(b"\n") {
            buf.push(b'\n');
        }
        buf.extend_from_slice(b"\n--- stderr ---\n");
        buf.extend_from_slice(stderr);
    }

    std::fs::write(&path, &buf)?;
    debug!(
        path = %path.display(),
        stdout_bytes = stdout.len(),
        stderr_bytes = stderr.len(),
        "Wrote shell output spill file"
    );
    Ok(path)
}

/// Return a safe filename fragment for a `tool_call_id`. IDs from the LLM
/// provider are usually ASCII (`call_abc123`) but we sanitize defensively so
/// a malformed or deliberately crafted id can't escape the directory or
/// create a hidden file on POSIX.
fn sanitize_tool_call_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        // Fall back to a random-enough identifier rather than producing an
        // empty filename. Using the current nanosecond count is fine here —
        // the spill file is throwaway and the caller wraps this in a unique
        // per-run directory anyway.
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        format!("unknown_{ts}")
    } else {
        cleaned
    }
}

/// Compute the relative path from `workspace_root` to `spill_path`, falling
/// back to the absolute path if the former doesn't contain the latter.
///
/// The agent-visible tool result includes this relative path so `fs_read`
/// with the agent's current sandbox root (which we've expanded via
/// `extra_read_roots`) resolves the spill file without the agent having to
/// know the project-root layout.
pub fn relative_spill_path(spill_path: &Path, workspace_root: Option<&Path>) -> String {
    if let Some(root) = workspace_root
        && let Ok(rel) = spill_path.strip_prefix(root)
    {
        return rel.to_string_lossy().replace('\\', "/");
    }
    spill_path.to_string_lossy().replace('\\', "/")
}

/// Delete spilled log files older than `retention_days` under
/// `{data_dir}/shell_output/`.
///
/// Walks every `{data_dir}/shell_output/<run_id>/*.log`, checks filesystem
/// `mtime`, and unlinks any file older than the retention window. Empty
/// `<run_id>` directories are also removed so the tree stays tidy.
///
/// Errors on individual files are logged at `warn` level; the sweep never
/// aborts partway through because of one bad entry.
///
/// Returns the number of files deleted (for diagnostics / logging at the
/// gateway-startup callsite).
pub fn sweep_expired(data_dir: &Path, retention_days: u32) -> io::Result<u64> {
    let spill_root = data_dir.join(SPILL_DIR_NAME);
    if !spill_root.exists() {
        return Ok(0);
    }

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
        // If subtraction underflows (retention_days is absurdly large), fall
        // back to UNIX_EPOCH — every on-disk file will be older, meaning the
        // caller would delete everything. That is the correct behaviour for
        // an "everything is expired" request.
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut deleted: u64 = 0;
    let dir_iter = match std::fs::read_dir(&spill_root) {
        Ok(it) => it,
        Err(e) => {
            warn!(path = %spill_root.display(), error = %e, "Failed to read shell_output directory");
            return Err(e);
        }
    };

    for run_entry in dir_iter {
        let run_entry = match run_entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to read shell_output run entry");
                continue;
            }
        };
        let run_path = run_entry.path();
        if !run_path.is_dir() {
            continue;
        }

        let files = match std::fs::read_dir(&run_path) {
            Ok(it) => it,
            Err(e) => {
                warn!(path = %run_path.display(), error = %e, "Failed to read run spill dir");
                continue;
            }
        };

        let mut remaining: u64 = 0;
        for file_entry in files {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "Failed to read spill file entry");
                    continue;
                }
            };
            let file_path = file_entry.path();
            let mtime = file_entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime < cutoff {
                match std::fs::remove_file(&file_path) {
                    Ok(()) => {
                        deleted += 1;
                        debug!(path = %file_path.display(), "Removed expired shell spill file");
                    }
                    Err(e) => {
                        warn!(path = %file_path.display(), error = %e, "Failed to remove expired spill file");
                        remaining += 1;
                    }
                }
            } else {
                remaining += 1;
            }
        }

        // If the per-run directory is now empty, remove it too.
        if remaining == 0
            && let Err(e) = std::fs::remove_dir(&run_path)
        {
            // Not fatal — the next sweep will try again.
            debug!(path = %run_path.display(), error = %e, "Failed to remove empty run spill dir");
        }
    }

    if deleted > 0 {
        info!(
            deleted,
            spill_root = %spill_root.display(),
            retention_days,
            "Swept expired shell output spill files"
        );
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sanitize_tool_call_id_passes_alphanumerics() {
        assert_eq!(sanitize_tool_call_id("call_abc123"), "call_abc123");
        assert_eq!(sanitize_tool_call_id("abc-def_123"), "abc-def_123");
    }

    #[test]
    fn sanitize_tool_call_id_replaces_path_separators() {
        // Both POSIX and Windows path separators must be neutralised so the
        // id can't escape the run directory.
        assert_eq!(sanitize_tool_call_id("../foo/bar"), "___foo_bar");
        assert_eq!(sanitize_tool_call_id(r"..\foo\bar"), "___foo_bar");
    }

    #[test]
    fn sanitize_tool_call_id_substitutes_empty() {
        let out = sanitize_tool_call_id("");
        assert!(out.starts_with("unknown_"));
    }

    #[test]
    fn sanitize_tool_call_id_handles_all_invalid() {
        assert_eq!(sanitize_tool_call_id("///"), "___");
    }

    #[test]
    fn is_active_requires_enabled_and_run_dir() {
        assert!(!ShellSpillPolicy::disabled().is_active());
        let p = ShellSpillPolicy::with_run_dir(PathBuf::from("/tmp/foo"));
        assert!(p.is_active());
    }

    #[test]
    fn write_spill_roundtrips_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_spill(dir.path(), "call_123", b"stdout", b"stderr").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // stdout first, stderr after the separator.
        assert!(bytes.starts_with(b"stdout"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("--- stderr ---"));
        assert!(text.ends_with("stderr"));
    }

    #[test]
    fn write_spill_without_stderr_omits_separator() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_spill(dir.path(), "call_empty_err", b"only stdout\n", b"").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, b"only stdout\n");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("--- stderr ---"),
            "no stderr means no separator line"
        );
    }

    #[test]
    fn write_spill_creates_nested_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("shell_output").join("run-abc");
        assert!(!nested.exists());
        let path = write_spill(&nested, "call_1", b"hello", b"").unwrap();
        assert!(path.exists());
        assert!(nested.is_dir());
    }

    #[test]
    fn relative_spill_path_strips_workspace_root() {
        let root = PathBuf::from("/ws");
        let path = PathBuf::from("/ws/.alms/shell_output/run1/shell_x.log");
        let rel = relative_spill_path(&path, Some(&root));
        assert_eq!(rel, ".alms/shell_output/run1/shell_x.log");
    }

    #[test]
    fn relative_spill_path_falls_back_to_absolute_when_unrelated() {
        let root = PathBuf::from("/home/user");
        let path = PathBuf::from("/var/log/spill.log");
        let rel = relative_spill_path(&path, Some(&root));
        // The abs path is preserved as-is (modulo backslash -> forward-slash
        // normalisation that helps cross-platform parity).
        assert_eq!(rel, "/var/log/spill.log");
    }

    #[test]
    fn sweep_expired_missing_root_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn sweep_expired_leaves_fresh_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(SPILL_DIR_NAME).join("run-fresh");
        std::fs::create_dir_all(&run_dir).unwrap();
        let file = run_dir.join("shell_a.log");
        std::fs::write(&file, b"hello").unwrap();

        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 0);
        assert!(file.exists(), "fresh file must survive retention sweep");
    }

    #[test]
    fn sweep_expired_deletes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(SPILL_DIR_NAME).join("run-old");
        std::fs::create_dir_all(&run_dir).unwrap();
        let old_file = run_dir.join("shell_a.log");
        std::fs::write(&old_file, b"old bytes").unwrap();

        // Backdate the file by 10 days by setting mtime via the filetime
        // crate isn't available here — use std::fs::File::set_modified, which
        // is stable on Rust 1.75+. The project targets nightly so this is
        // fine.
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options()
            .write(true)
            .open(&old_file)
            .unwrap();
        f.set_modified(ten_days_ago).unwrap();
        drop(f);

        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old_file.exists(), "old file must be deleted");
    }

    #[test]
    fn sweep_expired_removes_empty_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(SPILL_DIR_NAME).join("run-empty");
        std::fs::create_dir_all(&run_dir).unwrap();
        let old_file = run_dir.join("shell_a.log");
        std::fs::write(&old_file, b"").unwrap();
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options()
            .write(true)
            .open(&old_file)
            .unwrap();
        f.set_modified(ten_days_ago).unwrap();
        drop(f);

        sweep_expired(dir.path(), 7).unwrap();
        assert!(
            !run_dir.exists(),
            "empty per-run dir should be removed after sweep"
        );
    }

    #[test]
    fn sweep_expired_keeps_run_dir_with_fresh_files() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(SPILL_DIR_NAME).join("run-mixed");
        std::fs::create_dir_all(&run_dir).unwrap();

        // One old file (should be deleted), one fresh file (should survive).
        let old_file = run_dir.join("shell_old.log");
        let fresh_file = run_dir.join("shell_fresh.log");
        std::fs::write(&old_file, b"old").unwrap();
        std::fs::write(&fresh_file, b"fresh").unwrap();
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options()
            .write(true)
            .open(&old_file)
            .unwrap();
        f.set_modified(ten_days_ago).unwrap();
        drop(f);

        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old_file.exists());
        assert!(fresh_file.exists());
        assert!(run_dir.exists(), "run dir with fresh files must survive");
    }
}
