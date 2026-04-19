use crate::error::SandboxResult;
use crate::file_state_cache::{
    FileStateCache, GuardOutcome, check_guard_with_mtime, update_cache_after_write,
};
use crate::{SandboxError, Tool};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    check_sandbox_path_async, is_blocked_device_path, normalize_unsandboxed_path, reject_unc_path,
};

/// Write (or append to) a file on the filesystem.
#[derive(Debug, Clone, Default)]
pub struct FsWriteTool {
    sandbox_root: Option<PathBuf>,
    file_state_cache: Option<Arc<FileStateCache>>,
}

impl FsWriteTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
            file_state_cache: None,
        }
    }

    /// Attach a file state cache for read-before-write tracking.
    pub fn with_cache(mut self, cache: Arc<FileStateCache>) -> Self {
        self.file_state_cache = Some(cache);
        self
    }
}

#[async_trait::async_trait]
impl Tool for FsWriteTool {
    fn name(&self) -> &str {
        "fs_write"
    }

    fn description(&self) -> &str {
        "Write or append text content to a file. Creates the file and parent directories if needed."
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
                    "description": "Path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "Text content to write."
                },
                "mode": {
                    "type": "string",
                    "enum": ["write", "append"],
                    "description": "Write mode: 'write' (overwrite, default) or 'append'."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'path' is required".to_string()))?;

        // Block UNC paths that could leak NTLM credentials via SMB.
        reject_unc_path(path)?;

        // Block device paths that could cause system damage.
        if is_blocked_device_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot write to device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            normalize_unsandboxed_path(path).await
        };

        // Also check the resolved path for device paths.
        if is_blocked_device_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot write to device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        // Non-regular-file and read-before-write guards.
        // New file creation (file does not exist) bypasses both checks.
        // We capture the mtime here to avoid a redundant stat in the guard.
        let file_mtime = match tokio::fs::metadata(&resolved).await {
            Ok(meta) => {
                if !meta.is_file() {
                    return Err(SandboxError::InvalidParameters(format!(
                        "Path '{}' is not a regular file",
                        path
                    )));
                }
                meta.modified().ok()
            }
            Err(_) => None,
        };

        // Read-before-write guard: if the file exists on disk and a cache is
        // present, verify the agent has read the file first.
        if let Some(ref cache) = self.file_state_cache
            && let Some(mtime) = file_mtime
        {
            match check_guard_with_mtime(cache, &resolved, mtime).await {
                GuardOutcome::Allowed => {}
                GuardOutcome::NotRead => {
                    return Err(SandboxError::InvalidParameters(
                        "File has not been read yet. Use fs_read to read the file \
                         before writing to it."
                            .to_string(),
                    ));
                }
                GuardOutcome::StaleRead { reason } => {
                    return Err(SandboxError::InvalidParameters(reason));
                }
            }
        }

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'content' is required".to_string()))?;

        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("write");

        // Create parent directories if needed.
        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                SandboxError::Io(format!("Failed to create dirs for '{}': {}", path, e))
            })?;
        }

        match mode {
            "write" => {
                tokio::fs::write(&resolved, content)
                    .await
                    .map_err(|e| SandboxError::Io(format!("Failed to write '{}': {}", path, e)))?;
            }
            "append" => {
                use tokio::io::AsyncWriteExt;
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved)
                    .await
                    .map_err(|e| SandboxError::Io(format!("Failed to open '{}': {}", path, e)))?;
                file.write_all(content.as_bytes()).await.map_err(|e| {
                    SandboxError::Io(format!("Failed to append to '{}': {}", path, e))
                })?;
            }
            other => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Invalid mode '{}': must be 'write' or 'append'",
                    other
                )));
            }
        }

        // Update the cache so subsequent writes/edits to the same file
        // pass the guard without requiring a re-read.
        if let Some(ref cache) = self.file_state_cache {
            update_cache_after_write(cache, &resolved).await;
        }

        Ok(serde_json::json!({ "ok": true, "path": path }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::FsReadTool;

    #[tokio::test]
    async fn test_fs_write_sandbox_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsWriteTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({"path": "../../evil.sh", "content": "rm -rf /"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("outside sandbox"));
    }

    #[tokio::test]
    async fn test_fs_write_invalid_mode() {
        let result = FsWriteTool::new()
            .execute(serde_json::json!({"path": "test.txt", "content": "hi", "mode": "replace"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid mode"));
    }

    #[tokio::test]
    async fn test_fs_write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let path_str = path.to_str().unwrap();

        let result = FsWriteTool::new()
            .execute(serde_json::json!({"path": path_str, "content": "world"}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path_str}))
            .await
            .unwrap();
        assert_eq!(result["content"], "     1\tworld");
        assert_eq!(result["total_lines"], 1);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
    }

    #[tokio::test]
    async fn test_fs_write_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("append.txt");
        let path_str = path.to_str().unwrap();

        FsWriteTool::new()
            .execute(serde_json::json!({"path": path_str, "content": "line1\n"}))
            .await
            .unwrap();
        FsWriteTool::new()
            .execute(serde_json::json!({"path": path_str, "content": "line2\n", "mode": "append"}))
            .await
            .unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path_str}))
            .await
            .unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("     1\tline1"));
        assert!(content.contains("     2\tline2"));
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
    }

    #[tokio::test]
    async fn test_fs_write_rejects_unc_path() {
        let tool = FsWriteTool::new();
        let result = tool
            .execute(serde_json::json!({"path": "\\\\server\\share\\file.txt", "content": "x"}))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNC paths"), "error should mention UNC: {err}");

        let result = tool
            .execute(serde_json::json!({"path": "//server/share/file.txt", "content": "x"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UNC paths"));
    }

    /// Regression guard for #744: the hardcoded denied-filename
    /// (`secrets.json`) check was removed from `fs_write`. It was a
    /// single-entry list that was not user-configurable and was
    /// trivially bypassable (symlinks, alternate paths, shell tool).
    /// `fs_write` must no longer reject writes to a file just because
    /// its basename is `secrets.json` — the write succeeds (or fails
    /// for some other legitimate reason like sandbox root rules).
    #[tokio::test]
    async fn test_fs_write_no_hardcoded_secrets_json_deny() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");

        let tool = FsWriteTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": secrets.to_str().unwrap(),
                "content": "not secret"
            }))
            .await;
        // Must succeed: the hardcoded denied-filename check is gone
        // and no sandbox root is configured in this test.
        assert!(
            result.is_ok(),
            "fs_write to a file named 'secrets.json' must no longer be \
             blocked by a hardcoded deny list (got: {:?})",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_fs_write_blocked_device_path() {
        let device = if cfg!(unix) { "/dev/zero" } else { "NUL" };
        let result = FsWriteTool::new()
            .execute(serde_json::json!({"path": device, "content": "data"}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cannot write to device path"),
            "expected device path error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("system device"),
            "expected 'system device' in error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_write_directory_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = FsWriteTool::new()
            .execute(serde_json::json!({"path": dir.path().to_str().unwrap(), "content": "data"}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not a regular file"),
            "expected 'not a regular file', got: {err_msg}"
        );
    }
}
