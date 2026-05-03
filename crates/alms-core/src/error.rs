use crate::run::ToolCallRecord;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AlmsError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Duplicate agent name: {0}")]
    DuplicateName(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Tool execution failed: {0}")]
    ToolExecution(String),

    /// A tool call was blocked by a pre-execution guard (e.g. the shell risk
    /// classifier). Carries the structured `target` path so the runtime can
    /// surface it in SSE events, audit entries, and the approval/audit UI
    /// without re-parsing the stringified reason. Issue #758.
    #[error("{reason}")]
    ToolBlocked {
        reason: String,
        target: Option<String>,
    },

    #[error("Channel error: {0}")]
    Channel(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error("Sandbox error: {0}")]
    Sandbox(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Run cancelled")]
    Cancelled,

    /// Run was cancelled but partial tool call records are available.
    #[error("Run cancelled (with {n} tool call records)", n = tool_calls.len())]
    CancelledWithToolCalls { tool_calls: Vec<ToolCallRecord> },

    /// Run failed but partial tool call records are available.
    #[error("{source}")]
    FailedWithToolCalls {
        source: Box<AlmsError>,
        tool_calls: Vec<ToolCallRecord>,
    },
}

pub type AlmsResult<T> = Result<T, AlmsError>;

/// Produce a safe error description for session history.
///
/// Strips details that could contain secrets (API keys, URLs, headers,
/// raw provider response bodies) while preserving the error category so
/// retries and follow-up turns ("why did that fail?") still have useful
/// context.
///
/// Used by both the runtime's per-run failure path
/// (`alms_runtime::agent::AgentRuntime::run`) and the gateway's
/// lifecycle-layer error marker persistence
/// (`alms_gateway::runs::lifecycle`) so every error that reaches session
/// history — and therefore the LLM context on subsequent turns — passes
/// through the same sanitiser. See issues #874 and #911.
pub fn sanitize_error_for_session(err: &AlmsError) -> String {
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
        // The classifier `reason` is public info — surface a distinct label
        // so operators grepping session history can tell policy denials
        // apart from generic internal errors.
        AlmsError::ToolBlocked { .. } => "Tool blocked by policy".to_string(),
        _ => "Internal error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_runtime_auth_strips_url_and_keys() {
        let err = AlmsError::Runtime(
            "HTTP 401 Unauthorized at https://api.example.com (authorization: Bearer sk-test-12345)"
                .into(),
        );
        let sanitized = sanitize_error_for_session(&err);
        assert_eq!(sanitized, "LLM authentication error");
        assert!(
            !sanitized.contains("sk-test-12345"),
            "API key must not survive sanitisation"
        );
        assert!(
            !sanitized.contains("api.example.com"),
            "Hostname must not survive sanitisation"
        );
    }

    #[test]
    fn sanitize_runtime_rate_limit() {
        let err = AlmsError::Runtime("429 Too Many Requests".into());
        assert_eq!(sanitize_error_for_session(&err), "LLM rate limit exceeded");
    }

    #[test]
    fn sanitize_runtime_timeout() {
        let err = AlmsError::Runtime("request timed out after 60s".into());
        assert_eq!(sanitize_error_for_session(&err), "LLM request timed out");
    }

    #[test]
    fn sanitize_runtime_generic_strips_secrets() {
        // A raw runtime error containing an API-key-like token must not
        // survive sanitisation — this is the regression test for #911.
        let err = AlmsError::Runtime(
            "provider 500 internal error: secret-key=sk-test-12345 leaked in body".into(),
        );
        let sanitized = sanitize_error_for_session(&err);
        assert_eq!(sanitized, "Runtime error");
        assert!(
            !sanitized.contains("sk-test-12345"),
            "API key must not survive sanitisation: got {sanitized:?}"
        );
    }

    #[test]
    fn sanitize_tool_strips_output() {
        let err = AlmsError::ToolExecution("shell_exec: secret output here".into());
        assert_eq!(
            sanitize_error_for_session(&err),
            "Tool execution failed: shell_exec"
        );
    }

    #[test]
    fn sanitize_runtime_context_building() {
        let err = AlmsError::Runtime("failed to build context window".into());
        assert_eq!(sanitize_error_for_session(&err), "Context building failed");
    }

    #[test]
    fn sanitize_cancelled() {
        assert_eq!(
            sanitize_error_for_session(&AlmsError::Cancelled),
            "Run cancelled by user"
        );
    }

    #[test]
    fn sanitize_tool_blocked() {
        assert_eq!(
            sanitize_error_for_session(&AlmsError::ToolBlocked {
                reason: "rm -rf / blocked".to_string(),
                target: None,
            }),
            "Tool blocked by policy"
        );
    }

    /// Regression test for #911: a Runtime error whose `Display` output
    /// embeds an absolute filesystem path (cwd, home dir, internal config
    /// paths, etc.) must not leak that path into the session-persisted
    /// form. The sanitiser collapses every Runtime error to one of a
    /// fixed set of category labels, so any absolute path baked into the
    /// underlying message is dropped on the way to session history /
    /// LLM context.
    #[test]
    fn sanitize_runtime_strips_absolute_file_paths() {
        // Cover both POSIX-style and Windows-style absolute paths so the
        // test exercises the sanitiser's "drop the whole message body"
        // contract rather than any platform-specific escape.
        let cases = [
            "internal error reading /home/alper/.config/alms/secrets.toml",
            "internal error reading /etc/alms/config.toml entry api_key=sk-test-12345",
            "internal error reading C:\\Users\\Alper\\AppData\\Local\\alms\\auth.token",
        ];

        for raw in cases {
            let err = AlmsError::Runtime(raw.to_string());
            let sanitized = sanitize_error_for_session(&err);
            assert_eq!(
                sanitized, "Runtime error",
                "Runtime errors must collapse to category label, got {sanitized:?} for raw {raw:?}"
            );
            for needle in [
                "/home/",
                "/etc/",
                "C:\\Users",
                ".config/",
                "secrets.toml",
                "auth.token",
                "sk-test-12345",
            ] {
                assert!(
                    !sanitized.contains(needle),
                    "absolute path or secret {needle:?} must not survive sanitisation: got {sanitized:?} (raw: {raw:?})"
                );
            }
        }
    }

    /// Regression test for #911: a Runtime error containing a long
    /// stack-trace-shaped body (multi-line, internal frame addresses,
    /// arbitrary length) must not flood the session / LLM context. The
    /// sanitiser's category-label contract bounds the persisted form to
    /// a short fixed string regardless of how large the underlying
    /// message is, so a multi-KB trace ends up as a 13-byte label.
    #[test]
    fn sanitize_runtime_strips_long_stack_traces() {
        // Build a long, stack-trace-shaped body (not a real backtrace —
        // we just need something multi-line and arbitrarily large that a
        // panic-converted-to-error or a provider 500 echo could plausibly
        // contain).
        let mut trace = String::from("internal panic: something went wrong\n");
        for i in 0..200 {
            trace.push_str(&format!(
                "  at frame_{i} in /home/alper/dev/alms/crates/alms-runtime/src/agent/loop_impl.rs:{}\n",
                100 + i
            ));
        }
        assert!(
            trace.len() > 5000,
            "test fixture should be large enough to demonstrate the bound"
        );

        let err = AlmsError::Runtime(trace.clone());
        let sanitized = sanitize_error_for_session(&err);

        // The sanitised form is a short fixed category label, regardless
        // of input size.
        assert_eq!(sanitized, "Runtime error");
        assert!(
            sanitized.len() < 64,
            "sanitised form must be bounded to a category label, got {} bytes",
            sanitized.len()
        );
        // None of the noisy frame contents survive.
        for needle in [
            "frame_0",
            "frame_199",
            "loop_impl.rs",
            "/home/alper",
            "internal panic",
        ] {
            assert!(
                !sanitized.contains(needle),
                "stack-trace fragment {needle:?} must not survive sanitisation: got {sanitized:?}"
            );
        }
    }
}
