// SPDX-License-Identifier: Apache-2.0

//! Retention sweep for per-run spill directories.
//!
//! Two features spill oversized tool output to disk under `{data_dir}`:
//! the shell tool's head+tail spill (`shell_output/`, see
//! [`crate::shell::spill`]) and the runtime's in-loop tool-output
//! truncation (`tool-output/`). Both lay files out as
//! `{root}/{run_id}/<file>` and both retain them for a configurable number
//! of days, swept once at gateway startup. The sweep is the same algorithm
//! for both; only the root differs, so it lives here once and each feature
//! binds its own directory name.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// Delete files older than `retention_days` under `{root}/*/`.
///
/// Walks every per-run directory directly under `root`, checks each file's
/// filesystem `mtime`, and unlinks any file older than the retention window.
/// A per-run directory left empty is removed too, so the tree stays tidy.
/// A `root` that does not exist is not an error — there is nothing to
/// sweep — and returns `Ok(0)`.
///
/// The sweep never aborts partway through because of one bad entry; only a
/// failure to read `root` itself is returned. What happens to a bad entry
/// depends on what failed, and one of the answers is deletion:
///
/// - a directory or file entry that cannot be *listed* is logged at `warn`
///   and skipped;
/// - a file whose `mtime` cannot be *read* is treated as infinitely old
///   and **deleted**, traced only by the `debug!` on unlink — the
///   `metadata()` / `modified()` failure falls back to `UNIX_EPOCH`;
/// - a file that cannot be *unlinked* is logged at `warn` and kept, and
///   keeps its per-run directory from being removed.
///
/// Returns the number of files deleted, for the caller's startup log line.
pub fn sweep_expired_under(root: &Path, retention_days: u32) -> io::Result<u64> {
    if !root.exists() {
        return Ok(0);
    }

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
        // If subtraction underflows (retention_days is absurdly large), fall
        // back to UNIX_EPOCH — every on-disk file will be newer, so nothing
        // is deleted. That is the correct reading of "keep everything for
        // longer than the clock can express".
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut deleted: u64 = 0;
    let dir_iter = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(e) => {
            warn!(path = %root.display(), error = %e, "Failed to read spill root directory");
            return Err(e);
        }
    };

    for run_entry in dir_iter {
        let run_entry = match run_entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to read spill run entry");
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
                warn!(path = %run_path.display(), error = %e, "Failed to read spill run dir");
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
                        debug!(path = %file_path.display(), "Removed expired spill file");
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
            debug!(path = %run_path.display(), error = %e, "Failed to remove empty spill run dir");
        }
    }

    if deleted > 0 {
        info!(
            deleted,
            root = %root.display(),
            retention_days,
            "Swept expired spill files"
        );
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Set a file's mtime ten days into the past, well outside the 7-day
    /// window every test below sweeps with.
    fn backdate(path: &Path) {
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(ten_days_ago).unwrap();
    }

    fn run_dir(root: &Path, run: &str) -> PathBuf {
        let dir = root.join(run);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sweep_expired_under_missing_root_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let deleted = sweep_expired_under(&dir.path().join("absent"), 7).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn sweep_expired_under_leaves_fresh_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("spill");
        let file = run_dir(&root, "run-fresh").join("a.log");
        std::fs::write(&file, b"hello").unwrap();

        let deleted = sweep_expired_under(&root, 7).unwrap();
        assert_eq!(deleted, 0);
        assert!(file.exists(), "fresh file must survive retention sweep");
    }

    #[test]
    fn sweep_expired_under_deletes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("spill");
        let old_file = run_dir(&root, "run-old").join("a.log");
        std::fs::write(&old_file, b"old bytes").unwrap();
        backdate(&old_file);

        let deleted = sweep_expired_under(&root, 7).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old_file.exists(), "old file must be deleted");
    }

    #[test]
    fn sweep_expired_under_removes_empty_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("spill");
        let run = run_dir(&root, "run-empty");
        let old_file = run.join("a.log");
        std::fs::write(&old_file, b"").unwrap();
        backdate(&old_file);

        sweep_expired_under(&root, 7).unwrap();
        assert!(
            !run.exists(),
            "empty per-run dir should be removed after sweep"
        );
    }

    #[test]
    fn sweep_expired_under_keeps_run_dir_with_fresh_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("spill");
        let run = run_dir(&root, "run-mixed");
        // One old file (should be deleted), one fresh file (should survive).
        let old_file = run.join("old.log");
        let fresh_file = run.join("fresh.log");
        std::fs::write(&old_file, b"old").unwrap();
        std::fs::write(&fresh_file, b"fresh").unwrap();
        backdate(&old_file);

        let deleted = sweep_expired_under(&root, 7).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old_file.exists());
        assert!(fresh_file.exists());
        assert!(run.exists(), "run dir with fresh files must survive");
    }
}
