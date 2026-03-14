use crate::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use tracing::warn;

/// Built-in tool trait marker
pub trait BuiltinTool: Tool {}

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

    // Resolve: relative paths join to sandbox_root, absolute stay as-is
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        sandbox_root.join(p)
    };

    // Canonicalize to follow symlinks. Walk up for non-existent paths.
    let canonical = canonicalize_best_effort(&resolved)
        .map_err(|e| SandboxError::SandboxViolation(format!("Cannot resolve '{}': {}", path, e)))?;

    if !canonical.starts_with(sandbox_root) {
        return Err(SandboxError::SandboxViolation(format!(
            "Path '{}' is outside sandbox root",
            path
        )));
    }

    Ok(canonical)
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
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    // Walk back from max_bytes until we land on a char boundary.
    let boundary = (0..=max_bytes)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!(
        "{}…[truncated, {} bytes omitted]",
        &s[..boundary],
        s.len() - boundary
    )
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

impl BuiltinTool for EchoTool {}

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

impl BuiltinTool for MathTool {}

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
            .user_agent("ALMS/0.1.0")
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }

    /// Create with a custom client
    pub fn with_client(client: reqwest::Client) -> Self {
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

impl BuiltinTool for HttpGetTool {}

// ---------------------------------------------------------------------------
// Shell execution tool
// ---------------------------------------------------------------------------

/// Shell exec tool — runs an external program via argv (no shell injection).
///
/// Parameters:
///   argv     : [string, ...]   — program + args, e.g. ["ls", "-la"]
///   cwd      : string?         — working directory (default: current dir)
///   env      : {string: string}? — extra environment variables
///   timeout_secs : number?    — max wait time in seconds (default: 30, max: 120)
#[derive(Debug, Clone)]
pub struct ShellExecTool {
    /// When Some, cwd is validated against this root in sandboxed mode.
    sandbox_root: Option<PathBuf>,
    /// When true, no cwd restriction is applied (power-user mode).
    unrestricted: bool,
    /// Default working directory when no explicit `cwd` param is provided.
    /// Used to set the agent's workspace as the "home directory".
    /// Takes precedence over `sandbox_root` as default cwd.
    default_cwd: Option<PathBuf>,
}

impl Default for ShellExecTool {
    fn default() -> Self {
        Self {
            sandbox_root: None,
            unrestricted: true,
            default_cwd: None,
        }
    }
}

impl ShellExecTool {
    /// Create an unrestricted shell_exec tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed shell_exec tool. The cwd is restricted to `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
            unrestricted: false,
            default_cwd: None,
        }
    }

    /// Create with explicit policy.
    pub fn with_policy(sandbox_root: Option<PathBuf>, unrestricted: bool) -> Self {
        Self {
            sandbox_root,
            unrestricted,
            default_cwd: None,
        }
    }

    /// Set the default working directory for commands without an explicit `cwd` param.
    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        if let Some(ref root) = self.sandbox_root
            && !cwd.starts_with(root)
        {
            warn!(
                default_cwd = %cwd.display(),
                sandbox_root = %root.display(),
                "default_cwd is outside sandbox_root — commands without explicit cwd will run outside the sandbox boundary"
            );
        }
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn description(&self) -> &str {
        "Run an external program via argv array (no shell — safe from injection). \
        Returns stdout, stderr, and exit_code. Use for file system operations, \
        running scripts, checking system state, etc."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Program and arguments, e.g. [\"ls\", \"-la\", \"/tmp\"]",
                    "minItems": 1
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the process. Defaults to the agent workspace when available, otherwise the project root."
                },
                "env": {
                    "type": "object",
                    "description": "Extra environment variables as key-value pairs.",
                    "additionalProperties": { "type": "string" }
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Max execution time in seconds (default 30, max 120)."
                }
            },
            "required": ["argv"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let argv = params
            .get("argv")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'argv' must be an array".to_string())
            })?;

        if argv.is_empty() {
            return Err(SandboxError::InvalidParameters(
                "'argv' must not be empty".to_string(),
            ));
        }

        let program = argv[0].as_str().ok_or_else(|| {
            SandboxError::InvalidParameters("argv[0] must be a string".to_string())
        })?;

        let args = argv[1..]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.as_str().ok_or_else(|| {
                    SandboxError::InvalidParameters(format!("argv[{}] must be a string", i + 1))
                })
            })
            .collect::<SandboxResult<Vec<&str>>>()?;

        let timeout_secs = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(120);

        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        if let Some(cwd) = params.get("cwd").and_then(|v| v.as_str()) {
            if !self.unrestricted
                && let Some(ref root) = self.sandbox_root
            {
                check_sandbox_path(cwd, root)?;
            }
            cmd.current_dir(cwd);
        } else if let Some(ref default) = self.default_cwd {
            // Agent workspace "home directory" — use as default cwd
            cmd.current_dir(default);
        } else if !self.unrestricted
            && let Some(ref root) = self.sandbox_root
        {
            // Fallback: sandbox root as cwd
            cmd.current_dir(root);
        }

        // Clear the daemon's environment (which holds API keys, tokens, etc.)
        // before applying user-specified env vars.
        cmd.env_clear();
        if let Some(env_obj) = params.get("env").and_then(|v| v.as_object()) {
            for (k, v) in env_obj {
                if let Some(val) = v.as_str() {
                    cmd.env(k, val);
                }
            }
        }

        let child = cmd
            .spawn()
            .map_err(|e| SandboxError::Io(format!("Failed to spawn '{}': {}", program, e)))?;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| SandboxError::Io(format!("Process timed out after {}s", timeout_secs)))?
        .map_err(|e| SandboxError::Io(format!("Process error: {}", e)))?;

        const MAX_OUTPUT: usize = 8000;
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout_str = safe_truncate(&stdout, MAX_OUTPUT);
        let stderr_str = safe_truncate(&stderr, MAX_OUTPUT);

        Ok(serde_json::json!({
            "exit_code": result.status.code().unwrap_or(-1),
            "stdout": stdout_str,
            "stderr": stderr_str,
        }))
    }
}

impl BuiltinTool for ShellExecTool {}

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

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path(path, root)?
        } else {
            PathBuf::from(path)
        };

        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;

        const MAX_CONTENT: usize = 32000;
        let result = safe_truncate(&content, MAX_CONTENT);

        Ok(serde_json::json!({ "content": result }))
    }
}

impl BuiltinTool for FsReadTool {}

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

        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path(path, root)?
        } else {
            PathBuf::from(path)
        };

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

impl BuiltinTool for FsWriteTool {}

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
            check_sandbox_path(path, root)?
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

impl BuiltinTool for FsListTool {}

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
        assert!(!ShellExecTool::new().description().is_empty());
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

    // ── ShellExecTool ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_shell_exec_missing_argv() {
        let result = ShellExecTool::new().execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_exec_empty_argv() {
        let result = ShellExecTool::new()
            .execute(serde_json::json!({"argv": []}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shell_exec_non_string_arg() {
        let result = ShellExecTool::new()
            .execute(serde_json::json!({"argv": ["echo", 42]}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_echo() {
        let result = ShellExecTool::new()
            .execute(serde_json::json!({"argv": ["echo", "hello"]}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_env_cleared() {
        // The daemon env has OPENROUTER_API_KEY etc. After env_clear(), "env" output
        // should not contain any inherited vars — just whatever PATH-equivalent the
        // OS injects by default.
        let result = ShellExecTool::new()
            .execute(serde_json::json!({"argv": ["env"]}))
            .await
            .unwrap();
        // With env_clear(), no OPENROUTER_API_KEY should leak.
        assert!(
            !result["stdout"]
                .as_str()
                .unwrap()
                .contains("OPENROUTER_API_KEY")
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_sandboxed_cwd_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = ShellExecTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({"argv": ["ls"], "cwd": "/etc"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("outside sandbox"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_sandboxed_default_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // Create a marker file so we can verify cwd
        std::fs::write(root.join("marker.txt"), "found").unwrap();
        let tool = ShellExecTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({"argv": ["cat", "marker.txt"]}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("found"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_default_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspace");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("marker.txt"), "home").unwrap();

        let tool = ShellExecTool::new().with_default_cwd(ws_dir);
        let result = tool
            .execute(serde_json::json!({"argv": ["cat", "marker.txt"]}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("home"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_explicit_cwd_overrides_default() {
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = dir.path().join("workspace");
        let other_dir = dir.path().join("other");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(other_dir.join("marker.txt"), "explicit").unwrap();

        let other_dir = std::fs::canonicalize(&other_dir).unwrap();
        let tool = ShellExecTool::new().with_default_cwd(ws_dir);
        let result = tool
            .execute(serde_json::json!({
                "argv": ["cat", "marker.txt"],
                "cwd": other_dir.to_str().unwrap()
            }))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("explicit"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_exec_default_cwd_with_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let ws_dir = root.join("workspace");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("marker.txt"), "sandboxed-home").unwrap();

        let tool = ShellExecTool::sandboxed(root.clone()).with_default_cwd(ws_dir);

        // Default cwd should be workspace dir
        let result = tool
            .execute(serde_json::json!({"argv": ["cat", "marker.txt"]}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .contains("sandboxed-home")
        );

        // Explicit cwd outside sandbox should be rejected
        let result = tool
            .execute(serde_json::json!({"argv": ["ls"], "cwd": "/etc"}))
            .await;
        assert!(result.is_err());
    }

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
}
