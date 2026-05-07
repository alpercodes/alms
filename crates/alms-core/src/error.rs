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

    /// LLM provider returned a non-success HTTP status while serving a
    /// subagent's call. Carries the structured triple (provider, status,
    /// raw response body) so callers can render a one-line, diagnosable
    /// message instead of stringifying-and-rewrapping at every boundary
    /// from the provider client back to the parent agent's tool result.
    ///
    /// Issue #920. The `Display` impl renders as
    /// `Subagent LLM error ({provider} {status}): {body}`, a single
    /// human-readable line — *not* `Runtime error: Runtime error: ...`.
    /// The triple is preserved verbatim through the SubagentDispatcher
    /// boundary, the coordinator's `TaskResult`, the `invoke_agent` tool,
    /// and `ToolRegistry::execute`'s catch-all so the parent agent's
    /// `tool_result` message reads as a single tractable line.
    #[error("Subagent LLM error ({provider} {status}): {body}")]
    SubagentLlmError {
        provider: String,
        status: u16,
        body: String,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    // Note for #995 follow-up reviewers: every `SubagentLlmError` value
    // should be constructed via [`AlmsError::subagent_llm_error`] rather
    // than direct struct-literal syntax. The constructor normalises
    // newlines in `body` so the `Display` impl above stays a single
    // tractable line even when providers (notably Gemini) return
    // multi-line JSON error bodies. Issue #920 / Tim's PR #995 review.
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

impl AlmsError {
    /// Construct an [`AlmsError::SubagentLlmError`] with the body
    /// normalised so the `Display` impl renders as a single line.
    ///
    /// The variant's `Display` is documented as a single tractable line of
    /// the shape `Subagent LLM error ({provider} {status}): {body}`. That
    /// guarantee can only hold if `body` itself contains no line breaks —
    /// some providers (notably Gemini) return JSON error bodies that span
    /// multiple lines, which would otherwise smear the rendered error
    /// across audit logs, the `tool_result` parent message, and SSE
    /// `tool_end` payloads.
    ///
    /// This constructor replaces every `\r\n`, `\n`, and bare `\r`
    /// occurrence in `body` with a single ASCII space so the rendered
    /// line stays grep-friendly. The substitution is intentionally
    /// non-recoverable — operators reading audit logs get a sane
    /// single-line shape; if the original raw body is needed, the
    /// underlying provider response is logged separately at the call
    /// site (`error!("LLM API error: {} - {}", status, error_text)` in
    /// `llm_client::mod.rs`).
    ///
    /// All emission sites (`LlmClient::complete`, `complete_stream`, and
    /// their cache-retry branches) MUST go through this constructor so
    /// the single-line invariant holds at every boundary. Tests in this
    /// file pin the contract — see
    /// `subagent_llm_error_constructor_normalises_newlines`. Issue #920 /
    /// PR #995 polish.
    pub fn subagent_llm_error(
        provider: impl Into<String>,
        status: u16,
        body: impl Into<String>,
    ) -> Self {
        let raw = body.into();
        // Single pass: collapse CR/LF into spaces. We deliberately do not
        // collapse runs of whitespace — that would mutate the body's
        // internal structure beyond the line-shape guarantee.
        let normalised: String = raw
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        AlmsError::SubagentLlmError {
            provider: provider.into(),
            status,
            body: normalised,
        }
    }
}

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
        // Subagent LLM errors carry the raw provider response body, which
        // can echo prompts, snippets of model output, or other
        // potentially sensitive content. Categorise by HTTP status so
        // session history reflects the failure class without persisting
        // the body. Issue #920.
        AlmsError::SubagentLlmError { status, .. } => match *status {
            401 | 403 => "Subagent LLM authentication error".to_string(),
            429 => "Subagent LLM rate limit exceeded".to_string(),
            400..=499 => "Subagent LLM request rejected".to_string(),
            500..=599 => "Subagent LLM server error".to_string(),
            _ => "Subagent LLM error".to_string(),
        },
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

    /// #920: the structured subagent LLM error variant must render as a
    /// single human-readable line — no `Runtime error:` prefix, no
    /// `IO error: Subagent error:` prefix, no `Tool execution failed:`
    /// prefix. The 4-level wrap from before this issue produced
    /// `Tool execution failed: IO error: Subagent error: Runtime error:
    /// Runtime error: LLM API error: 400 - {body}`. The new shape is
    /// just `Subagent LLM error (anthropic 400): {body}`.
    #[test]
    fn subagent_llm_error_display_is_one_line_no_layer_prefixes() {
        let err = AlmsError::SubagentLlmError {
            provider: "anthropic".to_string(),
            status: 400,
            body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 270000 tokens > 262144 maximum"}}"#.to_string(),
        };
        let s = err.to_string();
        assert!(
            s.starts_with("Subagent LLM error (anthropic 400):"),
            "expected single-line structured Display, got: {s}"
        );
        // The legacy 4-prefix wrap that this variant collapses must not
        // reappear for any reason.
        for forbidden in [
            "Runtime error:",
            "IO error: Subagent error:",
            "Tool execution failed:",
            // The doubled prefix from the legacy stringify-then-rewrap
            // on the coordinator boundary.
            "Runtime error: Runtime error:",
            "LLM API error:",
        ] {
            assert!(
                !s.contains(forbidden),
                "structured Display must not contain legacy prefix {forbidden:?}, got: {s}"
            );
        }
        // The body — including the actual provider message — survives
        // verbatim so callers can render it.
        assert!(
            s.contains("prompt is too long"),
            "raw provider message must survive in body, got: {s}"
        );
    }

    /// #920: sanitiser must categorise the structured variant by HTTP
    /// status without persisting the raw provider response body, which
    /// can echo prompts or other sensitive content into session
    /// history. Mirrors the existing `Runtime` sanitiser contract.
    #[test]
    fn sanitize_subagent_llm_error_strips_body_keeps_status_class() {
        let cases = [
            (400, "Subagent LLM request rejected"),
            (401, "Subagent LLM authentication error"),
            (403, "Subagent LLM authentication error"),
            (429, "Subagent LLM rate limit exceeded"),
            (500, "Subagent LLM server error"),
            (503, "Subagent LLM server error"),
        ];
        for (status, expected) in cases {
            let err = AlmsError::SubagentLlmError {
                provider: "anthropic".to_string(),
                status,
                body: "secret payload with api-key=sk-test-12345 and prompt fragments".to_string(),
            };
            let s = sanitize_error_for_session(&err);
            assert_eq!(s, expected, "status {status} sanitised to wrong label");
            assert!(
                !s.contains("sk-test-12345"),
                "API key must not survive sanitisation for status {status}, got: {s}"
            );
            assert!(
                !s.contains("prompt fragments"),
                "raw body must not survive sanitisation for status {status}, got: {s}"
            );
        }
    }

    /// #920 / PR #995 polish: the `subagent_llm_error` constructor
    /// guarantees that the resulting `Display` is a single line, even
    /// when the provider returns a multi-line body (e.g. pretty-printed
    /// JSON from Gemini). The constructor replaces `\n`, `\r\n`, and
    /// bare `\r` with spaces; downstream renderers (audit log,
    /// `tool_result`, SSE `tool_end`) get a grep-friendly line.
    #[test]
    fn subagent_llm_error_constructor_normalises_newlines() {
        // Multi-line body covering all three line-ending shapes:
        // bare LF (Unix), CRLF (Windows / HTTP), and bare CR (legacy
        // Mac / mid-string).
        let body = "{\n  \"error\": {\r\n    \"message\": \"prompt is too long\"\r  }\n}";
        let err = AlmsError::subagent_llm_error("gemini", 400, body);

        let s = err.to_string();
        // Hard contract: no line breaks in the rendered Display.
        assert!(
            !s.contains('\n'),
            "Display must not contain LF after constructor normalisation: {s:?}"
        );
        assert!(
            !s.contains('\r'),
            "Display must not contain CR after constructor normalisation: {s:?}"
        );
        // Display still starts with the expected one-line prefix.
        assert!(
            s.starts_with("Subagent LLM error (gemini 400):"),
            "expected single-line prefix, got: {s}"
        );
        // The body's actual provider message survives — only line
        // breaks were substituted.
        assert!(
            s.contains("prompt is too long"),
            "provider message must survive normalisation, got: {s}"
        );
        // Pin the exact shape: each line break is exactly one ASCII
        // space (so `\r\n` becomes two spaces — one per char), no
        // collapsing of internal whitespace, no truncation.
        //
        // Input was: `{\n  "error": {\r\n    "message": "..."\r  }\n}`
        // After substitution: `{` + ` ` (\n) + `  ` (existing indent)
        // + `"error": {` + `  ` (\r\n) + `    ` + `"message": "..."`
        // + ` ` (\r) + `  }` + ` ` (\n) + `}`.
        match &err {
            AlmsError::SubagentLlmError { body, .. } => {
                assert_eq!(
                    body, "{   \"error\": {      \"message\": \"prompt is too long\"   } }",
                    "newline-to-space substitution must be 1:1, no whitespace collapsing"
                );
            }
            other => panic!("expected SubagentLlmError, got {other:?}"),
        }
    }

    /// PR #995 polish: a body that already contains no line breaks
    /// must round-trip through the constructor unchanged. Guards
    /// against the constructor accidentally mutating well-formed
    /// single-line bodies (the common 99% case).
    #[test]
    fn subagent_llm_error_constructor_preserves_single_line_body() {
        let raw = r#"{"error":{"message":"prompt is too long: 270000 tokens > 262144 maximum"}}"#;
        let err = AlmsError::subagent_llm_error("anthropic", 400, raw);
        match &err {
            AlmsError::SubagentLlmError { body, .. } => {
                assert_eq!(body, raw, "single-line body must round-trip unchanged");
            }
            other => panic!("expected SubagentLlmError, got {other:?}"),
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
