// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

/// Result type for sandbox operations
pub type SandboxResult<T> = Result<T, SandboxError>;

/// Errors that can occur in the sandbox
#[derive(Error, Debug)]
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

    /// A subagent's `AgentRuntime` returned a structured `AlmsError`
    /// (typically [`alms_core::AlmsError::SubagentLlmError`] when the
    /// downstream provider returned a non-success HTTP status).
    ///
    /// Carries the inner error verbatim so [`crate::ToolRegistry`]'s
    /// caller (`alms-runtime::tools::ToolRegistry::execute`) can recover
    /// the typed variant via the `From` impl below and propagate it to
    /// the parent agent's `tool_result` as a single tractable line —
    /// instead of the legacy 4-prefix wrap from
    /// `SandboxError::Io(format!("Subagent error: {}", e))` plus
    /// `AlmsError::ToolExecution(format!("Tool execution failed: {}", e))`.
    /// Issue #920.
    #[error("{0}")]
    Subagent(Box<alms_core::AlmsError>),
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
    /// Wrap an `AlmsError` so it survives the `SandboxResult` boundary
    /// without lossy stringification. The `alms-runtime` tool registry's
    /// catch-all unwraps `Subagent` back into the typed `AlmsError` so
    /// the structured variant reaches the parent agent's tool-result
    /// message intact. Issue #920.
    fn from(err: alms_core::AlmsError) -> Self {
        SandboxError::Subagent(Box::new(err))
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
