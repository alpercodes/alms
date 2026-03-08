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

    #[error("WASM compilation failed: {0}")]
    WasmCompile(String),

    #[error("WASM instantiation failed: {0}")]
    WasmInstantiate(String),

    #[error("WASM execution failed: {0}")]
    WasmExecution(String),

    #[error("WASM function not found: {0}")]
    WasmFunctionNotFound(String),

    #[error("Memory limit exceeded: allocated {allocated} bytes, limit {limit} bytes")]
    MemoryLimitExceeded { allocated: usize, limit: usize },

    #[error("Execution timeout after {0:?}")]
    ExecutionTimeout(std::time::Duration),

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

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<wasmtime::Error> for SandboxError {
    fn from(err: wasmtime::Error) -> Self {
        let msg = err.to_string();
        // Check for common sandbox violations
        if msg.contains("memory") && msg.contains("out of bounds") {
            SandboxError::SandboxViolation("Memory access out of bounds".to_string())
        } else if msg.contains("gas") || msg.contains("fuel") {
            SandboxError::SandboxViolation("Execution limit exceeded".to_string())
        } else {
            SandboxError::WasmExecution(msg)
        }
    }
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

        let err = SandboxError::MemoryLimitExceeded {
            allocated: 100,
            limit: 50,
        };
        assert!(err.to_string().contains("Memory limit exceeded"));
    }

    #[test]
    fn test_wasmtime_error_conversion() {
        // Create a fake wasmtime error by creating a string-based error
        let wasm_err = wasmtime::Error::msg("memory out of bounds access");
        let sandbox_err: SandboxError = wasm_err.into();

        match sandbox_err {
            SandboxError::SandboxViolation(msg) => {
                assert!(msg.contains("Memory access"));
            }
            _ => panic!("Expected SandboxViolation"),
        }
    }
}
