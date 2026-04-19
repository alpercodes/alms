use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use globset::GlobBuilder;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{
    check_sandbox_path_async, check_sandbox_path_with_extras_async, is_blocked_device_path,
    reject_unc_path, relativize, walk_filtered_files_with_extras,
};

/// Maximum total output size in characters. Prevents context window overload.
const MAX_OUTPUT_CHARS: usize = 20_000;

/// Default head_limit when not specified by the caller.
const DEFAULT_HEAD_LIMIT: usize = 250;

/// Maximum file size (in bytes) for `search_file_content` which reads the
/// entire file into memory.  Files larger than this are skipped with a debug
/// log.  1 MiB is generous enough for most source files while preventing OOM
/// on multi-GB log files.
const MAX_GREP_FILE_BYTES: u64 = 1_024 * 1_024;

/// Regex content search tool for files.
///
/// Searches file contents using regex patterns within the sandbox. Supports
/// three output modes (files_with_matches, content, count), glob filtering,
/// case-insensitive matching, and result pagination via head_limit/offset.
///
/// Security: respects sandbox root and VCS directory exclusion. Marked as
/// read-only and builtin.
#[derive(Debug, Clone, Default)]
pub struct FsGrepTool {
    sandbox_root: Option<PathBuf>,
    /// Additional read-only roots (see #242).
    extra_read_roots: Vec<PathBuf>,
}

impl FsGrepTool {
    /// Create an unrestricted fs_grep tool (no sandbox check).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed fs_grep tool. Paths must resolve within `root`.
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

/// A single content match with optional context lines.
#[derive(Debug)]
struct ContentMatch {
    line: usize,
    content: String,
    context_before: Vec<String>,
    context_after: Vec<String>,
}

#[async_trait::async_trait]
impl Tool for FsGrepTool {
    fn name(&self) -> &str {
        "fs_grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regex patterns. Returns matching file paths, content with \
         context lines, or per-file match counts. Supports glob filtering, case-insensitive \
         matching, and result pagination. Output capped at 20,000 characters."
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
                    "description": "Regex pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in. Defaults to sandbox root or cwd."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., \"*.rs\", \"**/*.toml\")."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "Output mode: \"files_with_matches\" (default), \"content\", or \"count\"."
                },
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Lines of context before and after each match (content mode only). Default: 0."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case-insensitive matching. Default: false."
                },
                "head_limit": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Max results to return (0 = unlimited). Default: 250."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Skip first N results before applying head_limit. Default: 0."
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
        let glob_param = params.get("glob").and_then(|v| v.as_str());

        let output_mode = params
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("files_with_matches");

        // Validate output_mode
        if !["files_with_matches", "content", "count"].contains(&output_mode) {
            return Err(SandboxError::InvalidParameters(format!(
                "Invalid output_mode '{}': must be 'files_with_matches', 'content', or 'count'",
                output_mode
            )));
        }

        let context_lines = params
            .get("context")
            .map(|v| {
                v.as_u64().ok_or_else(|| {
                    SandboxError::InvalidParameters(
                        "'context' must be a non-negative integer".to_string(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(0) as usize;

        let case_insensitive = params
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let head_limit = params
            .get("head_limit")
            .map(|v| {
                v.as_u64().ok_or_else(|| {
                    SandboxError::InvalidParameters(
                        "'head_limit' must be a non-negative integer".to_string(),
                    )
                })
            })
            .transpose()?
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_HEAD_LIMIT);

        let offset = params
            .get("offset")
            .map(|v| {
                v.as_u64().ok_or_else(|| {
                    SandboxError::InvalidParameters(
                        "'offset' must be a non-negative integer".to_string(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(0) as usize;

        // ── Compile regex ──────────────────────────────────────────────

        let re = regex::RegexBuilder::new(pattern_str)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| {
                SandboxError::InvalidParameters(format!("Invalid regex pattern: {}", e))
            })?;

        // ── Compile glob filter (if provided) ──────────────────────────

        let glob_matcher = glob_param
            .map(|g| {
                GlobBuilder::new(g)
                    .literal_separator(false)
                    .build()
                    .map(|glob| glob.compile_matcher())
                    .map_err(|e| {
                        SandboxError::InvalidParameters(format!("Invalid glob pattern: {}", e))
                    })
            })
            .transpose()?;

        // ── Resolve search root ────────────────────────────────────────

        let search_root: PathBuf = if let Some(path) = path_param {
            reject_unc_path(path)?;

            if is_blocked_device_path(Path::new(path)) {
                return Err(SandboxError::SandboxViolation(format!(
                    "Cannot search device path '{}' — this is a system device, not a regular file",
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

        // ── Collect files to search ────────────────────────────────────

        let sandbox_root_clone = self.sandbox_root.clone();
        let extra_read_roots_clone = self.extra_read_roots.clone();
        let search_root_clone = search_root.clone();
        let glob_matcher_clone = glob_matcher.clone();

        let files: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
            collect_files(
                &search_root_clone,
                sandbox_root_clone.as_deref(),
                &extra_read_roots_clone,
                glob_matcher_clone.as_ref(),
            )
        })
        .await
        .map_err(|e| SandboxError::Io(format!("File collection task failed: {}", e)))?;

        // ── Search files ───────────────────────────────────────────────

        match output_mode {
            "files_with_matches" => {
                search_files_with_matches(&re, &files, &search_root, head_limit, offset).await
            }
            "content" => {
                search_content(&re, &files, &search_root, context_lines, head_limit, offset).await
            }
            "count" => search_count(&re, &files, &search_root, head_limit, offset).await,
            _ => unreachable!(), // validated above
        }
    }
}

/// Collect all searchable files under `search_root`, respecting sandbox, VCS
/// exclusion, and optional glob filtering.
///
/// When `search_root` is a regular file (not a directory), returns a
/// single-element vec containing just that file (single-file mode).
fn collect_files(
    search_root: &Path,
    sandbox_root: Option<&Path>,
    extra_read_roots: &[PathBuf],
    glob_matcher: Option<&globset::GlobMatcher>,
) -> Vec<PathBuf> {
    // Single-file mode: if the search root is a file, just search that file.
    if search_root.is_file() {
        if is_blocked_device_path(search_root) {
            return Vec::new();
        }
        return vec![search_root.to_path_buf()];
    }

    let mut files = Vec::new();
    walk_filtered_files_with_extras(
        search_root,
        sandbox_root,
        extra_read_roots,
        glob_matcher,
        |entry| {
            files.push(entry.path().to_path_buf());
        },
    );

    // Sort for deterministic output order.
    files.sort();
    files
}

/// `files_with_matches` mode: return file paths that contain at least one match.
async fn search_files_with_matches(
    re: &Regex,
    files: &[PathBuf],
    search_root: &Path,
    head_limit: usize,
    offset: usize,
) -> SandboxResult<Value> {
    let mut matched_files: Vec<String> = Vec::new();
    let mut total_found: usize = 0;
    let mut output_chars: usize = 0;
    let mut truncated = false;

    let effective_limit = if head_limit == 0 {
        usize::MAX
    } else {
        head_limit
    };

    for file in files {
        if file_matches_regex(re, file).await? {
            total_found += 1;

            if total_found <= offset {
                continue;
            }

            if matched_files.len() >= effective_limit {
                truncated = true;
                break;
            }

            let rel = relativize(file, search_root);

            output_chars += rel.len() + 4;
            if output_chars > MAX_OUTPUT_CHARS {
                truncated = true;
                break;
            }

            matched_files.push(rel);
        }
    }

    Ok(serde_json::json!({
        "matches": matched_files,
        "total": total_found,
        "truncated": truncated
    }))
}

/// `content` mode: return matching lines with context.
async fn search_content(
    re: &Regex,
    files: &[PathBuf],
    search_root: &Path,
    context_lines: usize,
    head_limit: usize,
    offset: usize,
) -> SandboxResult<Value> {
    let mut results: Vec<Value> = Vec::new();
    let mut total_found: usize = 0;
    let mut output_chars: usize = 0;
    let mut truncated = false;

    let effective_limit = if head_limit == 0 {
        usize::MAX
    } else {
        head_limit
    };

    'outer: for file in files {
        let matches = search_file_content(re, file, context_lines).await?;
        for m in matches {
            total_found += 1;

            if total_found <= offset {
                continue;
            }

            if results.len() >= effective_limit {
                truncated = true;
                break 'outer;
            }

            let rel = relativize(file, search_root);

            let ctx_before = if context_lines > 0 {
                if let Some(prev) = results.last() {
                    let same_file = prev.get("file").and_then(|v| v.as_str()) == Some(&rel);
                    if same_file {
                        let prev_line =
                            prev.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let prev_after_len = prev
                            .get("context_after")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let prev_coverage = prev_line + prev_after_len;
                        let cur_ctx_start = m.line.saturating_sub(m.context_before.len());
                        if prev_coverage >= cur_ctx_start {
                            let overlap = prev_coverage - cur_ctx_start + 1;
                            let skip = overlap.min(m.context_before.len());
                            m.context_before[skip..].to_vec()
                        } else {
                            m.context_before.clone()
                        }
                    } else {
                        m.context_before.clone()
                    }
                } else {
                    m.context_before.clone()
                }
            } else {
                Vec::new()
            };

            let entry = serde_json::json!({
                "file": rel,
                "line": m.line,
                "content": m.content,
                "context_before": ctx_before,
                "context_after": m.context_after
            });

            let entry_size = serde_json::to_string(&entry).unwrap_or_default().len();
            output_chars += entry_size + 2;
            if output_chars > MAX_OUTPUT_CHARS {
                truncated = true;
                break 'outer;
            }

            results.push(entry);
        }
    }

    Ok(serde_json::json!({
        "matches": results,
        "total": total_found,
        "truncated": truncated
    }))
}

/// `count` mode: return per-file match counts.
async fn search_count(
    re: &Regex,
    files: &[PathBuf],
    search_root: &Path,
    head_limit: usize,
    offset: usize,
) -> SandboxResult<Value> {
    let mut results: Vec<Value> = Vec::new();
    let mut total_matches: usize = 0;
    let mut files_found: usize = 0;
    let mut output_chars: usize = 0;
    let mut truncated = false;

    let effective_limit = if head_limit == 0 {
        usize::MAX
    } else {
        head_limit
    };

    for file in files {
        let count = count_file_matches(re, file).await?;
        if count == 0 {
            continue;
        }

        total_matches += count;
        files_found += 1;

        if files_found <= offset {
            continue;
        }

        if results.len() >= effective_limit {
            truncated = true;
            break;
        }

        let rel = relativize(file, search_root);
        let entry = serde_json::json!({
            "file": rel,
            "count": count
        });

        let entry_size = serde_json::to_string(&entry).unwrap_or_default().len();
        output_chars += entry_size + 2;
        if output_chars > MAX_OUTPUT_CHARS {
            truncated = true;
            break;
        }

        results.push(entry);
    }

    Ok(serde_json::json!({
        "matches": results,
        "total_matches": total_matches,
        "truncated": truncated
    }))
}

/// Check whether a file contains at least one regex match (line-by-line).
async fn file_matches_regex(re: &Regex, path: &Path) -> SandboxResult<bool> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
            return Ok(false);
        }
    };

    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if re.is_match(&line) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Search a single file for content matches with context lines.
///
/// Checks file size before reading to prevent OOM on very large files.
/// Files exceeding `MAX_GREP_FILE_BYTES` (1 MiB) are skipped with a debug log.
async fn search_file_content(
    re: &Regex,
    path: &Path,
    context_lines: usize,
) -> SandboxResult<Vec<ContentMatch>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            if meta.len() > MAX_GREP_FILE_BYTES {
                tracing::debug!(
                    path = %path.display(),
                    size = meta.len(),
                    limit = MAX_GREP_FILE_BYTES,
                    "Skipping file exceeding grep size limit"
                );
                return Ok(Vec::new());
            }
        }
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping file with unreadable metadata");
            return Ok(Vec::new());
        }
    }

    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
            return Ok(Vec::new());
        }
    };

    let all_lines: Vec<&str> = content.lines().collect();
    let mut matches = Vec::new();

    for (idx, line) in all_lines.iter().enumerate() {
        if re.is_match(line) {
            let line_num = idx + 1; // 1-based

            let ctx_before: Vec<String> = if context_lines > 0 {
                let start = idx.saturating_sub(context_lines);
                all_lines[start..idx]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                Vec::new()
            };

            let ctx_after: Vec<String> = if context_lines > 0 {
                let end = (idx + 1 + context_lines).min(all_lines.len());
                all_lines[(idx + 1)..end]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                Vec::new()
            };

            matches.push(ContentMatch {
                line: line_num,
                content: line.to_string(),
                context_before: ctx_before,
                context_after: ctx_after,
            });
        }
    }

    Ok(matches)
}

/// Count regex matches in a single file (line-by-line).
async fn count_file_matches(re: &Regex, path: &Path) -> SandboxResult<usize> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
            return Ok(0);
        }
    };

    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut count: usize = 0;

    while let Ok(Some(line)) = lines.next_line().await {
        if re.is_match(&line) {
            count += 1;
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a temp directory with test files for grep tests.
    fn setup_grep_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"Hello, world!\");\n    let x = 42;\n}\n",
        )
        .unwrap();

        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn hello() {\n    println!(\"Hello from lib\");\n}\n",
        )
        .unwrap();

        std::fs::write(
            root.join("README.md"),
            "# My Project\n\nHello world example.\n",
        )
        .unwrap();

        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("data/config.toml"),
            "[server]\nport = 8080\nhost = \"localhost\"\n",
        )
        .unwrap();

        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        std::fs::write(root.join("secrets.json"), "{\"key\": \"sk-1234\"}").unwrap();

        dir
    }

    #[tokio::test]
    async fn test_fs_grep_literal_pattern_single_file() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/main.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": file.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line"], 2);
        assert!(matches[0]["content"].as_str().unwrap().contains("println"));
        assert_eq!(result["total"], 1);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_regex_pattern_across_directory() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "^(pub )?fn \\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        assert_eq!(result["total"], 3);
        assert!(!result["matches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_fs_grep_files_with_matches_mode() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(result["total"], 2);
        let paths: Vec<&str> = matches.iter().map(|m| m.as_str().unwrap()).collect();
        assert!(paths.contains(&"src/lib.rs"));
        assert!(paths.contains(&"src/main.rs"));
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_content_mode_line_numbers() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/lib.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "pub fn",
                "path": file.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["line"], 1);
        assert_eq!(matches[1]["line"], 5);
    }

    #[tokio::test]
    async fn test_fs_grep_count_mode() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(result["total_matches"], 2);
        assert_eq!(matches.len(), 2);

        for m in matches {
            assert!(m["file"].as_str().is_some());
            assert!(m["count"].as_u64().unwrap() > 0);
        }
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_context_lines() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/main.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "let x",
                "path": file.to_str().unwrap(),
                "output_mode": "content",
                "context": 1
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line"], 3);
        assert!(
            matches[0]["content"]
                .as_str()
                .unwrap()
                .contains("let x = 42")
        );

        let ctx_before = matches[0]["context_before"].as_array().unwrap();
        assert_eq!(ctx_before.len(), 1);
        assert!(ctx_before[0].as_str().unwrap().contains("println"));

        let ctx_after = matches[0]["context_after"].as_array().unwrap();
        assert_eq!(ctx_after.len(), 1);
        assert!(ctx_after[0].as_str().unwrap().contains("}"));
    }

    #[tokio::test]
    async fn test_fs_grep_case_insensitive() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result_sensitive = tool
            .execute(serde_json::json!({
                "pattern": "hello",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "case_insensitive": false
            }))
            .await
            .unwrap();

        let result_insensitive = tool
            .execute(serde_json::json!({
                "pattern": "hello",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "case_insensitive": true
            }))
            .await
            .unwrap();

        let sensitive_total = result_sensitive["total"].as_u64().unwrap();
        let insensitive_total = result_insensitive["total"].as_u64().unwrap();
        assert!(
            insensitive_total >= sensitive_total,
            "Case-insensitive should find at least as many matches: {insensitive_total} >= {sensitive_total}"
        );
        assert!(insensitive_total >= 2);
    }

    #[tokio::test]
    async fn test_fs_grep_head_limit_truncates() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "head_limit": 1
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(result["truncated"], true);
        assert!(result["total"].as_u64().unwrap() > 1);
    }

    #[tokio::test]
    async fn test_fs_grep_offset_pagination() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let all = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "head_limit": 0
            }))
            .await
            .unwrap();
        let all_matches = all["matches"].as_array().unwrap();
        let total = all_matches.len();
        assert!(total >= 3, "Need at least 3 files for pagination test");

        let page1 = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "head_limit": 2,
                "offset": 0
            }))
            .await
            .unwrap();
        let p1_matches = page1["matches"].as_array().unwrap();
        assert_eq!(p1_matches.len(), 2);

        let page2 = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "head_limit": 2,
                "offset": 2
            }))
            .await
            .unwrap();
        let p2_matches = page2["matches"].as_array().unwrap();
        assert!(!p2_matches.is_empty());

        let p1_set: std::collections::HashSet<&str> =
            p1_matches.iter().map(|m| m.as_str().unwrap()).collect();
        for m in p2_matches {
            assert!(
                !p1_set.contains(m.as_str().unwrap()),
                "Page 2 should not overlap with page 1"
            );
        }
    }

    #[tokio::test]
    async fn test_fs_grep_glob_filter() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "Hello",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "glob": "**/*.rs",
                "case_insensitive": true
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        for m in matches {
            assert!(
                m.as_str().unwrap().ends_with(".rs"),
                "Non-.rs file found: {}",
                m
            );
        }
        assert!(!matches.is_empty());
    }

    #[tokio::test]
    async fn test_fs_grep_vcs_dirs_excluded() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "refs/heads/main",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            0,
            "VCS directory contents should be excluded"
        );
    }

    /// Regression guard for #744: the hardcoded denied-filename filter
    /// was removed from `fs_grep`'s file walker. Matches inside a file
    /// whose basename is `secrets.json` are now included in results.
    #[tokio::test]
    async fn test_fs_grep_no_hardcoded_basename_exclusion() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "sk-1234",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            1,
            "secrets.json must now appear in grep results (hardcoded filter removed)"
        );
        assert!(
            matches[0].as_str().unwrap().ends_with("secrets.json"),
            "expected secrets.json match, got {:?}",
            matches[0]
        );
    }

    #[tokio::test]
    async fn test_fs_grep_sandbox_root_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGrepTool::sandboxed(root);

        let result = tool
            .execute(serde_json::json!({
                "pattern": "test",
                "path": "../../"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_grep_invalid_regex() {
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "[invalid",
                "path": "."
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("regex") || err.contains("Invalid"),
            "Error should mention regex: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_grep_empty_results() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "zzz_nonexistent_pattern_zzz",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_empty_results_content_mode() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "zzz_nonexistent_pattern_zzz",
                "path": root.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_empty_results_count_mode() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "zzz_nonexistent_pattern_zzz",
                "path": root.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 0);
        assert_eq!(result["total_matches"], 0);
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn test_fs_grep_default_output_mode_is_files_with_matches() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/main.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "main",
                "path": file.to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result["matches"].is_array());
        assert!(result["total"].is_number());
        let matches = result["matches"].as_array().unwrap();
        if !matches.is_empty() {
            assert!(matches[0].is_string());
        }
    }

    #[tokio::test]
    async fn test_fs_grep_invalid_output_mode() {
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "test",
                "path": ".",
                "output_mode": "invalid_mode"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("output_mode"));
    }

    #[tokio::test]
    async fn test_fs_grep_single_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line one\nline two\nline three\n").unwrap();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "two",
                "path": file.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line"], 2);
        assert!(matches[0]["content"].as_str().unwrap().contains("two"));
    }

    #[tokio::test]
    async fn test_fs_grep_path_relativization() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "fn main",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let path = matches[0].as_str().unwrap();
        assert_eq!(path, "src/main.rs");
        assert!(!path.starts_with('/'));
        assert!(!path.contains('\\'));
    }

    #[tokio::test]
    async fn test_fs_grep_head_limit_zero_means_unlimited() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "head_limit": 0
            }))
            .await
            .unwrap();

        assert_eq!(result["truncated"], false);
        let total = result["total"].as_u64().unwrap();
        let returned = result["matches"].as_array().unwrap().len() as u64;
        assert_eq!(total, returned);
    }

    #[tokio::test]
    async fn test_fs_grep_is_readonly_and_builtin() {
        let tool = FsGrepTool::new();
        assert!(tool.is_builtin());
        assert!(!tool.is_auto_approved());
    }

    #[tokio::test]
    async fn test_fs_grep_missing_pattern_is_error() {
        let tool = FsGrepTool::new();
        let result = tool.execute(serde_json::json!({"path": "."})).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pattern"));
    }

    #[tokio::test]
    async fn test_fs_grep_context_at_file_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("edge.txt");
        std::fs::write(&file, "first\nsecond\nthird\n").unwrap();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "first",
                "path": file.to_str().unwrap(),
                "output_mode": "content",
                "context": 2
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["context_before"].as_array().unwrap().is_empty());
        assert_eq!(matches[0]["context_after"].as_array().unwrap().len(), 2);

        let result2 = tool
            .execute(serde_json::json!({
                "pattern": "third",
                "path": file.to_str().unwrap(),
                "output_mode": "content",
                "context": 2
            }))
            .await
            .unwrap();

        let matches2 = result2["matches"].as_array().unwrap();
        assert_eq!(matches2.len(), 1);
        assert!(matches2[0]["context_after"].as_array().unwrap().is_empty());
        assert_eq!(matches2[0]["context_before"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_fs_grep_output_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..200 {
            let content = format!("match_line_{i}\n{}\n", "x".repeat(200));
            std::fs::write(root.join(format!("file_{i:03}.txt")), content).unwrap();
        }

        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "match_line_\\d+",
                "path": root.to_str().unwrap(),
                "output_mode": "content",
                "head_limit": 0,
                "context": 1
            }))
            .await
            .unwrap();

        assert!(result["matches"].is_array());
        assert!(result["total"].is_number());
    }

    #[tokio::test]
    async fn test_fs_grep_glob_filter_toml_only() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "port",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "glob": "**/*.toml"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].as_str().unwrap().ends_with("config.toml"));
    }

    #[tokio::test]
    async fn test_fs_grep_count_mode_head_limit() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "\\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "count",
                "head_limit": 1
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn test_fs_grep_offset_beyond_results() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "fn main",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches",
                "offset": 1000
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert!(matches.is_empty());
        assert_eq!(result["total"], 1);
    }

    #[tokio::test]
    async fn test_fs_grep_no_path_defaults_to_sandbox_root() {
        let dir = setup_grep_dir();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGrepTool::sandboxed(root);

        let result = tool
            .execute(serde_json::json!({
                "pattern": "fn main",
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].as_str().unwrap().contains("main.rs"));
    }

    #[tokio::test]
    async fn test_fs_grep_rejects_unc_path() {
        let tool = FsGrepTool::new();
        let result = tool
            .execute(serde_json::json!({"pattern": "test", "path": "\\\\server\\share"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNC paths"), "error should mention UNC: {err}");

        let result = tool
            .execute(serde_json::json!({"pattern": "test", "path": "//server/share"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UNC paths"));
    }

    // ── extra_read_roots (sibling workspace access, #242) ─────────────────

    #[tokio::test]
    async fn test_fs_grep_extra_root_allows_sibling_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        let child_ws = workspace_dir.join("child");
        std::fs::create_dir_all(&parent_ws).unwrap();
        std::fs::create_dir_all(&child_ws).unwrap();
        std::fs::write(child_ws.join("memories.md"), "Learned: alpha beta\n").unwrap();

        let tool =
            FsGrepTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir.clone()]);
        let result = tool
            .execute(serde_json::json!({
                "pattern": "alpha",
                "path": child_ws.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "should find memories.md in sibling ws");
    }

    #[tokio::test]
    async fn test_fs_grep_extra_root_does_not_widen_to_outside() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        std::fs::create_dir_all(&parent_ws).unwrap();

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("leak.md"), "secret content\n").unwrap();

        let tool = FsGrepTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir]);
        let result = tool
            .execute(serde_json::json!({
                "pattern": "secret",
                "path": outside.path().to_str().unwrap()
            }))
            .await;
        assert!(result.is_err(), "outside path must be rejected");
    }
}
