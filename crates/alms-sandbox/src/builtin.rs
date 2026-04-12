use crate::shell::security::DENIED_FILENAMES;
use crate::{SandboxError, Tool, error::SandboxResult};
use alms_core::truncate_to_char_boundary;
use chrono::{Local, Utc};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

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
fn safe_truncate(s: &str, max_bytes: usize) -> String {
    let truncated = truncate_to_char_boundary(s, max_bytes);
    if truncated.len() == s.len() {
        s.to_owned()
    } else {
        format!(
            "{}…[truncated, {} bytes omitted]",
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
        "Read the text content of a file. Returns the file's content as a string."
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
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'path' is required".to_string()))?;

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
        if is_denied_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
        }

        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;

        const MAX_CONTENT: usize = 32000;
        let result = safe_truncate(&content, MAX_CONTENT);

        Ok(serde_json::json!({ "content": result }))
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

        // Read back
        let result = FsReadTool::new()
            .execute(serde_json::json!({"path": path_str}))
            .await
            .unwrap();
        assert_eq!(result["content"], "world");
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
        assert_eq!(result["content"], "line1\nline2\n");
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
}
