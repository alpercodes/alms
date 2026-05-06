mod datetime;
mod echo;
mod fs_edit;
mod fs_glob;
mod fs_grep;
mod fs_list;
mod fs_read;
mod fs_write;
mod http_get;
mod line_cap;
mod math;

pub use datetime::DatetimeTool;
pub use echo::EchoTool;
pub use fs_edit::FsEditTool;
pub use fs_glob::FsGlobTool;
pub use fs_grep::FsGrepTool;
pub use fs_list::FsListTool;
pub use fs_read::FsReadTool;
pub use fs_write::FsWriteTool;
pub use http_get::HttpGetTool;
pub use math::MathTool;

// ---------------------------------------------------------------------------
// Shared helpers — used by multiple tool files via `super::`
// ---------------------------------------------------------------------------

use crate::{SandboxError, error::SandboxResult};
use std::path::{Component, Path, PathBuf};

/// Blocked Unix device paths that produce infinite output or hang.
#[cfg(unix)]
const BLOCKED_DEVICE_PATHS: &[&str] = &[
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
    "/dev/console",
    "/proc/self/fd/0",
    "/proc/self/fd/1",
    "/proc/self/fd/2",
];

/// Blocked Windows reserved device names (case-insensitive).
///
/// These are special device names on Windows that can appear anywhere in a
/// path (e.g. `C:\whatever\CON` still opens the console device).
#[cfg(windows)]
const BLOCKED_DEVICE_NAMES: &[&str] = &[
    "CON", "NUL", "PRN", "AUX", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Check whether a path references a blocked device that could hang or
/// produce infinite output.
///
/// On Unix: exact-match against known `/dev/*` and `/proc/self/fd/*` paths.
/// On Windows: case-insensitive match against reserved device names (the
/// file-stem portion, ignoring extension — Windows treats `NUL.txt` the
/// same as `NUL`).
pub(crate) fn is_blocked_device_path(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let s = path.to_str().unwrap_or("");
        if BLOCKED_DEVICE_PATHS.contains(&s) {
            return true;
        }
    }

    #[cfg(windows)]
    {
        // On Windows, reserved names are matched by file stem (the part
        // before the first dot), case-insensitively, regardless of where
        // they appear in the directory tree.
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && BLOCKED_DEVICE_NAMES
                .iter()
                .any(|&d| d.eq_ignore_ascii_case(stem))
        {
            return true;
        }
    }

    false
}

/// Check whether a path is a UNC (Universal Naming Convention) path.
///
/// UNC paths (`\\server\share` or `//server/share`) trigger automatic SMB
/// authentication on Windows, which can leak NTLM credentials to
/// attacker-controlled servers. We block them on all platforms to prevent
/// a malicious agent prompt from exploiting this.
///
/// The Windows extended-length prefix (`\\?\`) is NOT a UNC path — it is a
/// mechanism for paths longer than 260 characters — and is therefore allowed.
/// However, `\\?\UNC\server\share` is the extended-length *UNC* form and
/// still triggers SMB authentication, so it must be blocked.
///
/// The device namespace prefix (`\\.\`) also starts with `\\` and is caught
/// by the general `\\` check. This is intentional: `\\.\UNC\server\share`
/// can access remote shares, and `\\.\pipe\name` can access named pipes.
pub(crate) fn is_unc_path(path: &str) -> bool {
    // \\server\share (Windows UNC)
    if path.starts_with("\\\\") {
        // Allow \\?\ (extended-length path prefix, not UNC) …
        if let Some(rest) = path.strip_prefix("\\\\?\\") {
            // … but block \\?\UNC\ — extended-length UNC still triggers SMB.
            // Case-insensitive: NTFS treats \\?\unc\ the same as \\?\UNC\.
            let upper = rest.to_ascii_uppercase();
            return upper.starts_with("UNC\\") || upper.starts_with("UNC/");
        }
        // Everything else starting with \\ is standard UNC (including \\.\).
        return true;
    }
    // //server/share (URI-style UNC, common in cross-platform code)
    path.starts_with("//")
}

/// Reject UNC paths with a consistent error.
///
/// Returns `Ok(())` for safe paths, or a `SandboxViolation` error for any
/// path that would trigger SMB authentication.
pub(crate) fn reject_unc_path(path: &str) -> SandboxResult<()> {
    if is_unc_path(path) {
        return Err(SandboxError::SandboxViolation(
            "UNC paths (\\\\server\\share or //server/share) are not allowed \
             — they can leak credentials via SMB authentication."
                .to_string(),
        ));
    }
    Ok(())
}

/// Resolve a path and verify it falls within the sandbox root.
///
/// Relative paths are joined to `sandbox_root`. Absolute paths are checked
/// directly. Symlinks are followed via `canonicalize()` to prevent escapes.
/// For non-existent paths (e.g. fs_write targets) the nearest existing
/// ancestor is canonicalized and the remaining components are appended.
/// Returns the resolved path on success so callers can use it for I/O
/// (avoids re-resolving relative paths against a different base).
pub(crate) fn check_sandbox_path(path: &str, sandbox_root: &Path) -> SandboxResult<PathBuf> {
    let p = Path::new(path);

    // Canonicalize the sandbox root so the comparison works even when the
    // root was stored as a relative path or without UNC prefix (Windows).
    let canonical_root = canonicalize_best_effort(sandbox_root).map_err(|e| {
        SandboxError::SandboxViolation(format!(
            "Cannot resolve sandbox root '{}': {}",
            sandbox_root.display(),
            e
        ))
    })?;

    // Resolve: relative paths join to sandbox_root, absolute stay as-is
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        canonical_root.join(p)
    };

    // Canonicalize to follow symlinks. Walk up for non-existent paths.
    let canonical = canonicalize_best_effort(&resolved)
        .map_err(|e| SandboxError::SandboxViolation(format!("Cannot resolve '{}': {}", path, e)))?;

    if !canonical.starts_with(&canonical_root) {
        return Err(SandboxError::SandboxViolation(format!(
            "Path '{}' is outside sandbox root",
            path
        )));
    }

    Ok(canonical)
}

/// Async version of [`check_sandbox_path`] that offloads the blocking
/// `std::fs::canonicalize()` / `path.exists()` calls to a blocking thread
/// via `tokio::task::spawn_blocking`, preventing async worker stalls on
/// slow filesystems or Windows antivirus scans.
pub(crate) async fn check_sandbox_path_async(
    path: &str,
    sandbox_root: &Path,
) -> SandboxResult<PathBuf> {
    let path_owned = path.to_owned();
    let root_owned = sandbox_root.to_owned();
    tokio::task::spawn_blocking(move || check_sandbox_path(&path_owned, &root_owned))
        .await
        .map_err(|e| {
            SandboxError::SandboxViolation(format!("Sandbox path check task failed: {}", e))
        })?
}

/// Resolve a path and verify it falls within the primary sandbox root OR any
/// of the additional read-only roots.
///
/// Used by read-family filesystem tools (`fs_read`, `fs_list`, `fs_grep`,
/// `fs_glob`) to allow read-only access to directories outside the primary
/// sandbox root — for example, sibling agent workspace directories so that a
/// parent agent can read a subagent's `memories.md` without being able to
/// modify it (#242).
///
/// Relative paths are resolved against the primary sandbox root (preserving
/// current behaviour). Absolute paths are allowed if they resolve inside
/// either the primary root or any extra read-only root. Symlinks are followed
/// via `canonicalize()` to prevent escapes.
pub(crate) fn check_sandbox_path_with_extras(
    path: &str,
    sandbox_root: &Path,
    extra_read_roots: &[PathBuf],
) -> SandboxResult<PathBuf> {
    let p = Path::new(path);

    let canonical_root = canonicalize_best_effort(sandbox_root).map_err(|e| {
        SandboxError::SandboxViolation(format!(
            "Cannot resolve sandbox root '{}': {}",
            sandbox_root.display(),
            e
        ))
    })?;

    // Resolve: relative paths join to the primary sandbox root so tool UX is
    // unchanged — extras only expand the allowed *absolute* path set.
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        canonical_root.join(p)
    };

    let canonical = canonicalize_best_effort(&resolved)
        .map_err(|e| SandboxError::SandboxViolation(format!("Cannot resolve '{}': {}", path, e)))?;

    if canonical.starts_with(&canonical_root) {
        return Ok(canonical);
    }

    // Fall back to checking each extra read-only root. Each root is
    // canonicalized independently so mismatched UNC prefixes on Windows
    // don't cause spurious rejections.
    for extra in extra_read_roots {
        if let Ok(canon_extra) = canonicalize_best_effort(extra)
            && canonical.starts_with(&canon_extra)
        {
            return Ok(canonical);
        }
    }

    Err(SandboxError::SandboxViolation(format!(
        "Path '{}' is outside sandbox root",
        path
    )))
}

/// Async version of [`check_sandbox_path_with_extras`].
pub(crate) async fn check_sandbox_path_with_extras_async(
    path: &str,
    sandbox_root: &Path,
    extra_read_roots: &[PathBuf],
) -> SandboxResult<PathBuf> {
    let path_owned = path.to_owned();
    let root_owned = sandbox_root.to_owned();
    let extras_owned = extra_read_roots.to_vec();
    tokio::task::spawn_blocking(move || {
        check_sandbox_path_with_extras(&path_owned, &root_owned, &extras_owned)
    })
    .await
    .map_err(|e| SandboxError::SandboxViolation(format!("Sandbox path check task failed: {}", e)))?
}

/// Canonicalize a path, walking up to the nearest existing ancestor if the
/// full path does not yet exist (handles fs_write for new files/dirs).
pub(crate) fn canonicalize_best_effort(path: &Path) -> std::io::Result<PathBuf> {
    // Fast path: if the whole path exists, let the OS resolve it.
    if path.exists() {
        return std::fs::canonicalize(path);
    }

    // Walk components and resolve `.` / `..` manually so that non-existent
    // intermediate directories (e.g. `foo/../../secret`) are handled correctly.
    // `Path::file_name()` returns `None` for `..`, which caused the previous
    // recursive approach to silently skip `..` resolution.
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => resolved.push(p.as_os_str()),
            Component::RootDir => resolved.push(Component::RootDir.as_os_str()),
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                if !resolved.pop() {
                    // Already at root or empty — push `..` so the caller sees it
                    resolved.push("..");
                }
            }
            Component::Normal(c) => {
                let candidate = resolved.join(c);
                if candidate.exists() {
                    // Resolve symlinks for the segment that exists
                    resolved = std::fs::canonicalize(&candidate)?;
                } else {
                    resolved = candidate;
                }
            }
        }
    }
    Ok(resolved)
}

/// Normalize a path when no sandbox root is configured.
///
/// Uses `canonicalize_best_effort` so that `./foo.txt` and `foo.txt` resolve to
/// the same cache key. Falls back to `PathBuf::from(path)` if canonicalization
/// fails (e.g. the drive root is inaccessible).
pub(crate) async fn normalize_unsandboxed_path(path: &str) -> PathBuf {
    let path_owned = path.to_owned();
    match tokio::task::spawn_blocking(move || canonicalize_best_effort(Path::new(&path_owned)))
        .await
    {
        Ok(Ok(p)) => p,
        _ => PathBuf::from(path),
    }
}

/// Truncate a string to at most `max_bytes` bytes, respecting UTF-8 char boundaries.
/// Appends a truncation note when the string is shortened.
///
/// Kept for tests only -- production `fs_read` now uses line-based reading.
#[cfg(test)]
pub(super) fn safe_truncate(s: &str, max_bytes: usize) -> String {
    use alms_core::truncate_to_char_boundary;
    let truncated = truncate_to_char_boundary(s, max_bytes);
    if truncated.len() == s.len() {
        s.to_owned()
    } else {
        format!(
            "{}\u{2026}[truncated, {} bytes omitted]",
            truncated,
            s.len() - truncated.len()
        )
    }
}

// ---------------------------------------------------------------------------
// Shared fs_grep / fs_glob infrastructure
// ---------------------------------------------------------------------------

use walkdir::WalkDir;

/// VCS directories excluded from recursive directory walks.
pub(crate) const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// Maximum directory traversal depth for the recursive WalkDir.  Prevents
/// unbounded walks in deeply-nested trees (e.g. node_modules chains).
pub(crate) const MAX_WALK_DEPTH: usize = 50;

/// Walk `search_root` and call `visitor` for every regular file that passes the
/// shared security filters (VCS exclusion, denied-path rejection, sandbox
/// enforcement) and the optional glob filter.
///
/// This is the single place where the walker, VCS/denied/sandbox filtering, and
/// glob matching live — both `collect_files` (fs_grep) and `collect_glob_files`
/// (fs_glob) delegate to it, so a future security fix only needs one patch site.
///
/// A file is allowed through if it resolves inside the primary `sandbox_root`
/// OR any of the `extra_read_roots` — the latter is how parent agents get
/// read-only access to sibling agent workspaces (#242).  Pass an empty slice
/// for `extra_read_roots` to get the traditional sandbox-only behaviour.
pub(crate) fn walk_filtered_files_with_extras(
    search_root: &Path,
    sandbox_root: Option<&Path>,
    extra_read_roots: &[PathBuf],
    glob_matcher: Option<&globset::GlobMatcher>,
    mut visitor: impl FnMut(&walkdir::DirEntry),
) {
    // Canonicalize the sandbox root so the `starts_with` check works
    // correctly even on Windows where walkdir may return UNC paths
    // (`\\?\C:\...`) while the stored root uses the short form (`C:\...`).
    let canonical_sandbox_root = sandbox_root.and_then(|r| canonicalize_best_effort(r).ok());
    let canonical_extras: Vec<PathBuf> = extra_read_roots
        .iter()
        .filter_map(|r| canonicalize_best_effort(r).ok())
        .collect();

    let walker = WalkDir::new(search_root)
        .max_depth(MAX_WALK_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_str().unwrap_or("");

            // Skip VCS directories.
            if entry.file_type().is_dir() && VCS_DIRS.contains(&name) {
                return false;
            }

            true
        });

    for entry in walker.flatten() {
        // Only collect files, not directories.
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        // Enforce sandbox root on each discovered file.  When extras are
        // configured, the file may live inside any of them too (read-only).
        if let Some(ref root) = canonical_sandbox_root {
            let in_primary = path.starts_with(root);
            let in_extras = canonical_extras.iter().any(|e| path.starts_with(e));
            if !in_primary && !in_extras {
                continue;
            }
        }

        // Apply glob filter: match against the path relative to the search root.
        if let Some(matcher) = glob_matcher {
            let relative = path.strip_prefix(search_root).unwrap_or(path);
            // Convert to forward-slash for cross-platform glob matching.
            let rel_str = relative.to_string_lossy().replace('\\', "/");
            if !matcher.is_match(&*rel_str) {
                continue;
            }
        }

        visitor(&entry);
    }
}

/// Relativize a path against the search root for token-efficient output.
pub(crate) fn relativize(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Tests for shared helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use std::sync::Arc;

    // ── safe_truncate ─────────────────────────────────────────────────────────

    #[test]
    fn test_safe_truncate_short_string() {
        assert_eq!(safe_truncate("hello", 100), "hello");
    }

    #[test]
    fn test_safe_truncate_exact_boundary() {
        let s = "hello";
        assert_eq!(safe_truncate(s, 5), "hello");
    }

    #[test]
    fn test_safe_truncate_ascii() {
        let s = "abcde";
        let result = safe_truncate(s, 3);
        assert!(result.starts_with("abc"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_safe_truncate_multibyte() {
        // '€' is 3 bytes (0xE2 0x82 0xAC). Truncating at byte 4 would split it.
        let s = "€€€";
        // Truncate at 4 bytes — must not panic and must land on a char boundary.
        let result = safe_truncate(s, 4);
        // '€' (3 bytes) fits; second '€' starts at byte 3, so boundary is 3.
        assert!(result.starts_with('€'));
        assert!(result.contains("truncated"));
    }

    // ── check_sandbox_path ─────────────────────────────────────────────────────

    #[test]
    fn test_sandbox_relative_inside_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // Create a file so canonicalize succeeds
        std::fs::write(root.join("data.txt"), "").unwrap();
        assert!(check_sandbox_path("data.txt", &root).is_ok());
    }

    #[test]
    fn test_sandbox_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("../etc/passwd", &root).is_err());
        assert!(check_sandbox_path("foo/../../secret", &root).is_err());
    }

    #[test]
    fn test_sandbox_absolute_outside_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // An absolute path outside the sandbox root should be rejected
        #[cfg(unix)]
        assert!(check_sandbox_path("/etc/passwd", &root).is_err());
        #[cfg(windows)]
        assert!(check_sandbox_path("C:\\Windows\\System32", &root).is_err());
    }

    #[test]
    fn test_sandbox_new_file_allowed() {
        // Writing a new file inside sandbox root should work even though
        // the file doesn't exist yet — canonicalize_best_effort walks up.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("new_file.txt", &root).is_ok());
        assert!(check_sandbox_path("subdir/new_file.txt", &root).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_sandbox_symlink_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // Create a symlink inside sandbox pointing outside
        let link_path = root.join("escape");
        std::os::unix::fs::symlink("/etc", &link_path).unwrap();
        assert!(check_sandbox_path("escape/passwd", &root).is_err());
    }

    // ── is_blocked_device_path ────────────────────────────────────────────────

    #[test]
    fn test_is_blocked_device_path_unix_devices() {
        #[cfg(unix)]
        {
            assert!(is_blocked_device_path(Path::new("/dev/zero")));
            assert!(is_blocked_device_path(Path::new("/dev/random")));
            assert!(is_blocked_device_path(Path::new("/dev/urandom")));
            assert!(is_blocked_device_path(Path::new("/dev/stdin")));
            assert!(is_blocked_device_path(Path::new("/dev/stdout")));
            assert!(is_blocked_device_path(Path::new("/dev/stderr")));
            assert!(is_blocked_device_path(Path::new("/dev/tty")));
            assert!(is_blocked_device_path(Path::new("/dev/console")));
            assert!(is_blocked_device_path(Path::new("/proc/self/fd/0")));
            assert!(is_blocked_device_path(Path::new("/proc/self/fd/1")));
            assert!(is_blocked_device_path(Path::new("/proc/self/fd/2")));
        }
    }

    #[test]
    fn test_is_blocked_device_path_windows_devices() {
        #[cfg(windows)]
        {
            assert!(is_blocked_device_path(Path::new("CON")));
            assert!(is_blocked_device_path(Path::new("NUL")));
            assert!(is_blocked_device_path(Path::new("PRN")));
            assert!(is_blocked_device_path(Path::new("AUX")));
            assert!(is_blocked_device_path(Path::new("COM1")));
            assert!(is_blocked_device_path(Path::new("LPT1")));
            // Case-insensitive on Windows
            assert!(is_blocked_device_path(Path::new("con")));
            assert!(is_blocked_device_path(Path::new("nul")));
            assert!(is_blocked_device_path(Path::new("Nul.txt")));
        }
    }

    #[test]
    fn test_is_blocked_device_path_regular_files() {
        // Normal file paths should never be blocked.
        assert!(!is_blocked_device_path(Path::new("hello.txt")));
        assert!(!is_blocked_device_path(Path::new("/home/user/data.json")));
        assert!(!is_blocked_device_path(Path::new("src/main.rs")));
    }

    // ── UNC path blocking ─────────────────────────────────────────────────────

    #[test]
    fn test_is_unc_path_windows_backslash() {
        assert!(is_unc_path("\\\\server\\share"));
        assert!(is_unc_path("\\\\server\\share\\file.txt"));
        assert!(is_unc_path("\\\\attacker.com\\evil"));
        assert!(is_unc_path("\\\\192.168.1.1\\share"));
    }

    #[test]
    fn test_is_unc_path_uri_style_forward_slash() {
        assert!(is_unc_path("//server/share"));
        assert!(is_unc_path("//server/share/file.txt"));
        assert!(is_unc_path("//attacker.com/evil"));
    }

    #[test]
    fn test_is_unc_path_extended_length_allowed() {
        assert!(!is_unc_path("\\\\?\\C:\\long\\path"));
        assert!(!is_unc_path("\\\\?\\D:\\some\\deeply\\nested\\directory"));
    }

    #[test]
    fn test_is_unc_path_extended_length_unc_blocked() {
        assert!(is_unc_path("\\\\?\\UNC\\server\\share"));
        assert!(is_unc_path("\\\\?\\UNC\\attacker.com\\evil"));
        assert!(is_unc_path("\\\\?\\UNC/server/share"));
        assert!(is_unc_path("\\\\?\\unc\\server\\share"));
        assert!(is_unc_path("\\\\?\\Unc\\server\\share"));
        assert!(is_unc_path("\\\\?\\uNc/server/share"));
    }

    #[test]
    fn test_is_unc_path_normal_paths_allowed() {
        assert!(!is_unc_path("C:\\normal\\path"));
        assert!(!is_unc_path("C:\\Users\\test\\file.txt"));
        assert!(!is_unc_path("/normal/unix/path"));
        assert!(!is_unc_path("/home/user/file.txt"));
        assert!(!is_unc_path("./relative/path"));
        assert!(!is_unc_path("relative/path"));
        assert!(!is_unc_path("file.txt"));
        assert!(!is_unc_path("."));
    }

    #[test]
    fn test_is_unc_path_edge_cases() {
        assert!(!is_unc_path(""));
        assert!(!is_unc_path("\\"));
        assert!(!is_unc_path("/"));
    }

    // ── canonicalize_best_effort edge cases ────────────────────────────────────

    #[test]
    fn test_canonicalize_deep_mixed_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("a/b/../../c/../../../secret", &root).is_err());
    }

    #[test]
    fn test_canonicalize_dot_only_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let result_dot = check_sandbox_path(".", &root);
        assert!(result_dot.is_ok(), "path '.' should stay inside sandbox");
        assert_eq!(result_dot.unwrap(), root);

        let result_dot_slash = check_sandbox_path("./", &root);
        assert!(
            result_dot_slash.is_ok(),
            "path './' should stay inside sandbox"
        );
        assert_eq!(result_dot_slash.unwrap(), root);

        let result_dot_chain = check_sandbox_path("././.", &root);
        assert!(
            result_dot_chain.is_ok(),
            "path '././.' should stay inside sandbox"
        );
        assert_eq!(result_dot_chain.unwrap(), root);
    }

    #[test]
    fn test_canonicalize_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let result = check_sandbox_path("", &root);
        assert!(result.is_ok(), "empty string should resolve inside sandbox");
        assert_eq!(result.unwrap(), root);
    }

    #[test]
    fn test_canonicalize_excessive_parent_pops() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("../../../..", &root).is_err());
        assert!(check_sandbox_path("../../../../../../etc/passwd", &root).is_err());
    }

    #[test]
    fn test_canonicalize_trailing_slashes() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("foo")).unwrap();
        std::fs::create_dir(root.join("foo").join("bar")).unwrap();

        let result = check_sandbox_path("foo/bar/", &root);
        assert!(
            result.is_ok(),
            "trailing slash on existing dir should stay inside sandbox"
        );
        assert!(result.unwrap().starts_with(&root));

        let result_new = check_sandbox_path("newdir/subdir/", &root);
        assert!(
            result_new.is_ok(),
            "trailing slash on new path should stay inside sandbox"
        );
        assert!(result_new.unwrap().starts_with(&root));
    }

    #[cfg(windows)]
    #[test]
    fn test_canonicalize_mixed_separators_windows() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        assert!(
            check_sandbox_path("foo\\..\\..\\..\\secret", &root).is_err(),
            "backslash traversal should be rejected"
        );
        assert!(
            check_sandbox_path("foo/..\\..\\secret", &root).is_err(),
            "mixed-separator traversal should be rejected"
        );
    }

    #[test]
    fn test_canonicalize_null_byte_injection() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let result = check_sandbox_path("safe\0/../../../etc/passwd", &root);
        if let Ok(resolved) = &result {
            assert!(
                resolved.starts_with(&root),
                "null-byte path resolved outside sandbox: {}",
                resolved.display()
            );
        }

        let result2 = check_sandbox_path("file\0.txt", &root);
        if let Ok(resolved) = &result2 {
            assert!(
                resolved.starts_with(&root),
                "null-byte filename resolved outside sandbox: {}",
                resolved.display()
            );
        }
    }

    // ── normalize_unsandboxed_path tests ────────────────────────────────────

    #[tokio::test]
    async fn test_normalize_unsandboxed_path_dot_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foo.txt");
        std::fs::write(&file, b"content").unwrap();

        let _prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let a = normalize_unsandboxed_path("./foo.txt").await;
        let b = normalize_unsandboxed_path("foo.txt").await;
        assert_eq!(
            a, b,
            "./foo.txt and foo.txt should normalize to the same path"
        );

        let _ = std::env::set_current_dir(_prev);
    }

    #[tokio::test]
    async fn test_normalize_unsandboxed_path_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("abs.txt");
        std::fs::write(&file, b"data").unwrap();

        let result = normalize_unsandboxed_path(file.to_str().unwrap()).await;
        assert!(result.is_absolute());
        assert!(
            result.ends_with("abs.txt"),
            "expected path ending in abs.txt, got {}",
            result.display()
        );
    }

    #[tokio::test]
    async fn test_normalize_unsandboxed_path_nonexistent_fallback() {
        let result = normalize_unsandboxed_path("/unlikely/path/qwerty12345.txt").await;
        assert!(
            result.to_str().unwrap().contains("qwerty12345.txt"),
            "expected filename preserved, got {}",
            result.display()
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_allowed_normal_files() {
        let tool = crate::ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "echo data.json"}))
            .await;
        assert!(result.is_ok());
    }

    // ── Auto-approved flag on builtins ────────────────────────────────────

    #[test]
    fn test_echo_is_auto_approved() {
        assert!(EchoTool::new().is_auto_approved());
    }

    #[test]
    fn test_dangerous_tools_are_not_auto_approved() {
        assert!(!MathTool::new().is_auto_approved());
        assert!(!HttpGetTool::new().is_auto_approved());
        assert!(!FsReadTool::new().is_auto_approved());
        assert!(!FsWriteTool::new().is_auto_approved());
        assert!(!FsListTool::new().is_auto_approved());
        assert!(!FsEditTool::new().is_auto_approved());
    }

    #[test]
    fn test_tool_descriptions() {
        assert!(!EchoTool::new().description().is_empty());
        assert!(!MathTool::new().description().is_empty());
        assert!(!HttpGetTool::new().description().is_empty());
        assert!(!FsReadTool::new().description().is_empty());
        assert!(!FsWriteTool::new().description().is_empty());
        assert!(!FsListTool::new().description().is_empty());
        assert!(!FsEditTool::new().description().is_empty());
    }

    // ── FileStateCache integration tests (cross-tool) ──────────────────────

    use crate::file_state_cache::FileStateCache;

    /// Helper: create a cache-enabled fs_read tool.
    fn fs_read_with_cache(cache: Arc<FileStateCache>) -> FsReadTool {
        FsReadTool::new().with_cache(cache)
    }

    /// Helper: create a cache-enabled fs_write tool.
    fn fs_write_with_cache(cache: Arc<FileStateCache>) -> FsWriteTool {
        FsWriteTool::new().with_cache(cache)
    }

    /// Helper: create a cache-enabled fs_edit tool.
    fn fs_edit_with_cache(cache: Arc<FileStateCache>) -> FsEditTool {
        FsEditTool::new().with_cache(cache)
    }

    #[tokio::test]
    async fn test_write_without_prior_read_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("existing.txt");
        std::fs::write(&file, "hello").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let write_tool = fs_write_with_cache(cache);

        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "overwrite"
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not been read yet"),
            "expected 'not been read' error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_edit_without_prior_read_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("existing.txt");
        std::fs::write(&file, "hello world").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let edit_tool = fs_edit_with_cache(cache);

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "goodbye"
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not been read yet"),
            "expected 'not been read' error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_read_then_write_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "original").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let write_tool = fs_write_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "updated"
            }))
            .await;
        assert!(result.is_ok(), "write after read should succeed");
    }

    #[tokio::test]
    async fn test_read_then_edit_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let edit_tool = fs_edit_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "goodbye"
            }))
            .await;
        assert!(result.is_ok(), "edit after read should succeed");
    }

    #[tokio::test]
    async fn test_read_external_modification_then_edit_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "original").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let edit_tool = fs_edit_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&file, "externally modified").unwrap();

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "externally",
                "new_string": "agent"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("modified since"),
            "expected staleness error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_new_file_creation_via_fs_write_bypasses_guard() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brand_new.txt");
        assert!(!file.exists());

        let cache = Arc::new(FileStateCache::default());
        let write_tool = fs_write_with_cache(cache);

        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "new content"
            }))
            .await;
        assert!(
            result.is_ok(),
            "new file creation should bypass guard: {:?}",
            result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_new_file_creation_via_fs_edit_empty_old_string_bypasses_guard() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brand_new.txt");
        assert!(!file.exists());

        let cache = Arc::new(FileStateCache::default());
        let edit_tool = fs_edit_with_cache(cache);

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "",
                "new_string": "new content"
            }))
            .await;
        assert!(
            result.is_ok(),
            "new file via empty old_string should bypass guard: {:?}",
            result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_string_updates_cache_for_subsequent_ops() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("created_then_edited.txt");
        assert!(!file.exists());

        let cache = Arc::new(FileStateCache::default());
        let edit_tool = fs_edit_with_cache(cache.clone());
        let write_tool = fs_write_with_cache(cache);

        edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "",
                "new_string": "initial content"
            }))
            .await
            .expect("new file creation via empty old_string should succeed");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "initial content");

        let edit_result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "initial",
                "new_string": "updated"
            }))
            .await;
        assert!(
            edit_result.is_ok(),
            "edit after empty-old_string creation should succeed: {:?}",
            edit_result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "updated content");

        let write_result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "final content"
            }))
            .await;
        assert!(
            write_result.is_ok(),
            "write after edit-after-creation should succeed: {:?}",
            write_result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "final content");
    }

    #[tokio::test]
    async fn test_partial_read_allows_subsequent_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("long.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let write_tool = fs_write_with_cache(cache);

        read_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "offset": 0,
                "limit": 5
            }))
            .await
            .unwrap();

        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "replaced"
            }))
            .await;
        assert!(
            result.is_ok(),
            "write after partial read should succeed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_partial_read_allows_subsequent_edit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("long.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&file, &content).unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let edit_tool = fs_edit_with_cache(cache);

        read_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "offset": 0,
                "limit": 5
            }))
            .await
            .unwrap();

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "line 42\n",
                "new_string": "LINE 42\n"
            }))
            .await;
        assert!(
            result.is_ok(),
            "edit after partial read should succeed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_tools_work_without_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("no_cache.txt");
        std::fs::write(&file, "content").unwrap();

        let write_tool = FsWriteTool::new();
        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "overwritten"
            }))
            .await;
        assert!(
            result.is_ok(),
            "write without cache should succeed: {:?}",
            result.err()
        );

        let edit_tool = FsEditTool::new();
        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "overwritten",
                "new_string": "edited"
            }))
            .await;
        assert!(
            result.is_ok(),
            "edit without cache should succeed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_read_write_write_consecutive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("consecutive.txt");
        std::fs::write(&file, "original").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let write_tool = fs_write_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "first write"
            }))
            .await
            .expect("first write after read should succeed");

        let result = write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "second write"
            }))
            .await;
        assert!(
            result.is_ok(),
            "second consecutive write should succeed: {:?}",
            result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second write");
    }

    #[tokio::test]
    async fn test_read_edit_edit_consecutive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("consecutive.txt");
        std::fs::write(&file, "alpha beta gamma").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let edit_tool = fs_edit_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "alpha",
                "new_string": "ALPHA"
            }))
            .await
            .expect("first edit after read should succeed");

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "beta",
                "new_string": "BETA"
            }))
            .await;
        assert!(
            result.is_ok(),
            "second consecutive edit should succeed: {:?}",
            result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ALPHA BETA gamma");
    }

    #[tokio::test]
    async fn test_read_write_edit_consecutive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mixed.txt");
        std::fs::write(&file, "original").unwrap();

        let cache = Arc::new(FileStateCache::default());
        let read_tool = fs_read_with_cache(cache.clone());
        let write_tool = fs_write_with_cache(cache.clone());
        let edit_tool = fs_edit_with_cache(cache);

        read_tool
            .execute(serde_json::json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        write_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "hello world"
            }))
            .await
            .expect("write after read should succeed");

        let result = edit_tool
            .execute(serde_json::json!({
                "path": file.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "goodbye"
            }))
            .await;
        assert!(
            result.is_ok(),
            "edit after write should succeed: {:?}",
            result.err()
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye world");
    }
}
