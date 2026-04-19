use crate::error::SandboxResult;
use crate::file_state_cache::{FileStateCache, content_hash};
use crate::{SandboxError, Tool};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{
    check_sandbox_path_async, check_sandbox_path_with_extras_async, is_blocked_device_path,
    normalize_unsandboxed_path, reject_unc_path,
};

/// Read a file from the filesystem.
#[derive(Debug, Clone, Default)]
pub struct FsReadTool {
    sandbox_root: Option<PathBuf>,
    file_state_cache: Option<Arc<FileStateCache>>,
    /// Additional read-only roots that extend the set of paths allowed by
    /// this tool (see #242 — parent agents reading subagent workspaces).
    /// Writes continue to be gated by the primary sandbox root only.
    extra_read_roots: Vec<PathBuf>,
}

impl FsReadTool {
    /// Create an unrestricted fs_read tool (no sandbox check).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed fs_read tool. Paths must resolve within `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
            file_state_cache: None,
            extra_read_roots: Vec::new(),
        }
    }

    /// Attach a file state cache for read-before-write tracking.
    pub fn with_cache(mut self, cache: Arc<FileStateCache>) -> Self {
        self.file_state_cache = Some(cache);
        self
    }

    /// Attach additional read-only roots.  Absolute paths that resolve inside
    /// any of these roots will be allowed in addition to the primary sandbox
    /// root.  Relative paths still resolve against the primary root.
    pub fn with_extra_read_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.extra_read_roots = roots;
        self
    }
}

#[async_trait::async_trait]
impl Tool for FsReadTool {
    fn name(&self) -> &str {
        "fs_read"
    }

    fn description(&self) -> &str {
        "Read a file with line numbers (cat -n format). Supports partial reads via offset/limit. \
         Returns content with 1-based line numbers, has_more_before/has_more_after indicators, \
         and total_lines when known (file read to EOF). Max file size: 256 KB."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based line index to start reading from. Default: 0 (beginning of file)."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of lines to return. Default: 2000."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        /// Maximum file size we will attempt to read (256 KiB).
        const MAX_READ_BYTES: u64 = 256 * 1024;
        /// Maximum accumulated bytes in the formatted output (512 KiB).
        const MAX_OUTPUT_BYTES: usize = 512 * 1024;

        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'path' is required".to_string()))?;

        let offset = match params.get("offset") {
            Some(v) => v.as_u64().ok_or_else(|| {
                SandboxError::InvalidParameters("'offset' must be a non-negative integer".into())
            })? as usize,
            None => 0,
        };

        let limit = match params.get("limit") {
            Some(v) => {
                let l = v.as_u64().ok_or_else(|| {
                    SandboxError::InvalidParameters("'limit' must be a positive integer".into())
                })? as usize;
                if l == 0 {
                    return Err(SandboxError::InvalidParameters(
                        "'limit' must be greater than 0".into(),
                    ));
                }
                l
            }
            None => 2000,
        };

        // Block UNC paths that could leak NTLM credentials via SMB.
        reject_unc_path(path)?;

        // Block device paths that would hang or produce infinite output.
        if is_blocked_device_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot read device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            if self.extra_read_roots.is_empty() {
                check_sandbox_path_async(path, root).await?
            } else {
                check_sandbox_path_with_extras_async(path, root, &self.extra_read_roots).await?
            }
        } else {
            normalize_unsandboxed_path(path).await
        };

        // Also check the resolved path (handles relative traversals).
        if is_blocked_device_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot read device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        // Retrieve metadata and verify the path is a regular file.
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;

        if !meta.is_file() {
            return Err(SandboxError::InvalidParameters(format!(
                "Path '{}' is not a regular file",
                path
            )));
        }

        // File size guard — reject files larger than 256 KiB.
        if meta.len() > MAX_READ_BYTES {
            return Err(SandboxError::InvalidParameters(format!(
                "File '{}' is {} bytes, which exceeds the maximum read size of 256 KB. \
                 Use offset and limit parameters to read a portion of the file.",
                path,
                meta.len()
            )));
        }

        // Open file and read lines using BufReader for memory efficiency.
        let file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;
        let reader = BufReader::new(file);
        let mut lines_stream = reader.lines();

        let mut lines_read: usize = 0;
        let mut selected_lines: Vec<(usize, String)> = Vec::new();
        let end = offset.saturating_add(limit);
        let mut output_bytes: usize = 0;
        let mut byte_budget_exceeded = false;
        let mut read_past_end = false;

        // Phase 1: skip lines before offset, collect lines in [offset, end), stop
        // collecting once we've passed the requested range.
        while let Some(line) = lines_stream
            .next_line()
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?
        {
            let line_idx = lines_read; // 0-based index
            lines_read += 1;

            if line_idx >= offset && line_idx < end {
                // Byte budget: estimate formatted line size (6-digit number + tab + content + newline).
                let line_cost = 7 + line.len() + 1;
                if output_bytes.saturating_add(line_cost) > MAX_OUTPUT_BYTES {
                    byte_budget_exceeded = true;
                    break;
                }
                output_bytes += line_cost;
                selected_lines.push((line_idx + 1, line)); // 1-based line number
            }

            // Once we've passed the requested range, stop storing lines.
            // The line at `end` was consumed but not collected, so we already
            // know there is at least one unreturned line after the range.
            if line_idx >= end {
                read_past_end = true;
                break;
            }
        }

        // If we broke out early (due to limit or byte budget), we may not know total_lines.
        // Efficiently count remaining lines without storing them.
        // Skip the peek when `read_past_end` is set — the loop already consumed a
        // line beyond the requested range, proving more content exists.
        let hit_eof = !byte_budget_exceeded && !read_past_end && {
            // If we naturally exhausted the range without reading past it
            // (i.e. file had exactly `offset + limit` lines), peek one more
            // line to see whether the file continues.
            let peeked = lines_stream
                .next_line()
                .await
                .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;
            if peeked.is_some() {
                lines_read += 1;
                // There are more lines — don't count them all, just note we didn't hit EOF.
                false
            } else {
                true
            }
        };

        // Handle empty file.
        if lines_read == 0 && selected_lines.is_empty() {
            // Populate file state cache for empty files too.
            if let Some(ref cache) = self.file_state_cache {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                cache.record_read(resolved, content_hash(b""), mtime, false);
            }
            return Ok(serde_json::json!({
                "content": "",
                "lines_returned": 0,
                "has_more_before": false,
                "has_more_after": false,
                "note": "File exists but is empty."
            }));
        }

        // Format output in cat -n style: right-justified 6-char line number + tab.
        let content = selected_lines
            .iter()
            .map(|(num, line)| format!("{num:>6}\t{line}"))
            .collect::<Vec<_>>()
            .join("\n");

        let lines_returned = selected_lines.len();
        let has_more_before = offset > 0;
        let has_more_after = !hit_eof || byte_budget_exceeded;

        let mut result = serde_json::json!({
            "content": content,
            "lines_returned": lines_returned,
            "has_more_before": has_more_before,
            "has_more_after": has_more_after,
        });

        // Only include total_lines when we actually read to EOF (no extra pass needed).
        if hit_eof && !byte_budget_exceeded {
            result["total_lines"] = serde_json::json!(lines_read);
        }

        if has_more_before || has_more_after {
            result["offset"] = serde_json::json!(offset);
            result["limit"] = serde_json::json!(limit);
        }

        if byte_budget_exceeded {
            result["byte_budget_exceeded"] = serde_json::json!(true);
            result["note"] = serde_json::json!(format!(
                "Output truncated at {} bytes (max {}). Use smaller limit or narrower offset.",
                output_bytes, MAX_OUTPUT_BYTES
            ));
        }

        // Populate the file state cache so subsequent fs_write/fs_edit calls
        // can verify the file was read first.
        if let Some(ref cache) = self.file_state_cache {
            // Re-read raw bytes for hashing. The file is already verified to
            // be <= 256KB so this is cheap.
            if let Ok(raw_bytes) = tokio::fs::read(&resolved).await {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let is_partial = offset > 0 || has_more_after;
                cache.record_read(resolved, content_hash(&raw_bytes), mtime, is_partial);
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::FsWriteTool;

    #[tokio::test]
    async fn test_fs_read_sandbox_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsReadTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({"path": "../../../etc/passwd"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("outside sandbox"));
    }

    #[tokio::test]
    async fn test_fs_read_missing_path() {
        let result = FsReadTool::new().execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_read_nonexistent_file() {
        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": "nonexistent_file_xyz.txt"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_read_small_file_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        let content = result["content"].as_str().unwrap();
        assert!(content.contains("     1\talpha"));
        assert!(content.contains("     2\tbeta"));
        assert!(content.contains("     3\tgamma"));
        assert_eq!(result["total_lines"], 3);
        assert_eq!(result["lines_returned"], 3);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
        assert!(result.get("offset").is_none());
        assert!(result.get("limit").is_none());
    }

    #[tokio::test]
    async fn test_fs_read_large_file_default_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let content: String = (1..=2500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(result.get("total_lines").is_none());
        assert_eq!(result["lines_returned"], 2000);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], true);
        assert_eq!(result["offset"], 0);
        assert_eq!(result["limit"], 2000);
        let text = result["content"].as_str().unwrap();
        assert!(text.starts_with("     1\tline 1\n"));
    }

    #[tokio::test]
    async fn test_fs_read_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("numbered.txt");
        let content: String = (1..=100).map(|i| format!("line-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 50,
                "limit": 10
            }))
            .await
            .unwrap();

        let text = result["content"].as_str().unwrap();
        assert!(text.contains("    51\tline-51"));
        assert!(text.contains("    60\tline-60"));
        assert!(!text.contains("    50\t"));
        assert!(!text.contains("    61\t"));
        assert_eq!(result["lines_returned"], 10);
        assert_eq!(result["has_more_before"], true);
        assert_eq!(result["has_more_after"], true);
    }

    #[tokio::test]
    async fn test_fs_read_offset_beyond_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 9999
            }))
            .await
            .unwrap();

        assert_eq!(result["content"], "");
        assert_eq!(result["total_lines"], 3);
        assert_eq!(result["lines_returned"], 0);
        assert_eq!(result["has_more_before"], true);
        assert_eq!(result["has_more_after"], false);
    }

    #[tokio::test]
    async fn test_fs_read_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        assert_eq!(result["content"], "");
        assert_eq!(result["lines_returned"], 0);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
        assert_eq!(result["note"], "File exists but is empty.");
    }

    #[tokio::test]
    async fn test_fs_read_only_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limited.txt");
        let content: String = (1..=50).map(|i| format!("row-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": 5
            }))
            .await
            .unwrap();

        assert_eq!(result["lines_returned"], 5);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], true);
        let text = result["content"].as_str().unwrap();
        assert!(text.contains("     1\trow-1"));
        assert!(text.contains("     5\trow-5"));
        assert!(!text.contains("     6\t"));
    }

    #[tokio::test]
    async fn test_fs_read_only_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offset_only.txt");
        let content: String = (1..=10).map(|i| format!("item-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 5
            }))
            .await
            .unwrap();

        let text = result["content"].as_str().unwrap();
        assert!(text.contains("     6\titem-6"));
        assert!(text.contains("    10\titem-10"));
        assert!(!text.contains("     5\titem-5"));
        assert_eq!(result["lines_returned"], 5);
        assert_eq!(result["has_more_before"], true);
        assert_eq!(result["total_lines"], 10);
        assert_eq!(result["has_more_after"], false);
    }

    #[tokio::test]
    async fn test_fs_read_line_number_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fmt.txt");
        std::fs::write(&path, "a\nb\nc").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        let content = result["content"].as_str().unwrap();
        let lines: Vec<&str> = content.split('\n').collect();
        assert_eq!(lines[0], "     1\ta");
        assert_eq!(lines[1], "     2\tb");
        assert_eq!(lines[2], "     3\tc");
    }

    #[tokio::test]
    async fn test_fs_read_invalid_limit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "content").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": 0
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("greater than 0"));
    }

    #[tokio::test]
    async fn test_fs_read_invalid_offset_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "content").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": "abc"
            }))
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("non-negative integer")
        );
    }

    #[tokio::test]
    async fn test_fs_read_invalid_limit_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "content").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": "abc"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("positive integer"));
    }

    #[tokio::test]
    async fn test_fs_read_file_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.txt");
        let content = "x".repeat(256 * 1024 + 1);
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds the maximum read size"),
            "got: {err_msg}"
        );
        assert!(
            err_msg.contains("offset and limit"),
            "error should suggest offset/limit, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_read_byte_budget_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many_lines.txt");
        let content: String = (0..60_000).map(|_| "x\n").collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": 60000
            }))
            .await
            .unwrap();

        let lines_returned = result["lines_returned"].as_u64().unwrap();
        assert!(
            lines_returned < 60_000,
            "expected fewer than 60000 lines due to byte budget, got {lines_returned}"
        );
        assert_eq!(result["has_more_after"], true);
        assert_eq!(result["byte_budget_exceeded"], true);
        assert!(result["note"].as_str().unwrap().contains("truncated"));
    }

    #[tokio::test]
    async fn test_fs_read_total_lines_present_when_eof() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("complete.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        assert_eq!(result["total_lines"], 5);
        assert_eq!(result["lines_returned"], 5);
    }

    #[tokio::test]
    async fn test_fs_read_total_lines_absent_when_stopped_early() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.txt");
        let content: String = (1..=100).map(|i| format!("line-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": 10
            }))
            .await
            .unwrap();

        assert!(
            result.get("total_lines").is_none(),
            "total_lines should not be present when stopped early"
        );
        assert_eq!(result["lines_returned"], 10);
    }

    #[tokio::test]
    async fn test_fs_read_has_more_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("middle.txt");
        let content: String = (1..=20).map(|i| format!("line-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "offset": 5,
                "limit": 5
            }))
            .await
            .unwrap();

        assert_eq!(result["has_more_before"], true);
        assert_eq!(result["has_more_after"], true);
        assert_eq!(result["lines_returned"], 5);
        let text = result["content"].as_str().unwrap();
        assert!(text.contains("     6\tline-6"));
        assert!(text.contains("    10\tline-10"));
    }

    #[tokio::test]
    async fn test_fs_read_rejects_unc_path() {
        let tool = FsReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "\\\\server\\share\\file.txt"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNC paths"), "error should mention UNC: {err}");
        assert!(err.contains("SMB"), "error should mention SMB: {err}");

        let result = tool
            .execute(serde_json::json!({"path": "//server/share/file.txt"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UNC paths"));
    }

    #[tokio::test]
    async fn test_fs_read_blocked_device_path() {
        let device = if cfg!(unix) { "/dev/zero" } else { "NUL" };
        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": device}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cannot read device path"),
            "expected device path error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("system device"),
            "expected 'system device' in error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_read_directory_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": dir.path().to_str().unwrap()}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not a regular file"),
            "expected 'not a regular file', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_read_size_error_includes_actual_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let size = 256 * 1024 + 100;
        let content = "x".repeat(size);
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains(&size.to_string()),
            "error should contain actual file size {size}, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_read_under_limit_reads_normally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "hello\nworld\n").unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert_eq!(result["lines_returned"], 2);
        let text = result["content"].as_str().unwrap();
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
    }

    /// Regression guard for #744: the hardcoded denied-filename list
    /// (a one-entry `["secrets.json"]`) was removed from `fs_read`.
    /// Reading a file whose basename is `secrets.json` must no longer
    /// be rejected for that reason alone — file access is now governed
    /// only by the sandbox root (and, on Linux 5.13+, by Landlock).
    #[tokio::test]
    async fn test_fs_read_no_hardcoded_secrets_json_deny() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");
        std::fs::write(&secrets, r#"{"key": "sk-1234"}"#).unwrap();

        // Covers the three access patterns exercised by the removed
        // tests: absolute path, relative path, dot-slash, and a
        // relative parent-traversal that still resolves inside root.
        let tool_abs = FsReadTool::new();
        let result = tool_abs
            .execute(serde_json::json!({"path": secrets.to_str().unwrap()}))
            .await;
        assert!(result.is_ok(), "abs path: got {:?}", result.err());

        let tool_rel = FsReadTool::sandboxed(dir.path().to_path_buf());
        for rel in ["./secrets.json", "data/../secrets.json"] {
            let result = tool_rel.execute(serde_json::json!({"path": rel})).await;
            assert!(
                result.is_ok(),
                "relative path '{rel}' should no longer be blocked by the hardcoded deny list: {:?}",
                result.err()
            );
        }
    }

    #[tokio::test]
    async fn test_fs_read_has_more_after_exactly_one_past_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.txt");
        let path_str = path.to_str().unwrap();

        let content: String = (1..=6).map(|i| format!("line-{i}\n")).collect();
        FsWriteTool::new()
            .execute(serde_json::json!({"path": path_str, "content": content}))
            .await
            .unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path_str, "offset": 0, "limit": 5}))
            .await
            .unwrap();

        assert_eq!(result["lines_returned"], 5);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(
            result["has_more_after"], true,
            "file has 6 lines but only 5 returned — has_more_after must be true"
        );
        let text = result["content"].as_str().unwrap();
        assert!(text.contains("     1\tline-1"));
        assert!(text.contains("     5\tline-5"));
        assert!(
            !text.contains("line-6"),
            "line-6 should not appear in output"
        );
        assert!(
            result.get("total_lines").is_none(),
            "total_lines should be absent when not at EOF"
        );
    }

    // ── extra_read_roots (sibling workspace access, #242) ─────────────────

    #[tokio::test]
    async fn test_fs_read_extra_root_allows_sibling_workspace() {
        // Simulate layout: {workspace_dir}/parent/   and {workspace_dir}/child/
        // Parent agent's fs_read is sandboxed to parent/ but granted extra read
        // on {workspace_dir}, which lets it read child/memories.md.
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        let child_ws = workspace_dir.join("child");
        std::fs::create_dir_all(&parent_ws).unwrap();
        std::fs::create_dir_all(&child_ws).unwrap();
        let memories = child_ws.join("memories.md");
        std::fs::write(&memories, "alpha\nbeta\n").unwrap();

        let tool =
            FsReadTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir.clone()]);
        let result = tool
            .execute(serde_json::json!({"path": memories.to_str().unwrap()}))
            .await
            .unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
    }

    #[tokio::test]
    async fn test_fs_read_extra_root_does_not_widen_to_project() {
        // Extra read roots must not allow arbitrary absolute paths outside
        // both the primary sandbox root AND the extras.
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        std::fs::create_dir_all(&parent_ws).unwrap();

        // Create an "outside" file in a completely different tempdir.
        let outside_dir = tempfile::tempdir().unwrap();
        let outside_file = outside_dir.path().join("leak.txt");
        std::fs::write(&outside_file, "secret").unwrap();

        let tool =
            FsReadTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir.clone()]);
        let result = tool
            .execute(serde_json::json!({"path": outside_file.to_str().unwrap()}))
            .await;
        assert!(result.is_err(), "outside path must still be rejected");
    }

    /// Regression guard for #744: after the hardcoded denied-filename
    /// list was removed, a file named `secrets.json` in a sibling
    /// workspace reachable via an extra read root must now be readable
    /// (nothing distinguishes its basename from any other file).
    #[tokio::test]
    async fn test_fs_read_extra_root_allows_any_basename() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = std::fs::canonicalize(dir.path()).unwrap();
        let parent_ws = workspace_dir.join("parent");
        let child_ws = workspace_dir.join("child");
        std::fs::create_dir_all(&parent_ws).unwrap();
        std::fs::create_dir_all(&child_ws).unwrap();
        let target = child_ws.join("secrets.json");
        std::fs::write(&target, r#"{"k": "v"}"#).unwrap();

        let tool =
            FsReadTool::sandboxed(parent_ws).with_extra_read_roots(vec![workspace_dir.clone()]);
        let result = tool
            .execute(serde_json::json!({"path": target.to_str().unwrap()}))
            .await;
        assert!(
            result.is_ok(),
            "with deny list removed, read through extra root should succeed: {:?}",
            result.err()
        );
    }
}
