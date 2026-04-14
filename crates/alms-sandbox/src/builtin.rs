use crate::shell::security::DENIED_FILENAMES;
use crate::{SandboxError, Tool, error::SandboxResult};
use chrono::{Local, Utc};
use globset::GlobBuilder;
use regex::Regex;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use walkdir::WalkDir;

/// Check whether a resolved path references a denied filename.
///
/// Uses case-insensitive comparison so that `Secrets.JSON` and `SECRETS.json`
/// are caught on case-insensitive filesystems (Windows).
fn is_denied_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|name| {
            DENIED_FILENAMES
                .iter()
                .any(|d| d.eq_ignore_ascii_case(name))
        })
}

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
fn is_blocked_device_path(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let s = path.to_str().unwrap_or("");
        if BLOCKED_DEVICE_PATHS.iter().any(|&d| d == s) {
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

/// Resolve a path and verify it falls within the sandbox root.
///
/// Relative paths are joined to `sandbox_root`. Absolute paths are checked
/// directly. Symlinks are followed via `canonicalize()` to prevent escapes.
/// For non-existent paths (e.g. fs_write targets) the nearest existing
/// ancestor is canonicalized and the remaining components are appended.
/// Returns the resolved path on success so callers can use it for I/O
/// (avoids re-resolving relative paths against a different base).
fn check_sandbox_path(path: &str, sandbox_root: &Path) -> SandboxResult<PathBuf> {
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
async fn check_sandbox_path_async(path: &str, sandbox_root: &Path) -> SandboxResult<PathBuf> {
    let path_owned = path.to_owned();
    let root_owned = sandbox_root.to_owned();
    tokio::task::spawn_blocking(move || check_sandbox_path(&path_owned, &root_owned))
        .await
        .map_err(|e| {
            SandboxError::SandboxViolation(format!("Sandbox path check task failed: {}", e))
        })?
}

/// Canonicalize a path, walking up to the nearest existing ancestor if the
/// full path does not yet exist (handles fs_write for new files/dirs).
fn canonicalize_best_effort(path: &Path) -> std::io::Result<PathBuf> {
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

/// Truncate a string to at most `max_bytes` bytes, respecting UTF-8 char boundaries.
/// Appends a truncation note when the string is shortened.
///
/// Kept for tests only -- production `fs_read` now uses line-based reading.
#[cfg(test)]
fn safe_truncate(s: &str, max_bytes: usize) -> String {
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

/// Echo tool - returns the input unchanged
#[derive(Debug, Clone, Default)]
pub struct EchoTool;

impl EchoTool {
    /// Create a new echo tool
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Returns the input value unchanged. Useful for testing and debugging."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo back"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        // Extract the 'message' field if present, otherwise return entire params
        if let Some(msg) = params.get("message") {
            Ok(msg.clone())
        } else {
            Ok(params)
        }
    }
}

/// Datetime tool - returns the current date and time.
///
/// Agents use this to know what time it is. Returns ISO 8601 timestamp,
/// human-readable format, and UTC offset. Always auto-approved.
#[derive(Debug, Clone, Default)]
pub struct DatetimeTool;

impl DatetimeTool {
    /// Create a new datetime tool
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for DatetimeTool {
    fn name(&self) -> &str {
        "datetime"
    }

    fn description(&self) -> &str {
        "Returns the current date and time in both UTC and local device timezone. \
         Includes ISO 8601 format, human-readable format, timezone name, and UTC offset. \
         Use this whenever you need to know the current time."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(&self, _params: Value) -> SandboxResult<Value> {
        let utc_now = Utc::now();
        let local_now = Local::now();
        let utc_offset = local_now.format("%:z").to_string();
        let local_timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "Unknown".into());
        Ok(serde_json::json!({
            "iso": utc_now.to_rfc3339(),
            "human": utc_now.format("%A, %B %-d, %Y %-I:%M %p").to_string(),
            "timezone": "UTC",
            "local_iso": local_now.to_rfc3339(),
            "local_human": local_now.format("%A, %B %-d, %Y %-I:%M %p").to_string(),
            "local_timezone": local_timezone,
            "utc_offset": utc_offset,
        }))
    }
}

/// Math tool - performs mathematical operations
#[derive(Debug, Clone, Default)]
pub struct MathTool;

impl MathTool {
    /// Create a new math tool
    pub fn new() -> Self {
        Self
    }

    /// Perform addition
    fn add(&self, a: f64, b: f64) -> f64 {
        a + b
    }

    /// Perform subtraction
    fn subtract(&self, a: f64, b: f64) -> f64 {
        a - b
    }

    /// Perform multiplication
    fn multiply(&self, a: f64, b: f64) -> f64 {
        a * b
    }

    /// Perform division
    fn divide(&self, a: f64, b: f64) -> SandboxResult<f64> {
        if b == 0.0 {
            Err(SandboxError::InvalidParameters(
                "Division by zero".to_string(),
            ))
        } else {
            Ok(a / b)
        }
    }

    /// Calculate power
    fn power(&self, base: f64, exp: f64) -> f64 {
        base.powf(exp)
    }

    /// Calculate square root
    fn sqrt(&self, n: f64) -> SandboxResult<f64> {
        if n < 0.0 {
            Err(SandboxError::InvalidParameters(
                "Cannot calculate square root of negative number".to_string(),
            ))
        } else {
            Ok(n.sqrt())
        }
    }

    /// Calculate absolute value
    fn abs(&self, n: f64) -> f64 {
        n.abs()
    }

    /// Round to nearest integer
    fn round(&self, n: f64) -> f64 {
        n.round()
    }

    /// Floor
    fn floor(&self, n: f64) -> f64 {
        n.floor()
    }

    /// Ceiling
    fn ceil(&self, n: f64) -> f64 {
        n.ceil()
    }
}

#[async_trait::async_trait]
impl Tool for MathTool {
    fn name(&self) -> &str {
        "math"
    }

    fn description(&self) -> &str {
        "Performs mathematical operations: add, subtract, multiply, divide, power, sqrt, abs, round, floor, ceil"
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "The math operation to perform",
                    "enum": ["add", "subtract", "multiply", "divide", "power", "sqrt", "abs", "round", "floor", "ceil"]
                },
                "a": {
                    "type": "number",
                    "description": "First operand (used by add, subtract, multiply, divide, power)"
                },
                "b": {
                    "type": "number",
                    "description": "Second operand (used by add, subtract, multiply, divide, power)"
                },
                "n": {
                    "type": "number",
                    "description": "Single operand (used by sqrt, abs, round, floor, ceil). Falls back to 'a' if not provided."
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let operation = params
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("Missing 'operation' field".to_string())
            })?;

        let a = params.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = params.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let result = match operation {
            "add" => self.add(a, b),
            "subtract" => self.subtract(a, b),
            "multiply" => self.multiply(a, b),
            "divide" => self.divide(a, b)?,
            "power" => self.power(a, b),
            "sqrt" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.sqrt(n)?
            }
            "abs" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.abs(n)
            }
            "round" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.round(n)
            }
            "floor" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.floor(n)
            }
            "ceil" => {
                let n = params.get("n").and_then(|v| v.as_f64()).unwrap_or(a);
                self.ceil(n)
            }
            _ => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown operation: {}",
                    operation
                )));
            }
        };

        // Return as number if it's a whole number, otherwise float
        if result.fract() == 0.0 && result.is_finite() {
            Ok(Value::from(result as i64))
        } else {
            Ok(Value::from(result))
        }
    }
}

/// HTTP GET tool - performs HTTP GET requests
#[derive(Debug, Clone)]
pub struct HttpGetTool {
    client: reqwest::Client,
}

impl HttpGetTool {
    /// Create a new HTTP GET tool
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("ALMS/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }
}

impl Default for HttpGetTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }

    fn description(&self) -> &str {
        "Performs an HTTP GET request to a URL and returns the response body"
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to send a GET request to"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("Missing 'url' field".to_string()))?;

        // Parse headers if provided
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(hdrs) = params.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in hdrs {
                if let Some(val_str) = value.as_str()
                    && let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    && let Ok(header_value) = reqwest::header::HeaderValue::from_str(val_str)
                {
                    headers.insert(header_name, header_value);
                }
            }
        }

        // Build request
        let mut request = self.client.get(url);

        // Add headers
        if !headers.is_empty() {
            request = request.headers(headers);
        }

        // Execute request
        let response = request.send().await.map_err(SandboxError::from)?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body_text = response.text().await.map_err(SandboxError::from)?;

        // Try to parse as JSON, fallback to string
        let body = if let Ok(json) = serde_json::from_str::<Value>(&body_text) {
            json
        } else {
            Value::String(body_text)
        };

        // Build response object
        let result = serde_json::json!({
            "status": status,
            "content_type": content_type,
            "body": body,
        });

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Shell execution tool — see `crate::shell::ShellTool` (redesigned in #469).
// The old `ShellExecTool` struct has been removed. A type alias
// `ShellExecTool = ShellTool` is provided in `lib.rs` for backward compat.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Filesystem tools
// ---------------------------------------------------------------------------

/// Read a file from the filesystem.
#[derive(Debug, Clone, Default)]
pub struct FsReadTool {
    sandbox_root: Option<PathBuf>,
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
        }
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

        // Block device paths that would hang or produce infinite output.
        if is_blocked_device_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot read device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        // Deny-list check: block access to sensitive files regardless of sandbox scope.
        if is_denied_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
        }

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            PathBuf::from(path)
        };

        // Also check the resolved path (handles relative traversals like ../data/secrets.json).
        if is_blocked_device_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot read device path '{}' — this is a system device, not a regular file",
                path
            )));
        }
        if is_denied_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
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

        Ok(result)
    }
}

/// Write (or append to) a file on the filesystem.
#[derive(Debug, Clone, Default)]
pub struct FsWriteTool {
    sandbox_root: Option<PathBuf>,
}

impl FsWriteTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
        }
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

        // Deny-list check: block writes to sensitive files.
        if is_denied_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
        }

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            PathBuf::from(path)
        };

        // Also check the resolved path for denied filenames.
        if is_denied_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
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

        Ok(serde_json::json!({ "ok": true, "path": path }))
    }
}

/// List directory contents.
#[derive(Debug, Clone, Default)]
pub struct FsListTool {
    sandbox_root: Option<PathBuf>,
}

impl FsListTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
        }
    }
}

#[async_trait::async_trait]
impl Tool for FsListTool {
    fn name(&self) -> &str {
        "fs_list"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Returns filenames and whether each entry is a directory."
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
                    "description": "Directory path to list. Defaults to current working directory."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            PathBuf::from(path)
        };

        let mut read_dir = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to list '{}': {}", path, e)))?;

        const MAX_ENTRIES: usize = 500;
        let mut entries = Vec::new();
        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| SandboxError::Io(format!("Error reading dir entry: {}", e)))?
        {
            if entries.len() >= MAX_ENTRIES {
                entries.push(serde_json::json!({
                    "name": "…[truncated: more than 500 entries]",
                    "is_dir": false
                }));
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Filter denied filenames from directory listings to prevent
            // information disclosure (e.g. revealing that secrets.json exists).
            if is_denied_path(Path::new(&name)) {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(serde_json::json!({ "name": name, "is_dir": is_dir }));
        }

        // Sort: directories first, then files, both alphabetically.
        entries.sort_by(|a, b| {
            let a_dir = a["is_dir"].as_bool().unwrap_or(false);
            let b_dir = b["is_dir"].as_bool().unwrap_or(false);
            b_dir.cmp(&a_dir).then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["name"].as_str().unwrap_or(""))
            })
        });

        Ok(serde_json::json!({ "path": path, "entries": entries }))
    }
}

/// Search-and-replace editing tool for files.
///
/// Performs exact string matching (no regex). By default, the search string
/// must appear exactly once in the file (uniqueness guard). Set `replace_all`
/// to replace every occurrence.
///
/// Special case: when `old_string` is empty and the file does not exist (or
/// exists but is empty), the file is created/overwritten with `new_string`.
#[derive(Debug, Clone, Default)]
pub struct FsEditTool {
    sandbox_root: Option<PathBuf>,
}

impl FsEditTool {
    /// Create an unrestricted fs_edit tool (no sandbox check).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed fs_edit tool. Paths must resolve within `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
        }
    }
}

#[async_trait::async_trait]
impl Tool for FsEditTool {
    fn name(&self) -> &str {
        "fs_edit"
    }

    fn description(&self) -> &str {
        "Search and replace text in a file. Finds an exact string and replaces it. \
         By default the search string must appear exactly once (uniqueness guard). \
         Set replace_all to true to replace every occurrence. \
         When old_string is empty and the file does not exist, creates the file with new_string."
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
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text (must differ from old_string)."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "When true, replace all occurrences instead of requiring uniqueness. Default: false."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'path' is required".to_string()))?;

        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'old_string' is required".to_string())
            })?;

        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'new_string' is required".to_string())
            })?;

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // No-op rejection: old_string == new_string
        if old_string == new_string {
            return Err(SandboxError::InvalidParameters(
                "old_string and new_string must differ".to_string(),
            ));
        }

        // Deny-list check on raw path.
        if is_denied_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
        }

        // Resolve path within sandbox (or use as-is when unsandboxed).
        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            PathBuf::from(path)
        };

        // Deny-list check on resolved path.
        if is_denied_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
        }

        // Special case: empty old_string means "create file".
        if old_string.is_empty() {
            return self
                .handle_empty_old_string(&resolved, path, new_string)
                .await;
        }

        // File size guard — reject files larger than 2 MiB.
        const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;
        if meta.len() > MAX_EDIT_BYTES {
            return Err(SandboxError::InvalidParameters(format!(
                "File too large for fs_edit ({} bytes, max {}). Use fs_write for full file replacement.",
                meta.len(),
                MAX_EDIT_BYTES
            )));
        }

        // Read existing file content.
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;

        // Count occurrences.
        let count = content.matches(old_string).count();

        if count == 0 {
            return Err(SandboxError::InvalidParameters(format!(
                "old_string not found in '{}'",
                path
            )));
        }

        if !replace_all && count > 1 {
            return Err(SandboxError::InvalidParameters(format!(
                "old_string appears {} times in '{}'; set replace_all to true or provide a more unique string",
                count, path
            )));
        }

        // Perform the replacement.
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            // Replace only the first (and only) occurrence.
            content.replacen(old_string, new_string, 1)
        };

        // Write back.
        tokio::fs::write(&resolved, &new_content)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to write '{}': {}", path, e)))?;

        let replacements = if replace_all { count } else { 1 };
        Ok(serde_json::json!({
            "ok": true,
            "path": path,
            "replacements": replacements
        }))
    }
}

impl FsEditTool {
    /// Handle the special case where `old_string` is empty.
    ///
    /// - File does not exist: create it with `new_string`.
    /// - File exists and is empty: write `new_string`.
    /// - File exists and has content: error.
    async fn handle_empty_old_string(
        &self,
        resolved: &Path,
        display_path: &str,
        new_string: &str,
    ) -> SandboxResult<Value> {
        let file_exists = tokio::fs::metadata(resolved).await.is_ok();

        if file_exists {
            let content = tokio::fs::read_to_string(resolved).await.map_err(|e| {
                SandboxError::Io(format!("Failed to read '{}': {}", display_path, e))
            })?;

            if !content.is_empty() {
                return Err(SandboxError::InvalidParameters(format!(
                    "old_string is empty but '{}' has existing content; \
                     use a non-empty old_string to edit, or clear the file first",
                    display_path
                )));
            }
        }

        // Create parent directories if needed.
        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                SandboxError::Io(format!(
                    "Failed to create dirs for '{}': {}",
                    display_path, e
                ))
            })?;
        }

        tokio::fs::write(resolved, new_string)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to write '{}': {}", display_path, e)))?;

        Ok(serde_json::json!({
            "ok": true,
            "path": display_path,
            "replacements": 1
        }))
    }
}

// ── FsGrepTool ─────────────────────────────────────────────────────────────

/// VCS directories excluded from recursive directory walks.
const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// Maximum total output size in characters. Prevents context window overload.
const MAX_OUTPUT_CHARS: usize = 20_000;

/// Default head_limit when not specified by the caller.
const DEFAULT_HEAD_LIMIT: usize = 250;

/// Maximum file size (in bytes) for `search_file_content` which reads the
/// entire file into memory.  Files larger than this are skipped with a debug
/// log.  1 MiB is generous enough for most source files while preventing OOM
/// on multi-GB log files.
const MAX_GREP_FILE_BYTES: u64 = 1_024 * 1_024;

/// Maximum directory traversal depth for the recursive WalkDir.  Prevents
/// unbounded walks in deeply-nested trees (e.g. node_modules chains).
const MAX_WALK_DEPTH: usize = 50;

/// Regex content search tool for files.
///
/// Searches file contents using regex patterns within the sandbox. Supports
/// three output modes (files_with_matches, content, count), glob filtering,
/// case-insensitive matching, and result pagination via head_limit/offset.
///
/// Security: respects sandbox root, denied-path filtering, and VCS directory
/// exclusion. Marked as read-only and builtin.
#[derive(Debug, Clone, Default)]
pub struct FsGrepTool {
    sandbox_root: Option<PathBuf>,
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
        }
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
            // Block device paths that would hang or produce infinite output.
            if is_blocked_device_path(Path::new(path)) {
                return Err(SandboxError::SandboxViolation(format!(
                    "Cannot search device path '{}' — this is a system device, not a regular file",
                    path
                )));
            }

            // Deny-list check on raw path.
            if is_denied_path(Path::new(path)) {
                return Err(SandboxError::SandboxViolation(format!(
                    "Access to '{}' is denied",
                    path
                )));
            }

            if let Some(ref root) = self.sandbox_root {
                check_sandbox_path_async(path, root).await?
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

        // Offload blocking filesystem walk to a blocking thread.
        let sandbox_root_clone = self.sandbox_root.clone();
        let search_root_clone = search_root.clone();
        let glob_matcher_clone = glob_matcher.clone();

        let files: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
            collect_files(
                &search_root_clone,
                sandbox_root_clone.as_deref(),
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
/// exclusion, denied paths, and optional glob filtering.
///
/// When `search_root` is a regular file (not a directory), returns a
/// single-element vec containing just that file (single-file mode).
fn collect_files(
    search_root: &Path,
    sandbox_root: Option<&Path>,
    glob_matcher: Option<&globset::GlobMatcher>,
) -> Vec<PathBuf> {
    // Single-file mode: if the search root is a file, just search that file.
    if search_root.is_file() {
        if is_denied_path(search_root) || is_blocked_device_path(search_root) {
            return Vec::new();
        }
        return vec![search_root.to_path_buf()];
    }

    // Canonicalize the sandbox root so the `starts_with` check works
    // correctly even on Windows where walkdir may return UNC paths
    // (`\\?\C:\...`) while the stored root uses the short form (`C:\...`).
    let canonical_sandbox_root = sandbox_root.and_then(|r| canonicalize_best_effort(r).ok());

    let mut files = Vec::new();

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

            // Skip denied filenames.
            if is_denied_path(entry.path()) {
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

        // Enforce sandbox root on each discovered file.
        if let Some(ref root) = canonical_sandbox_root {
            // Use starts_with on the entry path directly — the walk already
            // starts within the sandbox, but symlinks inside the tree could
            // escape.  We compare against the canonicalized root so UNC vs
            // non-UNC forms do not cause false negatives on Windows.
            if !path.starts_with(root) {
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

        files.push(path.to_path_buf());
    }

    // Sort for deterministic output order.
    files.sort();
    files
}

/// Relativize a path against the search root for token-efficient output.
fn relativize(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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

            // Apply offset.
            if total_found <= offset {
                continue;
            }

            // Apply head_limit.
            if matched_files.len() >= effective_limit {
                truncated = true;
                // Stop scanning once both caps are hit — the total is
                // approximate from this point.
                break;
            }

            let rel = relativize(file, search_root);

            // Check output size cap.
            output_chars += rel.len() + 4; // account for JSON array overhead
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

            // Apply offset.
            if total_found <= offset {
                continue;
            }

            // Apply head_limit.
            if results.len() >= effective_limit {
                truncated = true;
                // Stop scanning once the cap is hit — the total is
                // approximate from this point.
                break 'outer;
            }

            let rel = relativize(file, search_root);

            // Deduplicate overlapping context: when adjacent matches have
            // overlapping before/after context windows, trim the "before"
            // context of the current match so it does not repeat lines
            // already shown in the previous match's "after" context.
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
                        // Last line covered by the previous entry (1-based).
                        let prev_coverage = prev_line + prev_after_len;
                        // First line of current context_before (1-based).
                        let cur_ctx_start = m.line.saturating_sub(m.context_before.len());
                        if prev_coverage >= cur_ctx_start {
                            // Trim overlapping prefix lines.
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

            // Check output size cap.
            let entry_size = serde_json::to_string(&entry).unwrap_or_default().len();
            output_chars += entry_size + 2; // comma + whitespace
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

        // Apply offset.
        if files_found <= offset {
            continue;
        }

        // Apply head_limit.
        if results.len() >= effective_limit {
            truncated = true;
            // Stop scanning once the cap is hit — the total is
            // approximate from this point.
            break;
        }

        let rel = relativize(file, search_root);
        let entry = serde_json::json!({
            "file": rel,
            "count": count
        });

        // Check output size cap.
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

    // On I/O error (e.g. binary file with invalid UTF-8) the loop exits
    // gracefully — `Err(...)` breaks the `while let Ok(...)` pattern.
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
    // File size guard — skip files larger than MAX_GREP_FILE_BYTES since
    // this function reads the entire file into memory for context extraction.
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

    // On I/O error (e.g. binary file with invalid UTF-8) the loop exits
    // gracefully — `Err(...)` breaks the `while let Ok(...)` pattern.
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

    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool::new();

        // Test with message field
        let result = tool
            .execute(serde_json::json!({"message": "hello"}))
            .await
            .unwrap();
        assert_eq!(result, "hello");

        // Test without message field
        let result = tool
            .execute(serde_json::json!({"key": "value"}))
            .await
            .unwrap();
        assert_eq!(result, serde_json::json!({"key": "value"}));
    }

    // ── DatetimeTool ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_datetime_tool_returns_valid_fields() {
        let tool = DatetimeTool::new();
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        // Must contain all expected UTC fields
        assert!(result.get("iso").is_some(), "missing 'iso' field");
        assert!(result.get("human").is_some(), "missing 'human' field");
        assert_eq!(result["timezone"], "UTC");

        // Must contain all expected local fields
        assert!(
            result.get("local_iso").is_some(),
            "missing 'local_iso' field"
        );
        assert!(
            result.get("local_human").is_some(),
            "missing 'local_human' field"
        );
        assert!(
            result.get("utc_offset").is_some(),
            "missing 'utc_offset' field"
        );

        // local_timezone should be an IANA name (e.g. "Europe/Istanbul"), not a numeric offset
        let tz = result["local_timezone"]
            .as_str()
            .expect("local_timezone must be a string");
        assert!(!tz.is_empty(), "local_timezone must not be empty");
        assert!(
            tz.contains('/') || tz == "Unknown",
            "local_timezone should be an IANA name like 'Region/City', got: {tz}"
        );

        // utc_offset should look like a numeric offset (e.g. "+03:00")
        let offset = result["utc_offset"]
            .as_str()
            .expect("utc_offset must be a string");
        assert!(
            offset.starts_with('+') || offset.starts_with('-'),
            "utc_offset should start with +/-, got: {offset}"
        );

        // UTC ISO string must parse back into a valid DateTime
        let iso_str = result["iso"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(iso_str)
            .unwrap_or_else(|_| panic!("invalid ISO 8601: {}", iso_str));

        // Local ISO string must also parse back into a valid DateTime
        let local_iso_str = result["local_iso"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(local_iso_str)
            .unwrap_or_else(|_| panic!("invalid local ISO 8601: {}", local_iso_str));
    }

    #[test]
    fn test_datetime_tool_is_auto_approved() {
        let tool = DatetimeTool::new();
        assert!(tool.is_auto_approved());
        assert!(tool.is_builtin());
    }

    // ── Auto-approved flag on builtins ────────────────────────────────────

    #[test]
    fn test_echo_is_auto_approved() {
        assert!(EchoTool::new().is_auto_approved());
    }

    #[test]
    fn test_dangerous_tools_are_not_auto_approved() {
        // Tools that modify state must NOT be auto-approved.
        assert!(!MathTool::new().is_auto_approved());
        assert!(!HttpGetTool::new().is_auto_approved());
        assert!(!FsReadTool::new().is_auto_approved());
        assert!(!FsWriteTool::new().is_auto_approved());
        assert!(!FsListTool::new().is_auto_approved());
        assert!(!FsEditTool::new().is_auto_approved());
    }

    #[tokio::test]
    async fn test_math_tool_add() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "add", "a": 10, "b": 32}))
            .await
            .unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_math_tool_divide() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "divide", "a": 10, "b": 2}))
            .await
            .unwrap();
        assert_eq!(result, 5);

        // Test division by zero
        let result = tool
            .execute(serde_json::json!({"operation": "divide", "a": 10, "b": 0}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_math_tool_sqrt() {
        let tool = MathTool::new();

        let result = tool
            .execute(serde_json::json!({"operation": "sqrt", "n": 16}))
            .await
            .unwrap();
        assert_eq!(result, 4);

        // Test negative number
        let result = tool
            .execute(serde_json::json!({"operation": "sqrt", "n": -1}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_http_get_invalid_url() {
        let tool = HttpGetTool::new();

        // Missing URL
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_tool_descriptions() {
        assert!(!EchoTool::new().description().is_empty());
        assert!(!MathTool::new().description().is_empty());
        assert!(!HttpGetTool::new().description().is_empty());
        // ShellTool description tested in crate::shell::tests
        assert!(!FsReadTool::new().description().is_empty());
        assert!(!FsWriteTool::new().description().is_empty());
        assert!(!FsListTool::new().description().is_empty());
        assert!(!FsEditTool::new().description().is_empty());
    }

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

    // ── ShellTool tests are in crate::shell::tests ────────────────────────────

    // ── FsReadTool ────────────────────────────────────────────────────────────

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
        // Read entire file to EOF => total_lines is present
        assert_eq!(result["total_lines"], 3);
        assert_eq!(result["lines_returned"], 3);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
        // When no more lines exist in either direction, offset/limit absent
        assert!(result.get("offset").is_none());
        assert!(result.get("limit").is_none());
    }

    #[tokio::test]
    async fn test_fs_read_large_file_default_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        // Create a file with 2500 lines
        let content: String = (1..=2500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();

        // Stopped early due to limit => total_lines not available
        assert!(result.get("total_lines").is_none());
        assert_eq!(result["lines_returned"], 2000);
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], true);
        assert_eq!(result["offset"], 0);
        assert_eq!(result["limit"], 2000);
        // First line should be present
        let text = result["content"].as_str().unwrap();
        assert!(text.starts_with("     1\tline 1\n"));
    }

    #[tokio::test]
    async fn test_fs_read_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("numbered.txt");
        let content: String = (1..=100).map(|i| format!("line-{i}\n")).collect();
        std::fs::write(&path, &content).unwrap();

        // Read lines 51-60 (offset 50, limit 10)
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
        assert!(!text.contains("    50\t")); // line 50 not included
        assert!(!text.contains("    61\t")); // line 61 not included
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
        // Read to EOF (past all lines) => total_lines known
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

        // Lines 6-10 (0-based index 5 onwards); 5 of 10 returned
        let text = result["content"].as_str().unwrap();
        assert!(text.contains("     6\titem-6"));
        assert!(text.contains("    10\titem-10"));
        assert!(!text.contains("     5\titem-5"));
        assert_eq!(result["lines_returned"], 5);
        // has_more_before=true because lines 1-5 are not included
        assert_eq!(result["has_more_before"], true);
        // Read to EOF => total_lines known
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
        // cat -n format: 6-char right-justified number + tab + content
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

        // String offset should error, not silently fall back to 0
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

        // String limit should error
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
        // Create a file slightly over 256 KB
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
        // Error message should suggest using offset/limit
        assert!(
            err_msg.contains("offset and limit"),
            "error should suggest offset/limit, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_read_byte_budget_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many_lines.txt");
        // Many very short lines: each line is 2 bytes on disk ("x\n") but
        // ~10 bytes formatted ("     1\tx\n"). 40k lines = 80KB on disk
        // (under 256KB limit) but ~400KB formatted, which eventually
        // exceeds the 512KB output budget when requesting all lines.
        // To reliably hit the budget: use slightly longer lines.
        // 10-char lines: 12 bytes on disk, ~20 bytes formatted.
        // 30k lines = ~360KB on disk (over 256KB). Let's use 20k lines
        // of 10-char each = ~220KB on disk (under 256KB), ~400KB formatted.
        // That's still under 512KB. Use 1-char lines instead:
        // 40k lines * 2 bytes = 80KB on disk. 40k * ~10 bytes = 400KB formatted.
        // Need more: 60k lines * 2 bytes = 120KB on disk. 60k * 10 = 600KB formatted.
        let content: String = (0..60_000).map(|_| "x\n").collect();
        std::fs::write(&path, &content).unwrap();

        let result = FsReadTool::new()
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "limit": 60000
            }))
            .await
            .unwrap();

        // Should have returned fewer than 60k lines due to byte budget
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
        // When we read the entire file, total_lines should be present
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
        // When we stop early due to limit, total_lines should be absent
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
        // offset>0 and more lines after => both indicators true
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

    // ── FsReadTool safety limits (#666) ─────────────────────────────────────

    #[test]
    fn test_is_blocked_device_path_unix_devices() {
        // On Unix these are exact-path matches; on Windows this function
        // checks reserved device names — we verify whichever platform we're on.
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

    #[tokio::test]
    async fn test_fs_read_blocked_device_path() {
        // Attempt to read a device path — should be rejected before any I/O.
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
        // Error must contain the actual file size
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

    // ── FsWriteTool ───────────────────────────────────────────────────────────

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

        // Write
        let result = FsWriteTool::new()
            .execute(serde_json::json!({"path": path_str, "content": "world"}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        // Read back — content now has cat -n line numbers
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
        // "line1\nline2\n" yields 2 lines via BufReader::lines()
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("     1\tline1"));
        assert!(content.contains("     2\tline2"));
        assert_eq!(result["has_more_before"], false);
        assert_eq!(result["has_more_after"], false);
    }

    #[tokio::test]
    async fn test_fs_read_has_more_after_exactly_one_past_end() {
        // Regression: when a file has exactly offset + limit + 1 lines, the
        // in-loop peek at `line_idx == end` consumed the extra line without
        // recording it, then the post-loop peek found nothing and incorrectly
        // set has_more_after = false.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exact.txt");
        let path_str = path.to_str().unwrap();

        // 6 lines total — offset=0, limit=5 → should return lines 1-5,
        // has_more_after = true because line 6 exists.
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
        // total_lines should NOT be present since we didn't read to EOF
        assert!(
            result.get("total_lines").is_none(),
            "total_lines should be absent when not at EOF"
        );
    }

    // ── FsListTool ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fs_list_sandbox_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsListTool::sandboxed(root);
        let result = tool.execute(serde_json::json!({"path": "../../"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fs_list_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().to_str().unwrap();

        // Create a file and a subdirectory
        std::fs::write(dir.path().join("file.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let result = FsListTool::new()
            .execute(serde_json::json!({"path": path_str}))
            .await
            .unwrap();

        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        // Dirs come first
        assert_eq!(entries[0]["name"], "subdir");
        assert_eq!(entries[0]["is_dir"], true);
        assert_eq!(entries[1]["name"], "file.txt");
        assert_eq!(entries[1]["is_dir"], false);
    }

    #[tokio::test]
    async fn test_fs_list_nonexistent() {
        let result = FsListTool::new()
            .execute(serde_json::json!({"path": "nonexistent_dir_xyz"}))
            .await;
        assert!(result.is_err());
    }

    // ── Denylist tests ──────────────────────────────────────────────────────

    #[test]
    fn test_is_denied_path_secrets_json() {
        assert!(is_denied_path(Path::new("secrets.json")));
        assert!(is_denied_path(Path::new("data/secrets.json")));
        assert!(is_denied_path(Path::new("/abs/path/data/secrets.json")));
    }

    #[test]
    fn test_is_denied_path_allowed_files() {
        assert!(!is_denied_path(Path::new("data.json")));
        assert!(!is_denied_path(Path::new("my_secrets.json")));
        assert!(!is_denied_path(Path::new("goals.md")));
        assert!(!is_denied_path(Path::new("alms.db")));
    }

    #[tokio::test]
    async fn test_fs_read_denied_secrets_json() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");
        std::fs::write(&secrets, r#"{"key": "sk-1234"}"#).unwrap();

        let tool = FsReadTool::new();
        let result = tool
            .execute(serde_json::json!({"path": secrets.to_str().unwrap()}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_fs_write_denied_secrets_json() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");

        let tool = FsWriteTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": secrets.to_str().unwrap(),
                "content": "malicious"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_shell_denied_secrets_json() {
        // Uses the ShellTool via the crate-level type alias
        let tool = crate::ShellTool::new();
        let result = tool
            .execute(serde_json::json!({"command": "cat data/secrets.json"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
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

    // ── Case-insensitive denylist tests ─────────────────────────────────────

    #[test]
    fn test_is_denied_path_case_insensitive() {
        // Windows filesystems are case-insensitive, so all casings must be denied.
        assert!(is_denied_path(Path::new("Secrets.JSON")));
        assert!(is_denied_path(Path::new("SECRETS.json")));
        assert!(is_denied_path(Path::new("secrets.JSON")));
        assert!(is_denied_path(Path::new("data/Secrets.Json")));
    }

    // ── Denylist via path traversal ─────────────────────────────────────────

    #[tokio::test]
    async fn test_fs_read_denied_via_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("sub").join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("secrets.json"), "secret").unwrap();

        let tool = FsReadTool::sandboxed(dir.path().to_path_buf());
        // The resolved path ends with `secrets.json` — denied by the
        // post-resolution check even though the raw path uses traversal.
        let result = tool
            .execute(serde_json::json!({"path": "sub/data/secrets.json"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_fs_read_denied_via_dot_slash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secrets.json"), "secret").unwrap();

        let tool = FsReadTool::sandboxed(dir.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"path": "./secrets.json"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_fs_read_denied_via_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secrets.json"), "secret").unwrap();

        let tool = FsReadTool::sandboxed(dir.path().to_path_buf());
        // `data/../secrets.json` resolves to `secrets.json` in the sandbox root.
        let result = tool
            .execute(serde_json::json!({"path": "data/../secrets.json"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    // ── fs_list denylist filtering ──────────────────────────────────────────

    #[tokio::test]
    async fn test_fs_list_hides_denied_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secrets.json"), "secret").unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        std::fs::write(dir.path().join("data.txt"), "hello").unwrap();

        let tool = FsListTool::sandboxed(dir.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"path": "."}))
            .await
            .unwrap();
        let entries = result["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(
            !names.contains(&"secrets.json"),
            "secrets.json should be filtered from directory listing"
        );
        assert!(names.contains(&"config.json"));
        assert!(names.contains(&"data.txt"));
    }

    // ── canonicalize_best_effort edge cases (issue #57) ─────────────────────

    #[test]
    fn test_canonicalize_deep_mixed_traversal() {
        // a/b/../../c/../../../secret should escape the sandbox root
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("a/b/../../c/../../../secret", &root).is_err());
    }

    #[test]
    fn test_canonicalize_dot_only_paths() {
        // Dot-only paths should resolve to the sandbox root itself
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
        // Empty string should resolve to the sandbox root (no components)
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let result = check_sandbox_path("", &root);
        assert!(result.is_ok(), "empty string should resolve inside sandbox");
        assert_eq!(result.unwrap(), root);
    }

    #[test]
    fn test_canonicalize_excessive_parent_pops() {
        // More .. pops than path depth should escape sandbox
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(check_sandbox_path("../../../..", &root).is_err());
        assert!(check_sandbox_path("../../../../../../etc/passwd", &root).is_err());
    }

    #[test]
    fn test_canonicalize_trailing_slashes() {
        // Trailing slashes should not affect sandbox containment
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

        // Trailing slash on non-existent path should also stay inside
        let result_new = check_sandbox_path("newdir/subdir/", &root);
        assert!(
            result_new.is_ok(),
            "trailing slash on new path should stay inside sandbox"
        );
        assert!(result_new.unwrap().starts_with(&root));
    }

    // ── Windows mixed-separator test (S1 from Tim's review) ────────────────

    #[cfg(windows)]
    #[test]
    fn test_canonicalize_mixed_separators_windows() {
        // On Windows, backslash-mixed paths like `foo\..\..\secret` are a
        // classic sandbox bypass vector.  `Path::components()` normalizes
        // separators, so the function should be safe — this test locks down
        // that assumption.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        // Pure backslash traversal
        assert!(
            check_sandbox_path("foo\\..\\..\\..\\secret", &root).is_err(),
            "backslash traversal should be rejected"
        );
        // Mixed forward- and back-slash traversal
        assert!(
            check_sandbox_path("foo/..\\..\\secret", &root).is_err(),
            "mixed-separator traversal should be rejected"
        );
    }

    // ── Null byte injection test (S2 from Tim's review) ────────────────────

    #[test]
    fn test_canonicalize_null_byte_injection() {
        // Some OS APIs truncate at `\0`, which could let an attacker turn
        // `"safe\0/../../../etc/passwd"` into just `"safe"`.  Rust's
        // `std::fs::canonicalize` and `path.exists()` reject embedded nulls
        // on all platforms, so `canonicalize_best_effort` falls through to
        // the component-by-component walk where the NUL stays in the
        // `Normal` component.  Regardless of the exact error, the path must
        // never resolve to something *outside* the sandbox.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        // Null byte mid-path — should either error or stay inside sandbox
        let result = check_sandbox_path("safe\0/../../../etc/passwd", &root);
        if let Ok(resolved) = &result {
            assert!(
                resolved.starts_with(&root),
                "null-byte path resolved outside sandbox: {}",
                resolved.display()
            );
        }
        // Error is also acceptable — the path is not usable

        // Null byte in a simple filename — same contract
        let result2 = check_sandbox_path("file\0.txt", &root);
        if let Ok(resolved) = &result2 {
            assert!(
                resolved.starts_with(&root),
                "null-byte filename resolved outside sandbox: {}",
                resolved.display()
            );
        }
    }

    // ── FsEditTool ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fs_edit_unique_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn test_fs_edit_reject_multiple_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "aaa bbb aaa ccc aaa").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "xxx"
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("3 times"),
            "error should mention count, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "aaa bbb aaa ccc aaa").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "xxx",
                "replace_all": true
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 3);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "xxx bbb xxx ccc xxx"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "missing",
                "new_string": "replacement"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_fs_edit_noop_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "world"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must differ"));
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.txt");
        assert!(!path.exists());

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "brand new content"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "brand new content");
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_on_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "new content"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_on_nonempty_file_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "existing content").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "replacement"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("existing content"));
    }

    #[tokio::test]
    async fn test_fs_edit_sandbox_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsEditTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({
                "path": "../../etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("outside sandbox"));
    }

    #[tokio::test]
    async fn test_fs_edit_denied_path() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");
        std::fs::write(&secrets, r#"{"key": "value"}"#).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": secrets.to_str().unwrap(),
                "old_string": "value",
                "new_string": "hacked"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
    }

    #[tokio::test]
    async fn test_fs_edit_preserves_surrounding_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "before TARGET after").unwrap();

        let tool = FsEditTool::new();
        tool.execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "TARGET",
            "new_string": "REPLACED"
        }))
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "before REPLACED after"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "line1\nold_line2\nold_line3\nline4").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "old_line2\nold_line3",
                "new_string": "new_line2\nnew_line3\nnew_extra"
            }))
            .await
            .unwrap();

        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nnew_line2\nnew_line3\nnew_extra\nline4"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unicode.txt");
        std::fs::write(&path, "Hej v\u{00e4}rlden! \u{1f600} foo").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "v\u{00e4}rlden! \u{1f600}",
                "new_string": "\u{4e16}\u{754c}"
            }))
            .await
            .unwrap();

        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Hej \u{4e16}\u{754c} foo"
        );
    }

    #[test]
    fn test_fs_edit_not_auto_approved() {
        assert!(!FsEditTool::new().is_auto_approved());
    }

    #[test]
    fn test_fs_edit_description_nonempty() {
        assert!(!FsEditTool::new().description().is_empty());
    }

    #[tokio::test]
    async fn test_fs_edit_rejects_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        // Create a file just over 2 MiB.
        let content = "x".repeat(2 * 1024 * 1024 + 1);
        std::fs::write(&path, &content).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "x",
                "new_string": "y",
                "replace_all": true
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "error should mention size limit, got: {err}"
        );
    }

    // ── FsGrepTool ────────────────────────────────────────────────────────────

    /// Helper: create a temp directory with test files for grep tests.
    fn setup_grep_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // src/main.rs
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"Hello, world!\");\n    let x = 42;\n}\n",
        )
        .unwrap();

        // src/lib.rs
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn hello() {\n    println!(\"Hello from lib\");\n}\n",
        )
        .unwrap();

        // README.md
        std::fs::write(
            root.join("README.md"),
            "# My Project\n\nHello world example.\n",
        )
        .unwrap();

        // data/config.toml
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("data/config.toml"),
            "[server]\nport = 8080\nhost = \"localhost\"\n",
        )
        .unwrap();

        // .git directory (should be excluded)
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        // secrets.json (should be excluded by denied-path filter)
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

        // Search for "fn " across all files
        let result = tool
            .execute(serde_json::json!({
                "pattern": "^(pub )?fn \\w+",
                "path": root.to_str().unwrap(),
                "output_mode": "content"
            }))
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        // main.rs has "fn main()" and lib.rs has "pub fn add" and "pub fn hello"
        assert_eq!(result["total"], 3);
        assert!(!matches.is_empty());
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
        // println appears in src/main.rs and src/lib.rs
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
        // "pub fn add" is on line 1
        assert_eq!(matches[0]["line"], 1);
        // "pub fn hello" is on line 5
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
        // src/main.rs has 1 println, src/lib.rs has 1 println
        assert_eq!(result["total_matches"], 2);
        assert_eq!(matches.len(), 2);

        // Verify each match has a file and count field.
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

        // Context before: line 2 (println)
        let ctx_before = matches[0]["context_before"].as_array().unwrap();
        assert_eq!(ctx_before.len(), 1);
        assert!(ctx_before[0].as_str().unwrap().contains("println"));

        // Context after: line 4 ("}")
        let ctx_after = matches[0]["context_after"].as_array().unwrap();
        assert_eq!(ctx_after.len(), 1);
        assert!(ctx_after[0].as_str().unwrap().contains("}"));
    }

    #[tokio::test]
    async fn test_fs_grep_case_insensitive() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        // "hello" should match "Hello" in multiple files when case-insensitive
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

        // Case-insensitive should find more or equal matches
        let sensitive_total = result_sensitive["total"].as_u64().unwrap();
        let insensitive_total = result_insensitive["total"].as_u64().unwrap();
        assert!(
            insensitive_total >= sensitive_total,
            "Case-insensitive should find at least as many matches: {insensitive_total} >= {sensitive_total}"
        );
        // "Hello" appears in README, main.rs, and lib.rs — case-insensitive should find them
        assert!(insensitive_total >= 2);
    }

    #[tokio::test]
    async fn test_fs_grep_head_limit_truncates() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        // Search for something common, limit to 1 result
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
        // total should reflect all found, not just returned
        assert!(result["total"].as_u64().unwrap() > 1);
    }

    #[tokio::test]
    async fn test_fs_grep_offset_pagination() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        // Get all files with matches first
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

        // Page 1: offset=0, limit=2
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

        // Page 2: offset=2, limit=2
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

        // Pages should not overlap
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

        // Only search .rs files
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
        // Only .rs files should be in results (not README.md)
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

        // .git/HEAD contains "ref: refs/heads/main" — should not be found
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

    #[tokio::test]
    async fn test_fs_grep_denied_paths_excluded() {
        let dir = setup_grep_dir();
        let root = dir.path();
        let tool = FsGrepTool::new();

        // secrets.json contains "sk-1234" — should not be found
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
            0,
            "Denied files should be excluded from results"
        );
    }

    #[tokio::test]
    async fn test_fs_grep_sandbox_root_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGrepTool::sandboxed(root);

        // Trying to search outside sandbox should fail
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

        // No output_mode specified — should default to files_with_matches
        let result = tool
            .execute(serde_json::json!({
                "pattern": "main",
                "path": file.to_str().unwrap()
            }))
            .await
            .unwrap();

        // files_with_matches mode returns a "matches" array of strings (paths)
        assert!(result["matches"].is_array());
        assert!(result["total"].is_number());
        // Single file mode should return the file as a relative path
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
        // Path should be relative, not absolute, using forward slashes
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

        // Should return all matching files (no truncation)
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

        // Match on first line — context_before should be empty
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
        // context_after should have 2 lines (second, third)
        assert_eq!(matches[0]["context_after"].as_array().unwrap().len(), 2);

        // Match on last line — context_after should be empty
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
        // context_before should have 2 lines
        assert_eq!(matches2[0]["context_before"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_fs_grep_denied_path_in_path_param() {
        let tool = FsGrepTool::new();

        // Trying to search secrets.json directly should fail
        let result = tool
            .execute(serde_json::json!({
                "pattern": "key",
                "path": "secrets.json"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("denied"));
    }

    #[tokio::test]
    async fn test_fs_grep_output_size_cap() {
        // Create a directory with many files containing matching content
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

        // The output should be truncated at MAX_OUTPUT_CHARS
        // Results might be capped due to the 20,000 char limit
        // We just verify the structure is valid and truncated is set correctly
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
        assert_eq!(result["total"], 1); // Total found is still 1
    }

    #[tokio::test]
    async fn test_fs_grep_no_path_defaults_to_sandbox_root() {
        let dir = setup_grep_dir();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsGrepTool::sandboxed(root);

        // No path param — should search sandbox root
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
}
