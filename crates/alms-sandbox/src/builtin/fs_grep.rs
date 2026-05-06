use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use globset::GlobBuilder;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::BufReader;

use super::line_cap::{LineRead, read_line_capped};
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

/// Choose the directory to relativize match paths against.
///
/// When the agent passes `path` pointing at a single file, `search_root` is
/// that file itself, so `relativize(file, search_root)` strips to the empty
/// string and the per-match `file` field comes back as `""` (issue #972).
///
/// In single-file mode we relativize against the file's parent instead, which
/// yields the basename — matching the directory-walk shape (relative path with
/// a non-empty filename).
fn relativize_base(search_root: &Path) -> &Path {
    if search_root.is_file() {
        search_root.parent().unwrap_or(search_root)
    } else {
        search_root
    }
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
    let mut truncated_lines: u64 = 0;

    let effective_limit = if head_limit == 0 {
        usize::MAX
    } else {
        head_limit
    };

    let rel_base = relativize_base(search_root);

    for file in files {
        let scan = file_matches_regex(re, file).await?;
        truncated_lines = truncated_lines.saturating_add(scan.truncated_lines);
        if scan.matched {
            total_found += 1;

            if total_found <= offset {
                continue;
            }

            if matched_files.len() >= effective_limit {
                truncated = true;
                break;
            }

            let rel = relativize(file, rel_base);

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
        "truncated": truncated,
        "truncated_lines": truncated_lines
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

    let rel_base = relativize_base(search_root);

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

            let rel = relativize(file, rel_base);

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
        "truncated": truncated,
        // Content mode reads whole files (skipping any >1 MiB via the
        // `MAX_GREP_FILE_BYTES` gate), so the per-line cap from
        // `read_line_capped` is never exercised here.  The field is emitted
        // unconditionally for response-shape consistency with the other
        // output modes.
        "truncated_lines": 0
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

    let mut truncated_lines: u64 = 0;

    let rel_base = relativize_base(search_root);

    for file in files {
        let scan = count_file_matches(re, file).await?;
        truncated_lines = truncated_lines.saturating_add(scan.truncated_lines);
        let count = scan.count;
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

        let rel = relativize(file, rel_base);
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
        "truncated": truncated,
        "truncated_lines": truncated_lines
    }))
}

/// Result of a per-line scan.  Carries both the boolean answer and a count of
/// over-cap lines encountered so the agent can detect partial scans (Tim's
/// review of PR #922).
struct FileScan {
    matched: bool,
    truncated_lines: u64,
}

/// Result of a per-line count scan.  Carries the match count plus a count of
/// over-cap lines encountered (Tim's review of PR #922).
struct CountScan {
    count: usize,
    truncated_lines: u64,
}

/// Check whether a file contains at least one regex match (line-by-line).
///
/// Uses [`read_line_capped`] to bound per-line allocation at 256 KiB (#913).
/// `BufReader::lines()` allocates per line without any cap, so a pathological
/// multi-GB single-line file (a minified bundle, a packed binary masquerading
/// as text, etc.) would otherwise pull the entire file into one `String` and
/// OOM the daemon.  The cap means the regex scan still runs against the
/// truncated prefix; if the pattern matches inside that prefix we report the
/// match honestly; if it lives past the cap we under-report this file.  That
/// is strictly better than crashing the process, and consistent with the
/// 1 MiB whole-file cap already applied to `search_file_content`.
///
/// The regex is evaluated against the captured-from-file slice
/// (`&line[..captured_byte_len]`) — never against the inline truncation
/// marker `read_line_capped` appends.  The marker text contains words like
/// `truncated` and `bytes` and is itself regex-matchable; including it
/// would cause user patterns like `truncated` to spuriously match the
/// marker rather than real content (Tim's review of PR #922).
async fn file_matches_regex(re: &Regex, path: &Path) -> SandboxResult<FileScan> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
            return Ok(FileScan {
                matched: false,
                truncated_lines: 0,
            });
        }
    };

    let mut reader = BufReader::new(file);
    let mut truncated_lines: u64 = 0;

    loop {
        match read_line_capped(&mut reader).await {
            Ok(Some(LineRead {
                line,
                truncated,
                captured_byte_len,
            })) => {
                if truncated {
                    truncated_lines = truncated_lines.saturating_add(1);
                }
                // Match against captured-from-file bytes only — the marker
                // (when present) is itself regex-matchable and would cause
                // spurious hits on patterns like `truncated`.
                let haystack = &line[..captured_byte_len];
                if re.is_match(haystack) {
                    return Ok(FileScan {
                        matched: true,
                        truncated_lines,
                    });
                }
            }
            Ok(None) => {
                return Ok(FileScan {
                    matched: false,
                    truncated_lines,
                });
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
                return Ok(FileScan {
                    matched: false,
                    truncated_lines,
                });
            }
        }
    }
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
///
/// Uses [`read_line_capped`] to bound per-line allocation at 256 KiB (#913).
/// See [`file_matches_regex`] for the rationale; the same trade-off applies,
/// including the captured-bytes-only matching to avoid false hits on the
/// truncation marker text.
async fn count_file_matches(re: &Regex, path: &Path) -> SandboxResult<CountScan> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
            return Ok(CountScan {
                count: 0,
                truncated_lines: 0,
            });
        }
    };

    let mut reader = BufReader::new(file);
    let mut count: usize = 0;
    let mut truncated_lines: u64 = 0;

    loop {
        match read_line_capped(&mut reader).await {
            Ok(Some(LineRead {
                line,
                truncated,
                captured_byte_len,
            })) => {
                if truncated {
                    truncated_lines = truncated_lines.saturating_add(1);
                }
                let haystack = &line[..captured_byte_len];
                if re.is_match(haystack) {
                    count += 1;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "Skipping unreadable file");
                return Ok(CountScan {
                    count,
                    truncated_lines,
                });
            }
        }
    }

    Ok(CountScan {
        count,
        truncated_lines,
    })
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

    // ── Per-line byte cap (#913, mirrors fs_read #902) ────────────────────

    /// Regression guard for #913: a 1 MiB single-line file (no newlines)
    /// exercises the per-line cap in `file_matches_regex` and
    /// `count_file_matches`.  Pre-#913 the underlying `BufReader::lines()`
    /// had no per-line cap, so this call would buffer the entire line as a
    /// single allocation; with multi-GB inputs that scaled with file size.
    /// Post-#913 each line is bounded by `MAX_LINE_BYTES` (256 KiB).  The
    /// pattern matches a substring well within the cap, so the truncated
    /// prefix is still searched honestly.  At 1 MiB this test allocates
    /// ~1 MiB worst-case pre-fix and ~256 KiB post-fix — it pins the
    /// line-cap code path, not OOM behaviour per se.
    #[tokio::test]
    async fn test_fs_grep_per_line_cap_huge_single_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("megaline.txt");
        // 1 MiB of `x`s with no newlines — exceeds MAX_LINE_BYTES (256 KiB)
        // by ~4x, enough to exercise the cap-and-drain path without
        // requiring a pathologically large fixture.  Embed a needle near
        // the start so the regex hits inside the truncated prefix.
        let mut content = String::from("NEEDLE");
        content.push_str(&"x".repeat(1024 * 1024));
        std::fs::write(&path, &content).unwrap();

        // files_with_matches mode (uses `file_matches_regex`).
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE",
                "path": path.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "should find one matching file");

        // count mode (uses `count_file_matches`).
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "should find one matching file");
        assert_eq!(matches[0]["count"], 1);
    }

    /// Multi-line files with all lines under the cap must search normally.
    /// Regression guard against the per-line cap accidentally firing on
    /// reasonable inputs and skewing match counts.
    #[tokio::test]
    async fn test_fs_grep_per_line_cap_normal_multiline_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("normal.txt");
        // 500 lines, ~20 bytes each — well under MAX_LINE_BYTES.  Half
        // contain the pattern.
        let content: String = (1..=500)
            .map(|i| {
                if i % 2 == 0 {
                    format!("line MATCH {i}\n")
                } else {
                    format!("line plain {i}\n")
                }
            })
            .collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "MATCH",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["count"], 250);
        assert_eq!(result["total_matches"], 250);
    }

    /// A file containing one over-cap line followed by normal short lines
    /// must continue scanning past the truncated line.  Specifically guards
    /// the `drain_to_newline` path inside `read_line_capped` when reused by
    /// fs_grep — the next read must position at the start of the line
    /// after the over-cap one.
    #[tokio::test]
    async fn test_fs_grep_per_line_cap_truncates_then_continues() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.txt");
        // First line: 600 KiB of `x` (well over the 256 KiB cap), no
        // pattern match within it.  Subsequent lines: short normal text
        // containing the needle.
        let mut content = "x".repeat(600 * 1024);
        content.push('\n');
        content.push_str("hit_line_one MATCH\n");
        content.push_str("hit_line_two MATCH\n");
        std::fs::write(&path, &content).unwrap();

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "MATCH",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "should find one matching file");
        // Two matches: both of the short lines following the truncated
        // one.  Pre-#913 the loop would buffer this file's first "line"
        // as a 600 KiB allocation; the cap-and-drain path post-#913
        // discards the surplus without growing the buffer past
        // `MAX_LINE_BYTES` and continues to the next newline.
        assert_eq!(
            matches[0]["count"], 2,
            "both follow-on lines must be matched after the truncated line"
        );
    }

    // ── Tim review of PR #922 follow-ups ──────────────────────────────────

    /// The truncation marker `[line truncated to N bytes; M bytes
    /// discarded]` that `read_line_capped` appends to over-cap lines is
    /// itself regex-matchable.  A user pattern like `truncated` or
    /// `bytes` would spuriously hit the marker rather than real file
    /// content.  `file_matches_regex` and `count_file_matches` must
    /// evaluate the regex against the captured-from-file slice only.
    ///
    /// Setup: a file whose only content is one over-cap line of `x`s —
    /// the captured prefix contains no `truncated` and no `bytes`, but
    /// the marker appended by `read_line_capped` does.  Pre-fix the
    /// pattern would match the marker; post-fix it must not.
    #[tokio::test]
    async fn test_fs_grep_marker_text_is_not_matched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker_only.txt");
        // 600 KiB of `x` — well over the 256 KiB cap.  Captured prefix
        // is just `x`s; the marker `[line truncated to ... bytes ...]`
        // contains the words `truncated` and `bytes`.
        let content = "x".repeat(600 * 1024);
        std::fs::write(&path, &content).unwrap();

        // files_with_matches: searching for `truncated` in a file whose
        // real content is only `x`s must return no matches.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "truncated",
                "path": path.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            0,
            "marker text 'truncated' must not be regex-matched as file content"
        );

        // count mode: same guarantee for `bytes`.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "bytes",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert!(
            matches.is_empty(),
            "marker text 'bytes' must not be regex-matched as file content"
        );
        assert_eq!(result["total_matches"], 0);
    }

    /// Pin the drop-bytes-past-cap semantic for the simplest case: a
    /// needle that lives *fully* past `MAX_LINE_BYTES` (i.e. starts at
    /// the cap boundary, no part of it inside the captured prefix) must
    /// not match.  The `truncated_lines` counter must report the over-
    /// cap line so the agent can detect the partial scan.
    ///
    /// Companion to `test_fs_grep_match_truly_spans_cap_boundary`, which
    /// covers the harder span-the-boundary case.
    ///
    /// This locks in the current behaviour so future refactors (e.g.
    /// raising the cap, streaming the regex over the full line) don't
    /// silently regress it.
    #[tokio::test]
    async fn test_fs_grep_match_fully_past_cap_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary.txt");
        // Build a single line: 256 KiB of `a` (exactly fills the cap),
        // followed by `NEEDLE`, followed by some trailing bytes, no `\n`.
        // The `NEEDLE` lives strictly past the cap and must be dropped
        // before regex evaluation.  Upper bound on captured prefix is
        // 256 KiB of `a` — the `NEEDLE` substring cannot appear in it.
        let mut content = "a".repeat(256 * 1024);
        content.push_str("NEEDLE");
        content.push_str(&"b".repeat(1024));
        std::fs::write(&path, &content).unwrap();

        // files_with_matches must say no.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE",
                "path": path.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            0,
            "needle past the cap must not be matched (drop-bytes-past-cap semantic)"
        );
        // The file *was* over-cap, so `truncated_lines` must be non-zero.
        assert!(
            result["truncated_lines"].as_u64().unwrap() >= 1,
            "over-cap line should be reported in truncated_lines: {:?}",
            result["truncated_lines"]
        );

        // count mode must return zero.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        assert_eq!(result["total_matches"], 0);
        assert!(result["matches"].as_array().unwrap().is_empty());
        assert!(
            result["truncated_lines"].as_u64().unwrap() >= 1,
            "over-cap line should be reported in truncated_lines"
        );
    }

    /// Genuine "match spans the cap boundary" case: the literal that the
    /// regex matches starts *inside* the captured prefix and extends
    /// *past* it.  After `read_line_capped` truncates at byte
    /// `MAX_LINE_BYTES` (256 KiB), the post-cap tail is drained and
    /// discarded; the regex is then evaluated against the captured
    /// prefix only.  The match would only succeed if the post-cap bytes
    /// were still present, so it must fail under the current
    /// drop-bytes-past-cap semantic.
    ///
    /// Setup: a single line laid out as
    ///   `"a" * 250_000`
    ///   `+ "NEEDLE_PREFIX_"`           (starts at byte 250_000)
    ///   `+ "z" * 20_000`
    ///   `+ "_NEEDLE_SUFFIX"`           (ends at byte ~270_014)
    ///   `+ "b" * 1024`
    /// MAX_LINE_BYTES = 262_144, so the prefix marker `NEEDLE_PREFIX_`
    /// lives entirely inside the captured prefix; `_NEEDLE_SUFFIX` lives
    /// entirely past it.  The pattern `NEEDLE_PREFIX_z+_NEEDLE_SUFFIX`
    /// requires both anchors, so it can only match if the full literal
    /// (markers + ~20 000 `z`s) is present in the haystack.  Post-#913
    /// the haystack is the captured prefix, which is missing
    /// `_NEEDLE_SUFFIX`, so `is_match` must be `false` and `count` must
    /// be `0`.  Companion test to
    /// `test_fs_grep_match_fully_past_cap_is_dropped`.
    #[tokio::test]
    async fn test_fs_grep_match_truly_spans_cap_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spanning.txt");
        let mut content = "a".repeat(250_000);
        content.push_str("NEEDLE_PREFIX_");
        content.push_str(&"z".repeat(20_000));
        content.push_str("_NEEDLE_SUFFIX");
        content.push_str(&"b".repeat(1024));
        std::fs::write(&path, &content).unwrap();

        // Sanity-check the layout: the prefix marker starts before the
        // cap and the suffix marker starts after it.  If MAX_LINE_BYTES
        // ever changes, this assertion will catch the test going stale.
        let prefix_start = 250_000usize;
        let suffix_start = 250_000 + "NEEDLE_PREFIX_".len() + 20_000;
        assert!(
            prefix_start < 262_144,
            "prefix marker must start inside the captured prefix"
        );
        assert!(
            suffix_start > 262_144,
            "suffix marker must start past the cap"
        );

        // files_with_matches: regex requires both markers + the run of
        // `z`s between them.  Without the post-cap bytes, no match.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE_PREFIX_z+_NEEDLE_SUFFIX",
                "path": path.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert!(
            matches.is_empty(),
            "match spanning the cap boundary must not succeed: {:?}",
            matches
        );
        assert!(
            result["truncated_lines"].as_u64().unwrap() >= 1,
            "spanning over-cap line should be reported in truncated_lines"
        );

        // count mode: same guarantee.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE_PREFIX_z+_NEEDLE_SUFFIX",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        assert_eq!(
            result["total_matches"], 0,
            "spanning match must yield zero count"
        );
        assert!(
            result["matches"].as_array().unwrap().is_empty(),
            "no matching files in count mode either"
        );
        assert!(
            result["truncated_lines"].as_u64().unwrap() >= 1,
            "spanning over-cap line should be reported in truncated_lines"
        );

        // Sanity guard: the *prefix-only* sub-pattern must still match
        // — it lives entirely inside the captured prefix.  This proves
        // the test isn't trivially failing because the file is somehow
        // unreadable; it's specifically failing because the suffix
        // tail was dropped.
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "NEEDLE_PREFIX_",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(
            matches.len(),
            1,
            "the prefix marker is inside the captured prefix and must match"
        );
        assert_eq!(matches[0]["count"], 1);
    }

    /// `truncated_lines` is non-zero when over-cap lines are encountered.
    /// Exercises both files_with_matches and count modes.
    #[tokio::test]
    async fn test_fs_grep_truncated_lines_field_nonzero_on_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two_over_cap.txt");
        // Two over-cap lines (300 KiB each, > 256 KiB cap), separated by
        // `\n`.  Neither contains the pattern in its captured prefix, but
        // the agent should still see that two lines were truncated so it
        // knows the scan was partial.
        let mut content = "z".repeat(300 * 1024);
        content.push('\n');
        content.push_str(&"z".repeat(300 * 1024));
        content.push('\n');
        std::fs::write(&path, &content).unwrap();

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "WILL_NOT_MATCH",
                "path": path.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        assert_eq!(
            result["truncated_lines"].as_u64().unwrap(),
            2,
            "both over-cap lines must be counted in truncated_lines"
        );

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "WILL_NOT_MATCH",
                "path": path.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        assert_eq!(
            result["truncated_lines"].as_u64().unwrap(),
            2,
            "count mode must surface truncated_lines too"
        );
    }

    /// `truncated_lines` is zero when no lines exceed the cap — the
    /// scan was complete, the agent can trust the result fully.
    #[tokio::test]
    async fn test_fs_grep_truncated_lines_field_zero_on_normal_input() {
        let dir = setup_grep_dir();
        let root = dir.path();

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();
        assert_eq!(
            result["truncated_lines"].as_u64().unwrap(),
            0,
            "normal-sized fixtures must have truncated_lines == 0"
        );

        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();
        assert_eq!(result["truncated_lines"].as_u64().unwrap(), 0);

        // Content mode also emits the field, always 0 (it uses the
        // whole-file gate, not the per-line cap).
        let result = FsGrepTool::new()
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();
        assert_eq!(result["truncated_lines"].as_u64().unwrap(), 0);
    }

    /// Regression for #972: when `path` points at a single file, `content`
    /// mode must populate the per-match `file` field with the file's basename
    /// (matching the directory-walk shape). Previously it returned `""`
    /// because `relativize(file, search_root)` strips a self-prefix to empty.
    #[tokio::test]
    async fn test_fs_grep_single_file_path_populates_file_field_content_mode() {
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
        let file_field = matches[0]["file"].as_str().unwrap();
        assert!(
            !file_field.is_empty(),
            "single-file mode must not return empty file field (#972)"
        );
        assert_eq!(
            file_field, "main.rs",
            "single-file mode emits the basename, matching directory-walk shape"
        );
    }

    /// Regression for #972: same fix must apply to `count` mode.
    #[tokio::test]
    async fn test_fs_grep_single_file_path_populates_file_field_count_mode() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/main.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": file.to_str().unwrap(),
                "output_mode": "count"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let file_field = matches[0]["file"].as_str().unwrap();
        assert_eq!(file_field, "main.rs");
        assert_eq!(matches[0]["count"].as_u64().unwrap(), 1);
    }

    /// Regression for #972: same fix must apply to `files_with_matches` mode.
    /// In that mode `matches` is a flat string array (not objects), so we
    /// assert directly on the string value.
    #[tokio::test]
    async fn test_fs_grep_single_file_path_populates_file_field_fwm_mode() {
        let dir = setup_grep_dir();
        let file = dir.path().join("src/main.rs");
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": file.to_str().unwrap(),
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let entry = matches[0].as_str().unwrap();
        assert!(
            !entry.is_empty(),
            "single-file mode must not return empty path (#972)"
        );
        assert_eq!(entry, "main.rs");
    }

    /// Sanity: directory-mode behaviour is unchanged — paths are still the
    /// relative path under the search root, not just the basename.
    #[tokio::test]
    async fn test_fs_grep_directory_mode_file_field_unchanged() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        let result = tool
            .execute(serde_json::json!({
                "pattern": "println",
                "path": root.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        let files: Vec<&str> = matches
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        // Directory walk emits paths relative to `search_root`, e.g.
        // "src/main.rs" — never just the basename, never empty.
        assert!(files.contains(&"src/main.rs"));
        assert!(files.iter().all(|f| !f.is_empty()));
    }
}
