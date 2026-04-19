use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use globset::GlobBuilder;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{
    check_sandbox_path_async, check_sandbox_path_with_extras_async, is_blocked_device_path,
    reject_unc_path, relativize, walk_filtered_files_with_extras,
};

/// Maximum number of files returned by `fs_glob`.
const MAX_GLOB_RESULTS: usize = 200;

/// Fast file pattern matching tool for file discovery by name pattern.
///
/// Searches for files matching glob patterns (e.g. `**/*.rs`, `*.json`)
/// within the sandbox. Returns paths relative to the search base, sorted
/// by modification time (most recent first).
///
/// Security: respects sandbox root and VCS directory exclusion. Marked as
/// read-only and builtin.
#[derive(Debug, Clone, Default)]
pub struct FsGlobTool {
    sandbox_root: Option<PathBuf>,
    /// Additional read-only roots (see #242).
    extra_read_roots: Vec<PathBuf>,
}

impl FsGlobTool {
    /// Create an unrestricted fs_glob tool (no sandbox check).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed fs_glob tool. Paths must resolve within `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
            extra_read_roots: Vec::new(),
        }
    }

    /// Attach additional read-only roots.  Absolute paths that resolve inside
    /// any of these roots will be searchable in addition to the primary
    /// sandbox root.
    pub fn with_extra_read_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.extra_read_roots = roots;
        self
    }
}

#[async_trait::async_trait]
impl Tool for FsGlobTool {
    fn name(&self) -> &str {
        "fs_glob"
    }

    fn description(&self) -> &str {
        "Find files matching glob patterns. Returns paths relative to the search base, \
         sorted by modification time (most recent first). Supports *, **, ?, and [...] \
         patterns. Maximum 200 results."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., \"**/*.rs\", \"src/**/*.toml\", \"*.json\")."
                },
                "path": {
                    "type": "string",
                    "description": "Base directory to search in. Defaults to sandbox root or cwd."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // ── Parse parameters ───────────────────────────────────────────

        let pattern_str = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'pattern' is required".to_string()))?;

        let path_param = params.get("path").and_then(|v| v.as_str());

        // ── Compile glob matcher ───────────────────────────────────────

        let glob_matcher = GlobBuilder::new(pattern_str)
            // literal_separator(true): standard glob semantics — `*` matches within
            // a single directory, `**` required for recursive. Expected for a path finder.
            .literal_separator(true)
            .build()
            .map(|glob| glob.compile_matcher())
            .map_err(|e| SandboxError::InvalidParameters(format!("Invalid glob pattern: {}", e)))?;

        // ── Resolve search root ────────────────────────────────────────

        let search_root: PathBuf = if let Some(path) = path_param {
            // Block UNC paths that could leak NTLM credentials via SMB.
            reject_unc_path(path)?;

            // Block device paths.
            if is_blocked_device_path(Path::new(path)) {
                return Err(SandboxError::SandboxViolation(format!(
                    "Cannot search device path '{}' — this is a system device, not a directory",
                    path
                )));
            }

            if let Some(ref root) = self.sandbox_root {
                if self.extra_read_roots.is_empty() {
                    check_sandbox_path_async(path, root).await?
                } else {
                    check_sandbox_path_with_extras_async(path, root, &self.extra_read_roots).await?
                }
            } else {
                PathBuf::from(path)
            }
        } else if let Some(ref root) = self.sandbox_root {
            root.clone()
        } else {
            std::env::current_dir().map_err(|e| {
                SandboxError::Io(format!("Cannot determine current directory: {}", e))
            })?
        };

        // ── Validate search root is a directory ────────────────────────

        if !search_root.exists() {
            return Err(SandboxError::Io(format!(
                "Directory not found: '{}'",
                search_root.display()
            )));
        }
        if !search_root.is_dir() {
            return Err(SandboxError::InvalidParameters(format!(
                "'path' must be a directory, not a file: '{}'",
                search_root.display()
            )));
        }

        // ── Collect matching files ─────────────────────────────────────

        let sandbox_root_clone = self.sandbox_root.clone();
        let extra_read_roots_clone = self.extra_read_roots.clone();
        let search_root_clone = search_root.clone();

        let mut files: Vec<(PathBuf, std::time::SystemTime)> =
            tokio::task::spawn_blocking(move || {
                collect_glob_files(
                    &search_root_clone,
                    sandbox_root_clone.as_deref(),
                    &extra_read_roots_clone,
                    &glob_matcher,
                )
            })
            .await
            .map_err(|e| SandboxError::Io(format!("Glob task failed: {}", e)))?;

        // Sort by mtime, most recent first.
        files.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        // Apply result limit.
        let truncated = files.len() > MAX_GLOB_RESULTS;
        let total = files.len();
        files.truncate(MAX_GLOB_RESULTS);

        // Build relative paths for output.
        let relative_files: Vec<String> = files
            .iter()
            .map(|(path, _)| relativize(path, &search_root))
            .collect();

        Ok(serde_json::json!({
            "files": relative_files,
            "total": total,
            "truncated": truncated
        }))
    }
}

/// Collect all files matching a glob pattern under `search_root`, respecting
/// sandbox and VCS exclusion.
///
/// Returns tuples of (path, modification_time) so the caller can sort by mtime.
fn collect_glob_files(
    search_root: &Path,
    sandbox_root: Option<&Path>,
    extra_read_roots: &[PathBuf],
    glob_matcher: &globset::GlobMatcher,
) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut files = Vec::new();
    walk_filtered_files_with_extras(
        search_root,
        sandbox_root,
        extra_read_roots,
        Some(glob_matcher),
        |entry| {
            // Use DirEntry::metadata() — already cached from the directory walk,
            // avoids an extra syscall per matched file vs std::fs::metadata().
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files.push((entry.path().to_path_buf(), mtime));
        },
    );
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to set up a directory tree for glob tests.
    fn setup_glob_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn add() {}\n").unwrap();

        std::fs::create_dir_all(root.join("src/utils")).unwrap();
        std::fs::write(root.join("src/utils/helpers.rs"), "pub fn help() {}\n").unwrap();

        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        std::fs::write(root.join("src/config.toml"), "[server]\nport = 8080\n").unwrap();
        std::fs::write(root.join("README.md"), "# Test\n").unwrap();

        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/input.json"), "{}\n").unwrap();

        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join(".git/config.rs"), "not a rust file\n").unwrap();

        std::fs::write(root.join("secrets.json"), "{\"key\": \"sk-1234\"}").unwrap();

        dir
    }

    #[tokio::test]
    async fn test_fs_glob_match_star_rs() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.rs",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_glob_recursive_rs() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.rs",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&"src/main.rs"));
        assert!(paths.contains(&"src/lib.rs"));
        assert!(paths.contains(&"src/utils/helpers.rs"));
        assert_eq!(result["total"], 3);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_glob_scoped_recursive_toml() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "src/**/*.toml",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert_eq!(paths.len(), 1);
        assert!(paths.contains(&"src/config.toml"));
    }

    #[tokio::test]
    async fn test_fs_glob_sorted_by_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("old.txt"), "old\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(root.join("new.txt"), "new\n").unwrap();

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.txt",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].as_str().unwrap(), "new.txt");
        assert_eq!(files[1].as_str().unwrap(), "old.txt");
    }

    /// `fs_glob` must sort strictly by mtime descending across more than
    /// two entries — a stronger guarantee than the two-entry test above
    /// and a regression guard for the "newest first" ordering required by
    /// #754 item 2.
    #[tokio::test]
    async fn test_fs_glob_sorted_by_mtime_strict_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Write in a random order — the sort should reorder them by mtime.
        let names = ["c.log", "a.log", "b.log", "d.log"];
        for name in &names {
            std::fs::write(root.join(name), "x\n").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Expected newest-first order is reverse of write order.

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.log",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files: Vec<String> = result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(files, vec!["d.log", "b.log", "a.log", "c.log"]);
    }

    #[tokio::test]
    async fn test_fs_glob_result_limit_and_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        for i in 0..210 {
            std::fs::write(root.join(format!("file_{:04}.txt", i)), "data\n").unwrap();
        }

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.txt",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 200);
        assert_eq!(result["total"], 210);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn test_fs_glob_vcs_dirs_excluded() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.rs",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(
            !paths.iter().any(|p| p.contains(".git")),
            "VCS directory contents should be excluded"
        );
    }

    /// Regression guard for #744: the hardcoded denied-filename filter
    /// was removed from `fs_glob`'s file walker. `secrets.json` must
    /// now be visible in glob results alongside `data/input.json`.
    #[tokio::test]
    async fn test_fs_glob_no_hardcoded_basename_exclusion() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.json",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(paths.contains(&"data/input.json"));
        assert!(
            paths.iter().any(|p| p.contains("secrets.json")),
            "secrets.json must now appear in glob results (hardcoded filter removed)"
        );
    }

    #[tokio::test]
    async fn test_fs_glob_sandbox_root_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGlobTool::sandboxed(root);

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*",
                "path": "../../"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_glob_nonexistent_directory() {
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.rs",
                "path": "/nonexistent/dir/that/does/not/exist"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found") || err.contains("Directory"),
            "Error should mention directory not found: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_glob_file_path_not_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello\n").unwrap();

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.txt",
                "path": file.to_str().unwrap()
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("directory") || err.contains("not a file"),
            "Error should mention path must be a directory: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_glob_empty_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("test.txt"), "hello\n").unwrap();

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.rs",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_glob_no_matches_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.xyz_nonexistent_ext",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert_eq!(result["files"].as_array().unwrap().len(), 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_glob_relative_path_output() {
        let dir = setup_glob_dir();
        let root = dir.path();
        let tool = FsGlobTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.rs",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        for f in files {
            let path_str = f.as_str().unwrap();
            assert!(
                !path_str.starts_with('/') && !path_str.contains(":\\"),
                "Path should be relative: {path_str}"
            );
            assert!(
                !path_str.contains('\\'),
                "Path should use forward slashes: {path_str}"
            );
        }
    }

    #[tokio::test]
    async fn test_fs_glob_missing_pattern_is_error() {
        let tool = FsGlobTool::new();
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_glob_invalid_pattern_is_error() {
        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "[invalid",
                "path": "."
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("glob") || err.contains("Invalid"),
            "Error should mention invalid glob: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_glob_is_readonly_and_builtin() {
        let tool = FsGlobTool::new();
        assert!(tool.is_builtin());
        assert_eq!(tool.name(), "fs_glob");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn test_fs_glob_no_path_defaults_to_sandbox_root() {
        let dir = setup_glob_dir();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGlobTool::sandboxed(root);

        let result = tool
            .execute(serde_json::json!({
                "pattern": "**/*.rs"
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    async fn test_fs_glob_question_mark_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("ab.txt"), "ab\n").unwrap();
        std::fs::write(root.join("abc.txt"), "abc\n").unwrap();

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "?.txt",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert_eq!(paths.len(), 1);
        assert!(paths.contains(&"a.txt"));
    }

    #[tokio::test]
    async fn test_fs_glob_character_class() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("test.rs"), "rs\n").unwrap();
        std::fs::write(root.join("test.py"), "py\n").unwrap();
        std::fs::write(root.join("test.js"), "js\n").unwrap();

        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({
                "pattern": "test.[rp]*",
                "path": root.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(paths.contains(&"test.rs"));
        assert!(paths.contains(&"test.py"));
        assert!(!paths.contains(&"test.js"));
    }

    #[tokio::test]
    async fn test_fs_glob_rejects_unc_path() {
        let tool = FsGlobTool::new();
        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs", "path": "\\\\server\\share"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNC paths"), "error should mention UNC: {err}");

        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs", "path": "//server/share"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UNC paths"));
    }

    // ── extra_read_roots (sibling workspace access, #242) ─────────────────

    #[tokio::test]
    async fn test_fs_glob_extra_root_allows_sibling_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        let child_ws = workspace_dir.join("child");
        std::fs::create_dir_all(&parent_ws).unwrap();
        std::fs::create_dir_all(&child_ws).unwrap();
        std::fs::write(child_ws.join("memories.md"), "m").unwrap();
        std::fs::write(child_ws.join("goals.md"), "g").unwrap();
        std::fs::write(child_ws.join("personality.md"), "p").unwrap();

        let tool =
            FsGlobTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir.clone()]);
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.md",
                "path": child_ws.to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(
            files.len(),
            3,
            "expected exactly the 3 .md files written above, got {}",
            files.len()
        );
    }

    #[tokio::test]
    async fn test_fs_glob_extra_root_does_not_widen_to_outside() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        std::fs::create_dir_all(&parent_ws).unwrap();

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("leak.md"), "x").unwrap();

        let tool = FsGlobTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir]);
        let result = tool
            .execute(serde_json::json!({
                "pattern": "*.md",
                "path": outside.path().to_str().unwrap()
            }))
            .await;
        assert!(result.is_err());
    }
}
