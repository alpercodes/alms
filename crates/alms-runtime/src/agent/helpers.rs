use alms_core::AlmsError;

/// Check whether a tool result value indicates success.
///
/// Most tools return plain JSON with no `exit_code` field, which is
/// treated as success.  `shell_exec` is the exception: it embeds an
/// `exit_code` integer in the result JSON.  A non-zero exit code is
/// treated as a failure even though the tool call itself succeeded
/// (the error is semantic, not structural).
///
/// This helper centralises the check so `agent_loop` and
/// `execute_tool_call` do not need to know about `shell_exec`'s
/// return shape directly (addresses #368 feedback point 6).
pub(crate) fn tool_result_ok(value: &serde_json::Value) -> bool {
    value
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .is_none_or(|code| code == 0)
}

/// Produce a safe error description for session history.
///
/// Strips details that could contain secrets (API keys, URLs, headers)
/// while preserving the error category so retries have useful context.
pub(crate) fn sanitize_error_for_session(err: &AlmsError) -> String {
    match err {
        AlmsError::Runtime(msg) => {
            // Runtime errors may contain API URLs, keys, or raw HTTP details.
            if msg.contains("401") || msg.contains("403") {
                "LLM authentication error".to_string()
            } else if msg.contains("429") {
                "LLM rate limit exceeded".to_string()
            } else if msg.contains("timeout") || msg.contains("timed out") {
                "LLM request timed out".to_string()
            } else if msg.contains("context") || msg.contains("summary") {
                "Context building failed".to_string()
            } else {
                "Runtime error".to_string()
            }
        }
        AlmsError::ToolExecution(msg) => {
            // Tool name is safe, but output may contain secrets.
            let safe = msg.split(':').next().unwrap_or("unknown tool");
            format!("Tool execution failed: {safe}")
        }
        AlmsError::SessionNotFound(_) => "Session not found".to_string(),
        AlmsError::InvalidConfig(_) => "Invalid configuration".to_string(),
        AlmsError::Cancelled => "Run cancelled by user".to_string(),
        AlmsError::Io(_) => "I/O error".to_string(),
        _ => "Internal error".to_string(),
    }
}
