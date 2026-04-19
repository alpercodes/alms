use thiserror::Error;

/// Result type for sandbox operations
pub type SandboxResult<T> = Result<T, SandboxError>;

/// Errors that can occur in the sandbox
#[derive(Error, Debug, Clone)]
pub enum SandboxError {
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool already registered: {0}")]
    ToolAlreadyExists(String),

    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),

    #[error("Invalid result: {0}")]
    InvalidResult(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Sandbox violation: {0}")]
    SandboxViolation(String),

    /// A shell command was blocked by the built-in risk classifier.
    ///
    /// Carries the structured `target` path (when the parser extracted one
    /// from the command, e.g. `rm -rf /etc/passwd` → `Some("/etc/passwd")`)
    /// so the UI / audit log / approval panel can surface *what* was targeted,
    /// not just that something was blocked. `None` for findings without a
    /// specific target (fork bombs, `curl | sh`, etc.). Issue #758.
    #[error("{reason}")]
    ShellBlocked {
        reason: String,
        target: Option<String>,
    },

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<serde_json::Error> for SandboxError {
    fn from(err: serde_json::Error) -> Self {
        SandboxError::Serialization(err.to_string())
    }
}

impl From<std::io::Error> for SandboxError {
    fn from(err: std::io::Error) -> Self {
        SandboxError::Io(err.to_string())
    }
}

impl From<reqwest::Error> for SandboxError {
    fn from(err: reqwest::Error) -> Self {
        SandboxError::Http(err.to_string())
    }
}

impl From<alms_core::AlmsError> for SandboxError {
    fn from(err: alms_core::AlmsError) -> Self {
        SandboxError::Internal(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = SandboxError::ToolNotFound("test_tool".to_string());
        assert_eq!(err.to_string(), "Tool not found: test_tool");

        let err = SandboxError::SandboxViolation("path traversal".to_string());
        assert!(err.to_string().contains("Sandbox violation"));
    }
}
