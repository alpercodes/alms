// SPDX-License-Identifier: Apache-2.0

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

    /// LLM provider returned a non-success HTTP status. Raised by
    /// `LlmClient::complete` / `complete_stream` for *every* LLM call —
    /// a top-level chat run, a scheduled job, a DM turn, or a subagent's
    /// loop. Carries the structured triple (configured provider name,
    /// status, raw response body) so callers can render a one-line,
    /// diagnosable message instead of stringifying-and-rewrapping at
    /// every boundary.
    ///
    /// `provider` is the operator-facing provider key — `[llm].provider`
    /// or the `[llm.providers.<name>]` entry name (`openrouter`,
    /// `anthropic`, a custom entry) — **not** the wire-protocol family.
    /// It therefore matches `resolved_config.provider` on the run record
    /// and names the entry whose key needs fixing (#162).
    ///
    /// History: introduced by #920 as `SubagentLlmError`, because the
    /// motivating trace was a subagent's 400 arriving at the parent as a
    /// 4-prefix wrap. The client raises it for every call, though, so the
    /// "Subagent" label was wrong on the most common path (an operator's
    /// own chat run) and the wire-family label (`openai`) was wrong for
    /// every OpenAI-compatible provider; #162 renamed it. The `Display`
    /// impl renders as `LLM error ({provider} {status}): {body}`, a
    /// single human-readable line — *not* `Runtime error: Runtime error:
    /// ...`. The triple is preserved verbatim through the
    /// SubagentDispatcher boundary, the coordinator's `TaskResult`, the
    /// `invoke_agent` tool, and `ToolRegistry::execute`'s catch-all, so
    /// when the failing call *was* a subagent's, the parent agent's
    /// `tool_result` for `invoke_agent` still reads as one tractable line
    /// — the tool-result envelope is what frames it as the subagent's.
    #[error("LLM error ({provider} {status}): {body}")]
    LlmApiError {
        provider: String,
        status: u16,
        body: String,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    // Note for #995 follow-up reviewers: every `LlmApiError` value
    // should be constructed via [`AlmsError::llm_api_error`] rather
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
    /// Construct an [`AlmsError::LlmApiError`] with the body
    /// normalised so the `Display` impl renders as a single line.
    ///
    /// `provider` must be the configured provider key (`LlmConfig::provider`
    /// — `openrouter`, `anthropic`, a custom `[llm.providers.<name>]`
    /// entry), not the wire-protocol family; see the variant docs (#162).
    ///
    /// The variant's `Display` is documented as a single tractable line of
    /// the shape `LLM error ({provider} {status}): {body}`. That
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
    /// `provider` gets the same treatment via [`normalise_provider_label`]
    /// — newlines collapsed, trimmed, length-clamped, empty replaced —
    /// because since #162 it is `LlmConfig::provider`, which the agent API
    /// accepts with only trim-and-reject-empty, and it now reaches the
    /// sanitiser's fixed-shape output.
    ///
    /// All emission sites (`LlmClient::complete`, `complete_stream`, and
    /// their cache-retry branches) MUST go through this constructor so
    /// the single-line invariant holds at every boundary. Tests in this
    /// file pin the contract — see
    /// `llm_api_error_constructor_normalises_newlines` and
    /// `llm_api_error_constructor_normalises_provider_label`. Issue #920 /
    /// PR #995 polish; #162.
    pub fn llm_api_error(
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
        AlmsError::LlmApiError {
            provider: normalise_provider_label(provider.into()),
            status,
            body: normalised,
        }
    }
}

/// Upper bound on the `provider` label carried by [`AlmsError::LlmApiError`].
///
/// Real provider names are `[llm.providers.<name>]` keys — a few characters
/// — but the per-agent `provider` override arrives through `POST /agents` /
/// `PATCH /agents/{id}` with only trim-and-reject-empty applied, so an
/// arbitrary string can reach the label. 64 is generous for a real name
/// and small enough to keep a persisted marker one tractable line.
const MAX_PROVIDER_LABEL_CHARS: usize = 64;

/// Bring a configured provider name into label shape (#162; Tim's #167
/// review).
///
/// The name lands in `Display` (run record, SSE `run_error`, the parent's
/// `tool_result`) and — on the 401/403 arm — in
/// [`sanitize_error_for_session`]'s output, the one surface designed to be
/// fixed-shape. `body` has been newline-normalised since #995 for exactly
/// that reason; `provider` was a `&'static str` from a closed set of three
/// until #162 made it `LlmConfig::provider`, which the agent API does not
/// validate (its neighbour `summary_provider` is checked against the live
/// providers map; the plain override is not). So: CR/LF become spaces, the
/// result is trimmed, clamped to [`MAX_PROVIDER_LABEL_CHARS`] with a
/// trailing `…` so truncation is visible, and an empty name becomes
/// `unknown` rather than rendering `LLM error ( 401)`.
fn normalise_provider_label(raw: String) -> String {
    let collapsed: String = raw
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    let mut chars = trimmed.chars();
    let clamped: String = chars.by_ref().take(MAX_PROVIDER_LABEL_CHARS).collect();
    if chars.next().is_some() {
        format!("{clamped}…")
    } else {
        clamped
    }
}

/// Produce a safe error description for the audit log.
///
/// The audit log records every tool execution with its decision and any
/// error string. Most `AlmsError` variants are safe to render verbatim
/// there — they carry operator-authored category labels (e.g.
/// `Tool execution failed: ...`, `Runtime error: ...`) that aid
/// debuggability and do not echo provider response bodies.
///
/// The exception is [`AlmsError::LlmApiError`], whose `Display`
/// embeds the raw provider response `body` (preserved deliberately for
/// in-context rendering of subagent failures back to the parent agent's
/// tool result, per issue #920). That body can contain prompt fragments,
/// snippets of model output, API-key-shaped tokens echoed back by buggy
/// providers, or other potentially sensitive content. Persisting it
/// verbatim into the audit log is the leak class Tim flagged on PR #995
/// and tracked in issue #997.
///
/// This helper performs a per-variant dispatch:
///
/// - `LlmApiError` → the same status-class category label used by
///   [`sanitize_error_for_session`] (e.g. `"LLM request rejected"`), so
///   audit-log emissions and session-history persistence agree on the
///   redacted shape.
/// - Every other variant → `err.to_string()`, the existing pre-#997
///   shape, preserving the operator-debuggable detail audit-log
///   consumers depend on.
///
/// The audit-log JSON wire shape is unchanged — only the `error` string
/// content shrinks for the one targeted variant. Operator grep / dashboard
/// queries that match on category-label prefixes (e.g. `"Tool execution
/// failed:"`, `"Runtime error:"`) keep working byte-for-byte, and queries
/// that previously matched the raw provider body for `LlmApiError`
/// audit rows now match the category label instead.
///
/// If a future variant grows a sensitive-payload field (e.g. a hypothetical
/// `ProviderRateLimitError { body }`), add it to the dispatch here and to
/// [`sanitize_error_for_session`] in lockstep so both the audit and
/// session-history paths agree.
///
/// Issue #997. Follow-up to #920 / PR #995.
pub fn audit_error_string(err: &AlmsError) -> String {
    match err {
        // The single leak class from Tim's review: route through the
        // same status-class label used by session-history sanitisation
        // so the audit log, session history, and SSE error markers all
        // agree on the redacted shape for LLM provider failures.
        AlmsError::LlmApiError { .. } => sanitize_error_for_session(err),
        // `FailedWithToolCalls` wraps another `AlmsError` and its `Display`
        // delegates to `{source}`. Recurse so an `LlmApiError` source
        // is still redacted if a future audit emission stringifies a
        // wrapped error. Today no audit site reaches this arm — the
        // variant is constructed at `agent::mod.rs:1114` *after* the
        // run-loop's audit rows are already written — but pinning the
        // recursive contract means an accidental future emission of
        // `audit_error_string(&FailedWithToolCalls { source: LlmApiError })`
        // can't silently leak the body through the catch-all arm. Tim's
        // PR #1006 review.
        AlmsError::FailedWithToolCalls { source, .. } => audit_error_string(source),
        // Every other variant is operator-authored (category labels, tool
        // names, classifier reasons) or already redacted at construction
        // (`AlmsError::ToolExecution(format!("Tool '{name}' not allowed"))`).
        // Pass through verbatim — preserves the pre-#997 audit-log shape
        // and the debuggability operators rely on.
        //
        // WARNING: any new variant whose `Display` embeds an unredacted,
        // potentially sensitive payload (provider response body, raw
        // headers, secret material, etc.) MUST be added to the explicit
        // dispatch above rather than falling through here. The catch-all
        // is `to_string()`, so a sensitive payload baked into a new
        // variant's `#[error]` template would silently land in audit
        // rows. Mirror the addition in `sanitize_error_for_session` so
        // the audit and session-history surfaces stay in lockstep.
        _ => err.to_string(),
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
            } else if msg.contains("maximum") && msg.contains("iteration") {
                // Agent-loop iteration cap (#987 / B3). The message is
                // self-authored and secret-free; surface a distinct label so
                // operators (and the DM peer's notification) can tell a wedged
                // tool loop apart from a generic internal error.
                "Agent stopped after reaching its iteration limit".to_string()
            } else if msg.contains("maximum duration") {
                // Agent-loop absolute wall-clock backstop (#987 / B3 / #1150).
                // Same rationale.
                "Agent stopped after reaching its time limit".to_string()
            } else if msg.contains("stalled") {
                // Phase-aware inactivity timeout (#1150). The message is
                // self-authored ("agent run stalled -- no activity for {n}s
                // during {phase}") and secret-free; surface a distinct label
                // so a stalled (no-progress) run reads differently from the
                // absolute-duration backstop above and the generic error.
                "Agent stopped after stalling (no activity)".to_string()
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
        // LLM API errors carry the raw provider response body, which can
        // echo prompts, snippets of model output, or API-key-shaped
        // tokens. Categorise by HTTP status so session history reflects
        // the failure class without persisting the body (#920). Only the
        // `provider` field is surfaced, and only on the auth arm: it is
        // the configured provider key (`[llm].provider` or a per-agent
        // override, brought into label shape by the constructor), never
        // provider output, and it is what tells the operator WHICH key
        // to fix (#162).
        AlmsError::LlmApiError {
            provider, status, ..
        } => match *status {
            401 | 403 => llm_auth_error_hint(provider, *status),
            429 => "LLM rate limit exceeded".to_string(),
            400..=499 => "LLM request rejected".to_string(),
            500..=599 => "LLM server error".to_string(),
            _ => "LLM error".to_string(),
        },
        // The classifier `reason` is public info — surface a distinct label
        // so operators grepping session history can tell policy denials
        // apart from generic internal errors.
        AlmsError::ToolBlocked { .. } => "Tool blocked by policy".to_string(),
        _ => "Internal error".to_string(),
    }
}

/// Session-history label for an LLM 401/403: names the provider whose key
/// was rejected and where that key is set (#162).
///
/// Two audiences read this string, and the second shapes it more than the
/// first. It renders as a web-chat bubble (`[Run failed: …]`), and it is
/// also rebuilt into the agent's own context on the next turn
/// (`context/error_markers.rs`), so it sits in a model's prompt. That is
/// why it is descriptive — a place to look — and never an imperative
/// command: an agent with the shell tool, asked to fix the failure, could
/// run `alms auth set <p> <key>` literally and store the string `<key>` as
/// the key (Tim's #167 review). The descriptive half is also the
/// actionable one — the bubble's reader is already in the dashboard.
///
/// Where the key lives differs by provider name, and naming a place that
/// rejects the name is worse than naming none:
///
/// - Names in [`crate::secrets::VALID_PROVIDERS`] — the five secrets-store
///   slots: `openai`, `anthropic`, `openrouter`, `gemini`, plus `telegram`,
///   a channel token no LLM call runs under — are what `PUT /auth/keys`
///   accepts. That is the dashboard's Settings key rows, live on a running
///   gateway, so the hint points there.
/// - Any other `[llm.providers.<name>]` entry is rejected by that route;
///   its key comes from the entry's own `api_key_env` / `api_key` in
///   `alms.toml`, which is read once at boot (`[llm.providers]` is
///   config-file-only, never mutable at runtime) — hence "restart".
///
/// A bare `OPENROUTER_API_KEY`-style export is deliberately NOT named:
/// startup detects and ignores it
/// (`AlmsConfig::warn_deprecated_secret_env_vars`).
///
/// The label keeps the `LLM authentication error` prefix the `Runtime`
/// arm above emits, so grep queries on the category keep matching.
fn llm_auth_error_hint(provider: &str, status: u16) -> String {
    if crate::secrets::VALID_PROVIDERS.contains(&provider) {
        format!(
            "LLM authentication error: {provider} rejected the API key (HTTP {status}). \
             Check the {provider} key in the dashboard Settings."
        )
    } else {
        format!(
            "LLM authentication error: {provider} rejected the API key (HTTP {status}). \
             Check the api_key_env / api_key declared under [llm.providers.{provider}] \
             in alms.toml and restart the gateway."
        )
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
    /// just `LLM error (anthropic 400): {body}`.
    #[test]
    fn llm_api_error_display_is_one_line_no_layer_prefixes() {
        let err = AlmsError::LlmApiError {
            provider: "anthropic".to_string(),
            status: 400,
            body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 270000 tokens > 262144 maximum"}}"#.to_string(),
        };
        let s = err.to_string();
        assert!(
            s.starts_with("LLM error (anthropic 400):"),
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
            // #162: the pre-rename label claimed a subagent on every
            // top-level run.
            "Subagent",
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
    /// The 401/403 rows pin the label's prefix only — the rest of that
    /// label is the #162 key hint, pinned by its own tests below.
    #[test]
    fn sanitize_llm_api_error_strips_body_keeps_status_class() {
        let cases = [
            (400, "LLM request rejected"),
            (401, "LLM authentication error"),
            (403, "LLM authentication error"),
            (429, "LLM rate limit exceeded"),
            (500, "LLM server error"),
            (503, "LLM server error"),
        ];
        for (status, expected) in cases {
            let err = AlmsError::LlmApiError {
                provider: "anthropic".to_string(),
                status,
                body: "secret payload with api-key=sk-test-12345 and prompt fragments".to_string(),
            };
            let s = sanitize_error_for_session(&err);
            if matches!(status, 401 | 403) {
                assert!(
                    s.starts_with(expected),
                    "status {status} sanitised to wrong label prefix, got: {s}"
                );
            } else {
                assert_eq!(s, expected, "status {status} sanitised to wrong label");
            }
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

    /// #162: a 401/403 must persist as a label that names the provider
    /// whose key was rejected and where that key is set — and must NOT
    /// persist the provider's response body, which is the leak class
    /// `sanitize_error_for_session` exists to close. Both halves are
    /// pinned; the omission half is the one that would catch a body
    /// leaking through a future rewrite of the hint.
    #[test]
    fn sanitize_llm_api_error_401_names_provider_and_settings_but_omits_body() {
        // OpenRouter's verbatim 401 body from the #162 trace, padded with
        // an API-key-shaped token and a URL so the omission half has teeth.
        let body = r#"{"error":{"message":"No cookie auth credentials found","code":401},"request":"https://openrouter.ai/api/v1/chat/completions","authorization":"Bearer sk-or-v1-test-12345"}"#;
        for status in [401u16, 403] {
            let err = AlmsError::llm_api_error("openrouter", status, body);
            let s = sanitize_error_for_session(&err);
            let http = format!("HTTP {status}");

            // What the persisted rendering must say: the category, the
            // provider, the credential, and the one place that accepts
            // `openrouter` (`PUT /auth/keys` behind the dashboard's
            // Settings, live on a running gateway).
            assert!(
                s.starts_with("LLM authentication error"),
                "category prefix must survive for grep compatibility, got: {s}"
            );
            for needle in [
                "openrouter rejected the API key",
                http.as_str(),
                "Check the openrouter key in the dashboard Settings",
            ] {
                assert!(
                    s.contains(needle),
                    "hint must contain {needle:?} for status {status}, got: {s}"
                );
            }

            // What it must omit.
            for needle in [
                // The provider body, in every recognisable piece.
                "No cookie auth credentials found",
                "cookie",
                "sk-or-v1-test-12345",
                "Bearer",
                "authorization",
                "https://",
                "openrouter.ai",
                r#"{"error""#,
                // The variant is raised for every LLM call; this run had
                // no subagent (#162 point 1).
                "Subagent",
                "subagent",
                // The wire family is not the configured provider (#162
                // point 2).
                "openai",
                // A bare env export is NOT a key source — startup
                // detects it and logs "IGNORED for security"
                // (`AlmsConfig::warn_deprecated_secret_env_vars`), so the hint
                // must not send anyone there.
                "OPENROUTER_API_KEY",
                "_API_KEY",
                "export ",
                // This marker is rebuilt into the agent's own context
                // (`context/error_markers.rs`), so it must never carry a
                // runnable command with a placeholder: an agent with the
                // shell tool could run it literally and store `<key>` as
                // the key (Tim's #167 review). Descriptive only.
                "alms auth set",
                "<key>",
                "restart",
            ] {
                assert!(
                    !s.contains(needle),
                    "persisted label must omit {needle:?} for status {status}, got: {s}"
                );
            }
        }
    }

    /// #162: every provider `PUT /auth/keys` accepts is pointed at the
    /// dashboard's Settings, spelled with its own name — and none of them
    /// gets a runnable command (see the prompt-hazard note in the test
    /// above). This list is the four LLM slots of `VALID_PROVIDERS`; it
    /// catches a removal from the constant.
    #[test]
    fn sanitize_llm_api_error_401_known_providers_point_at_settings_never_a_command() {
        for provider in ["openai", "anthropic", "openrouter", "gemini"] {
            let err = AlmsError::llm_api_error(provider, 401, "body");
            let s = sanitize_error_for_session(&err);
            let place = format!("Check the {provider} key in the dashboard Settings");
            assert!(
                s.contains(&place),
                "{provider} is in VALID_PROVIDERS and must be pointed at Settings, got: {s}"
            );
            assert!(
                s.contains(&format!("{provider} rejected the API key")),
                "hint must name the provider, got: {s}"
            );
            for needle in ["alms auth set", "<key>", "restart", "api_key_env"] {
                assert!(
                    !s.contains(needle),
                    "known-provider hint must omit {needle:?}, got: {s}"
                );
            }
        }
    }

    /// #162: a provider that is NOT in `VALID_PROVIDERS` (a custom
    /// `[llm.providers.<name>]` entry) is rejected by `PUT /auth/keys`, so
    /// the hint must point at the entry's own `api_key_env` / `api_key` in
    /// `alms.toml` — and must not name Settings, which would fail on the
    /// name, nor any runnable command.
    #[test]
    fn sanitize_llm_api_error_401_custom_provider_points_at_toml_entry() {
        let err = AlmsError::llm_api_error(
            "myproxy",
            401,
            r#"{"error":"invalid key sk-test-12345 for https://llm.internal"}"#,
        );
        let s = sanitize_error_for_session(&err);
        assert!(
            s.starts_with("LLM authentication error: myproxy rejected the API key (HTTP 401)"),
            "got: {s}"
        );
        for needle in [
            "[llm.providers.myproxy]",
            "api_key_env",
            "alms.toml",
            "restart the gateway",
        ] {
            assert!(s.contains(needle), "hint must contain {needle:?}, got: {s}");
        }
        for needle in [
            "alms auth set",
            "<key>",
            "Settings",
            "sk-test-12345",
            "invalid key",
            "llm.internal",
            "https://",
            "Subagent",
        ] {
            assert!(
                !s.contains(needle),
                "persisted label must omit {needle:?}, got: {s}"
            );
        }
    }

    /// Tim's #167 review: `provider` is `LlmConfig::provider`, which the
    /// agent API accepts with only trim-and-reject-empty, so a
    /// newline-bearing or oversized name can reach the label. The
    /// constructor must bring it into the same single-line shape `body`
    /// gets, and the 401 sanitiser output — the fixed-shape surface —
    /// must stay one line on every rendering.
    #[test]
    fn llm_api_error_constructor_normalises_provider_label() {
        // Line breaks collapse to spaces and the result is trimmed.
        let err = AlmsError::llm_api_error("open\nrouter\r\n", 401, "body");
        match &err {
            AlmsError::LlmApiError { provider, .. } => assert_eq!(provider, "open router"),
            other => panic!("expected LlmApiError, got {other:?}"),
        }
        for rendered in [
            err.to_string(),
            sanitize_error_for_session(&err),
            audit_error_string(&err),
        ] {
            assert!(
                !rendered.contains('\n') && !rendered.contains('\r'),
                "every rendering must be one line, got: {rendered:?}"
            );
        }

        // Oversized names are clamped, and visibly so.
        let long = "p".repeat(MAX_PROVIDER_LABEL_CHARS + 40);
        let err = AlmsError::llm_api_error(long.clone(), 401, "body");
        match &err {
            AlmsError::LlmApiError { provider, .. } => {
                assert!(
                    provider.starts_with(&"p".repeat(MAX_PROVIDER_LABEL_CHARS)),
                    "clamp must keep the first {MAX_PROVIDER_LABEL_CHARS} chars, got: {provider}"
                );
                assert!(
                    provider.ends_with('…'),
                    "truncation must be visible: {provider}"
                );
                assert_eq!(provider.chars().count(), MAX_PROVIDER_LABEL_CHARS + 1);
            }
            other => panic!("expected LlmApiError, got {other:?}"),
        }
        assert!(
            !sanitize_error_for_session(&err).contains(&long),
            "the unclamped name must not reach the persisted label"
        );

        // A name exactly at the bound is untouched.
        let exact = "p".repeat(MAX_PROVIDER_LABEL_CHARS);
        match AlmsError::llm_api_error(exact.clone(), 401, "body") {
            AlmsError::LlmApiError { provider, .. } => assert_eq!(provider, exact),
            other => panic!("expected LlmApiError, got {other:?}"),
        }

        // An empty name (unreachable via the API, which rejects it) still
        // renders a readable label instead of `LLM error ( 401)`.
        let err = AlmsError::llm_api_error("  ", 401, "body");
        assert!(
            err.to_string().starts_with("LLM error (unknown 401):"),
            "got: {err}"
        );
        assert!(
            sanitize_error_for_session(&err)
                .starts_with("LLM authentication error: unknown rejected the API key (HTTP 401)"),
            "got: {}",
            sanitize_error_for_session(&err)
        );
    }

    /// #162: the variant is raised for every LLM call — a top-level chat
    /// run, a job, a DM turn — so no rendering of it, on any status, may
    /// claim a subagent was involved. Covers all three surfaces that
    /// reach an operator: `Display` (run record / SSE), the session
    /// sanitiser (persisted marker), and the audit helper.
    #[test]
    fn llm_api_error_never_says_subagent_on_any_surface() {
        for status in [400u16, 401, 403, 404, 429, 500, 503, 599, 600] {
            let err = AlmsError::llm_api_error("openrouter", status, "body");
            for rendered in [
                err.to_string(),
                sanitize_error_for_session(&err),
                audit_error_string(&err),
            ] {
                assert!(
                    !rendered.to_lowercase().contains("subagent"),
                    "status {status}: {rendered}"
                );
            }
        }
    }

    /// #920 / PR #995 polish: the `llm_api_error` constructor
    /// guarantees that the resulting `Display` is a single line, even
    /// when the provider returns a multi-line body (e.g. pretty-printed
    /// JSON from Gemini). The constructor replaces `\n`, `\r\n`, and
    /// bare `\r` with spaces; downstream renderers (audit log,
    /// `tool_result`, SSE `tool_end`) get a grep-friendly line.
    #[test]
    fn llm_api_error_constructor_normalises_newlines() {
        // Multi-line body covering all three line-ending shapes:
        // bare LF (Unix), CRLF (Windows / HTTP), and bare CR (legacy
        // Mac / mid-string).
        let body = "{\n  \"error\": {\r\n    \"message\": \"prompt is too long\"\r  }\n}";
        let err = AlmsError::llm_api_error("gemini", 400, body);

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
            s.starts_with("LLM error (gemini 400):"),
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
            AlmsError::LlmApiError { body, .. } => {
                assert_eq!(
                    body, "{   \"error\": {      \"message\": \"prompt is too long\"   } }",
                    "newline-to-space substitution must be 1:1, no whitespace collapsing"
                );
            }
            other => panic!("expected LlmApiError, got {other:?}"),
        }
    }

    /// PR #995 polish: a body that already contains no line breaks
    /// must round-trip through the constructor unchanged. Guards
    /// against the constructor accidentally mutating well-formed
    /// single-line bodies (the common 99% case).
    #[test]
    fn llm_api_error_constructor_preserves_single_line_body() {
        let raw = r#"{"error":{"message":"prompt is too long: 270000 tokens > 262144 maximum"}}"#;
        let err = AlmsError::llm_api_error("anthropic", 400, raw);
        match &err {
            AlmsError::LlmApiError { body, .. } => {
                assert_eq!(body, raw, "single-line body must round-trip unchanged");
            }
            other => panic!("expected LlmApiError, got {other:?}"),
        }
    }

    /// #997: the audit-log helper redacts the `LlmApiError` body
    /// to the same status-class label `sanitize_error_for_session`
    /// uses, so the leak class Tim flagged on PR #995 (raw provider
    /// response body landing in the audit log verbatim) is closed.
    #[test]
    fn audit_error_string_redacts_llm_api_body() {
        let cases = [
            (400, "LLM request rejected"),
            (401, "LLM authentication error"),
            (403, "LLM authentication error"),
            (429, "LLM rate limit exceeded"),
            (500, "LLM server error"),
            (503, "LLM server error"),
        ];
        for (status, expected) in cases {
            let err = AlmsError::LlmApiError {
                provider: "anthropic".to_string(),
                status,
                body: "secret-key=sk-test-12345 prompt fragments leaked here".to_string(),
            };
            let s = audit_error_string(&err);
            if matches!(status, 401 | 403) {
                // The auth label carries the #162 key hint after the
                // prefix; the twin-contract assertion below pins that
                // the audit surface carries exactly the same text.
                assert!(
                    s.starts_with(expected),
                    "status {status} audit-redacted to wrong label prefix, got: {s}"
                );
            } else {
                assert_eq!(s, expected, "status {status} audit-redacted to wrong label");
            }
            assert!(
                !s.contains("sk-test-12345"),
                "API key must not survive audit redaction for status {status}, got: {s}"
            );
            assert!(
                !s.contains("prompt fragments"),
                "raw body must not survive audit redaction for status {status}, got: {s}"
            );
            // Twin contract: the audit-log helper agrees with the
            // session-history sanitiser on the redacted shape, so
            // operators see the same label in both surfaces.
            assert_eq!(
                s,
                sanitize_error_for_session(&err),
                "audit and session sanitisers must agree for LlmApiError"
            );
        }
    }

    /// #997: every non-`LlmApiError` variant must pass through the
    /// audit-log helper byte-for-byte, preserving the pre-#997 wire
    /// shape and the operator-authored debuggability the audit log is
    /// built for. Pin the contract for the variants that actually
    /// surface in `loop_impl.rs` audit emissions today.
    #[test]
    fn audit_error_string_passes_through_non_subagent_variants() {
        let cases = [
            AlmsError::ToolExecution("Invalid arguments: expected object at line 1".to_string()),
            AlmsError::ToolExecution("Tool 'shell' not allowed".to_string()),
            AlmsError::Runtime("provider 500 internal error".to_string()),
            AlmsError::ToolBlocked {
                reason: "rm -rf / blocked by classifier".to_string(),
                target: Some("/".to_string()),
            },
            AlmsError::Cancelled,
            AlmsError::SessionNotFound("sess-123".to_string()),
            AlmsError::AgentNotFound("missing-agent".to_string()),
            AlmsError::InvalidConfig("bad model".to_string()),
            AlmsError::Channel("telegram disconnected".to_string()),
            AlmsError::Sandbox("path escape".to_string()),
        ];
        for err in &cases {
            assert_eq!(
                audit_error_string(err),
                err.to_string(),
                "non-LlmApiError variants must pass through audit redaction unchanged"
            );
        }
    }

    /// #997: a `LlmApiError` whose body contains an API-key-shaped
    /// token, a verbatim user prompt fragment, and a verbatim model
    /// output snippet — the realistic Tim-flagged leak shape — must
    /// have all three stripped from the audit-log emission.
    #[test]
    fn audit_error_string_strips_api_key_and_prompt_and_output_fragments() {
        let body = r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: \"Authorization: Bearer sk-test-12345\\nUser said: please summarise this confidential memo about Project Apollo\\nAssistant began: Sure, the memo states that...\""}}"#;
        let err = AlmsError::LlmApiError {
            provider: "anthropic".to_string(),
            status: 400,
            body: body.to_string(),
        };
        let s = audit_error_string(&err);
        assert_eq!(s, "LLM request rejected");
        for needle in [
            "sk-test-12345",
            "Bearer",
            "Authorization",
            "Project Apollo",
            "confidential memo",
            "User said",
            "Assistant began",
            "prompt is too long",
        ] {
            assert!(
                !s.contains(needle),
                "leak fragment {needle:?} must not survive audit redaction: got {s:?}"
            );
        }
    }

    /// PR #1006 review (Tim): `FailedWithToolCalls`'s `Display` delegates
    /// to `{source}`, which means the catch-all `_` arm in
    /// `audit_error_string` would silently leak a wrapped
    /// `LlmApiError` body through `to_string()` if a future audit
    /// emission ever stringifies a wrapped error.
    ///
    /// Today this is unreachable — the variant is constructed at
    /// `crates/alms-runtime/src/agent/mod.rs:1114` *after* the run-loop's
    /// audit rows are already written, so audit emissions only ever see
    /// the unwrapped source. But the recursive-dispatch contract pins
    /// the redacted shape so that defence holds even if a new audit
    /// emission site is added that happens to stringify a wrapped
    /// error.
    ///
    /// This test exercises the wrapping shape directly: build a
    /// `FailedWithToolCalls` whose `source` is a `LlmApiError`
    /// carrying an API-key-shaped body, run it through
    /// `audit_error_string`, and assert the output is the same
    /// status-class label `sanitize_error_for_session` produces — no
    /// body fragment, no API key, no provider message survives the
    /// audit redaction even one wrap deep.
    #[test]
    fn audit_error_string_redacts_through_failed_with_tool_calls_wrapper() {
        let inner = AlmsError::LlmApiError {
            provider: "anthropic".to_string(),
            status: 400,
            body: "secret-key=sk-test-12345 prompt fragments leaked here".to_string(),
        };
        let wrapped = AlmsError::FailedWithToolCalls {
            source: Box::new(inner),
            tool_calls: vec![],
        };

        // Sanity check: the wrapper's own `Display` would leak the body
        // (this is exactly the latent leak Tim flagged) — `Display`
        // delegates to `{source}`, so the raw `to_string()` of the
        // wrapper renders the full `LlmApiError` line including
        // body. The audit helper must NOT take this path.
        let raw_display = wrapped.to_string();
        assert!(
            raw_display.contains("sk-test-12345"),
            "fixture sanity: wrapper's raw Display delegates to {{source}} and leaks the body \
             — that's the catch-all `_ => err.to_string()` failure mode this test guards against, \
             got: {raw_display:?}"
        );

        // The audit helper takes the recursive arm and redacts to the
        // same status-class label `sanitize_error_for_session` produces
        // for a bare `LlmApiError` of the same status.
        let s = audit_error_string(&wrapped);
        assert_eq!(
            s, "LLM request rejected",
            "FailedWithToolCalls wrapping LlmApiError must redact to status-class label"
        );
        for needle in [
            "sk-test-12345",
            "secret-key",
            "prompt fragments",
            "leaked here",
        ] {
            assert!(
                !s.contains(needle),
                "leak fragment {needle:?} must not survive audit redaction through wrapper: \
                 got {s:?}"
            );
        }

        // Twin-contract: the audit helper agrees with the session-history
        // sanitiser for the wrapped shape too (the session sanitiser
        // doesn't recurse today — it only sees the bare source — but
        // the resulting label is the same redacted shape, so the two
        // surfaces stay aligned).
        let inner_for_session = AlmsError::LlmApiError {
            provider: "anthropic".to_string(),
            status: 400,
            body: "irrelevant".to_string(),
        };
        assert_eq!(
            s,
            sanitize_error_for_session(&inner_for_session),
            "audit redaction through FailedWithToolCalls must match session-history sanitisation \
             of the equivalent bare LlmApiError"
        );
    }

    /// PR #1006 review: the recursive arm must also work when the
    /// `FailedWithToolCalls` source is a non-`LlmApiError` variant
    /// — the wrapped variant's normal pass-through behaviour is
    /// preserved, byte-for-byte. Pinning this guards against a regression
    /// where the recursive arm accidentally redacts variants that
    /// `audit_error_string` is meant to leave alone.
    #[test]
    fn audit_error_string_passes_through_failed_with_tool_calls_wrapping_safe_source() {
        let inner = AlmsError::ToolExecution("shell: invalid arguments".to_string());
        let inner_display = inner.to_string();
        let wrapped = AlmsError::FailedWithToolCalls {
            source: Box::new(inner),
            tool_calls: vec![],
        };
        // Recursive dispatch unwraps and falls through to the catch-all
        // `_ => err.to_string()` arm for the source — same shape an
        // unwrapped `ToolExecution` would produce.
        assert_eq!(
            audit_error_string(&wrapped),
            inner_display,
            "FailedWithToolCalls wrapping a non-sensitive variant must pass through verbatim \
             via recursive dispatch, matching the unwrapped source's audit shape"
        );
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
