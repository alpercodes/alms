// SPDX-License-Identifier: Apache-2.0

//! Structured diagnostic formatting for LLM-adapter decode/parse failures
//! (#1044).
//!
//! Background — before this module, the runtime collapsed every adapter
//! decode failure into one of two opaque strings:
//!
//! ```text
//! Runtime error: Failed to read response body: error decoding response body
//! Runtime error: Failed to parse response: <serde error>
//! ```
//!
//! Neither carries the provider name, the model, the HTTP status, the
//! response content-type, or any prefix of the body the parser actually
//! choked on. Operators staring at a subagent failure block in the UI
//! couldn't tell whether they were looking at a Cloudflare HTML error
//! page, a mid-stream truncation, schema drift, or a rate-limit response
//! with the wrong shape — and couldn't decide whether a retry would help.
//!
//! This module produces a single structured diagnostic string that the
//! decode/parse sites in [`super`] and [`super::streaming`] bake into the
//! bubbled `AlmsError::Runtime(...)`. Because the coordinator's subagent
//! path forwards `e.to_string()` verbatim through the tool-call result
//! payload, the enriched diagnostic surfaces automatically in:
//!
//! - The daemon log (existing `error!(...)` calls).
//! - The parent agent's `invoke_agent` tool-call result.
//! - The web UI's subagent error block.
//!
//! Output shape:
//!
//! ```text
//! LLM response decode failed [provider=<p> model=<m> status=<s>
//!   content_type=<ct> bytes_read=<n>]: <underlying>; body_prefix="<512B>"
//! ```
//!
//! Fields are omitted when not available (e.g. `bytes_read` only applies
//! to streaming, `body_prefix` only applies when the body was successfully
//! read but failed to deserialize). The bracketed key=value block is
//! single-line so it remains greppable in `RUST_LOG` output.
//!
//! Body prefix capture is bounded to `BODY_PREFIX_MAX_BYTES` (512 bytes)
//! to keep the bubbled error a reasonable size for the tool-call result
//! payload that the parent agent receives. Embedded control characters
//! are escaped so a malformed binary body can't break log line framing.

use std::fmt::Write as _;

/// Maximum number of bytes of response body to include in the
/// diagnostic. Keeps the bubbled error a sane size for the tool-call
/// result payload that the parent agent receives — large enough to
/// distinguish HTML error pages, JSON error envelopes, and schema-drift
/// shapes at a glance; small enough to not bloat the LLM context if the
/// error survives into a follow-up turn.
pub(super) const BODY_PREFIX_MAX_BYTES: usize = 512;

/// One key=value field in a diagnostic string.
///
/// All fields are optional; the formatter skips any that are `None` so
/// the produced string carries only what the caller actually has at the
/// failure site. `bytes_read` is `Option<usize>` so it can encode both
/// "not applicable" (non-streaming path) and "applicable but zero".
#[derive(Debug, Default)]
pub(super) struct DecodeDiagnostic<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub status: Option<u16>,
    pub content_type: Option<&'a str>,
    pub content_length: Option<u64>,
    /// Number of bytes consumed before the error fired. Streaming path
    /// only — `None` on non-streaming decode failures.
    pub bytes_read: Option<usize>,
    /// First [`BODY_PREFIX_MAX_BYTES`] bytes of the response body when
    /// the body was successfully read but failed to deserialize. `None`
    /// on the body-read-failed path (we don't have the body in that case).
    pub body_prefix: Option<&'a str>,
}

/// Format a decode/parse failure into a structured single-line message.
///
/// `context` describes what failed at a high level — typically
/// `"LLM response decode failed"` (body-read) or
/// `"LLM response parse failed"` (deserialize) or
/// `"LLM stream decode failed"` (streaming body-read). `underlying` is
/// the chained underlying error's `Display` rendering.
pub(super) fn format_decode_error(
    context: &str,
    diag: &DecodeDiagnostic<'_>,
    underlying: &dyn std::fmt::Display,
) -> String {
    // Buffer sized for the typical "context + ~6 fields + underlying +
    // 512-byte body prefix" worst case. Avoids repeated reallocs during
    // the format loop.
    let mut out = String::with_capacity(context.len() + 768);
    out.push_str(context);
    out.push_str(" [");

    let mut first = true;
    let mut push = |buf: &mut String, key: &str, val: &dyn std::fmt::Display| {
        if !first {
            buf.push(' ');
        }
        first = false;
        // `write!` to a String is infallible.
        let _ = write!(buf, "{key}={val}");
    };

    // Skip empty provider/model so a malformed operator config
    // (`provider = ""`) doesn't produce a literal `provider= model=...`
    // bracket in the diagnostic. Tim, review on #1064.
    if !diag.provider.is_empty() {
        push(&mut out, "provider", &diag.provider);
    }
    if !diag.model.is_empty() {
        push(&mut out, "model", &diag.model);
    }
    if let Some(s) = diag.status {
        push(&mut out, "status", &s);
    }
    if let Some(ct) = diag.content_type {
        push(&mut out, "content_type", &ct);
    }
    if let Some(cl) = diag.content_length {
        push(&mut out, "content_length", &cl);
    }
    if let Some(br) = diag.bytes_read {
        push(&mut out, "bytes_read", &br);
    }

    out.push(']');
    out.push(':');
    out.push(' ');
    let _ = write!(out, "{underlying}");

    if let Some(body) = diag.body_prefix {
        let trimmed = truncate_for_diagnostic(body, BODY_PREFIX_MAX_BYTES);
        out.push_str("; body_prefix=\"");
        out.push_str(&escape_for_diagnostic(&trimmed));
        out.push('"');
    }

    out
}

/// Truncate to at most `max_bytes` bytes, snapping back to the nearest
/// char boundary so we never split a UTF-8 codepoint mid-byte. Appends
/// the `…` ellipsis marker when truncation actually occurred so the
/// reader can tell at-a-glance that more body followed.
pub(super) fn truncate_for_diagnostic(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Snap to a char boundary at or below `max_bytes` so we don't split
    // a multi-byte UTF-8 codepoint. `floor_char_boundary` would be
    // stable-friendlier but only landed recently — walk backwards
    // manually instead.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// Replace embedded ASCII control characters with their `\xNN` escape
/// form so a malformed binary body in the prefix can't break log-line
/// framing or terminal rendering. Newlines, tabs, and carriage returns
/// are replaced with their conventional escapes for readability. Non-
/// ASCII bytes pass through untouched — they're already valid UTF-8 by
/// construction (we only call this on `&str`).
pub(super) fn escape_for_diagnostic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Walk a `std::error::Error` chain and collapse it into a single string
/// of the form `"top: cause1: cause2"`. Used so the bubbled error
/// preserves the `reqwest::Error` → `hyper::Error` → IO error chain that
/// the bare `Display` of the top-level error otherwise drops.
///
/// Anchors the walk at one full pass and bounds depth to 8 to defend
/// against pathological self-referential error chains.
pub(super) fn flatten_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = String::new();
    let _ = write!(out, "{err}");
    let mut depth = 0;
    let mut src = err.source();
    while let Some(s) = src {
        if depth >= 8 {
            out.push_str(": <chain truncated>");
            break;
        }
        let _ = write!(out, ": {s}");
        src = s.source();
        depth += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_all_fields_present() {
        let diag = DecodeDiagnostic {
            provider: "anthropic",
            model: "claude-sonnet-4-5",
            status: Some(200),
            content_type: Some("application/json"),
            content_length: Some(2048),
            bytes_read: None,
            body_prefix: Some(r#"{"type":"error","error":{"type":"overloaded"}}"#),
        };
        let s = format_decode_error("LLM response parse failed", &diag, &"missing field `id`");
        assert!(s.starts_with("LLM response parse failed ["));
        assert!(s.contains("provider=anthropic"));
        assert!(s.contains("model=claude-sonnet-4-5"));
        assert!(s.contains("status=200"));
        assert!(s.contains("content_type=application/json"));
        assert!(s.contains("content_length=2048"));
        assert!(s.contains("missing field `id`"));
        assert!(s.contains(r#"body_prefix="{\"type\":\"error\""#));
    }

    #[test]
    fn formats_minimal_fields() {
        let diag = DecodeDiagnostic {
            provider: "openai",
            model: "gpt-4o",
            ..Default::default()
        };
        let s = format_decode_error(
            "LLM response decode failed",
            &diag,
            &"error decoding response body",
        );
        assert_eq!(
            s,
            "LLM response decode failed [provider=openai model=gpt-4o]: error decoding response body"
        );
    }

    #[test]
    fn formats_streaming_bytes_read() {
        let diag = DecodeDiagnostic {
            provider: "gemini",
            model: "gemini-2.5-pro",
            bytes_read: Some(1024),
            ..Default::default()
        };
        let s = format_decode_error("LLM stream decode failed", &diag, &"connection reset");
        assert!(s.contains("bytes_read=1024"));
        assert!(s.contains("connection reset"));
        assert!(!s.contains("body_prefix"));
    }

    /// Nit 2 from Tim's review on #1064: a malformed operator config
    /// that leaves `provider = ""` (or `model = ""`) must not produce a
    /// literal `provider= model=...` bracket in the diagnostic. The
    /// formatter skips empty fields so the bracket only carries
    /// information the caller actually has.
    #[test]
    fn formats_skip_empty_provider_and_model() {
        let diag = DecodeDiagnostic {
            provider: "",
            model: "",
            status: Some(200),
            ..Default::default()
        };
        let s = format_decode_error("LLM response decode failed", &diag, &"boom");
        // The bracket carries only the fields actually populated.
        assert_eq!(s, "LLM response decode failed [status=200]: boom");
        assert!(!s.contains("provider="));
        assert!(!s.contains("model="));
    }

    /// Companion to `formats_skip_empty_provider_and_model`: when only
    /// one of provider/model is empty, the populated one still surfaces.
    #[test]
    fn formats_skip_empty_provider_keeps_model() {
        let diag = DecodeDiagnostic {
            provider: "",
            model: "gpt-4o",
            ..Default::default()
        };
        let s = format_decode_error("LLM response decode failed", &diag, &"boom");
        assert_eq!(s, "LLM response decode failed [model=gpt-4o]: boom");
    }

    #[test]
    fn truncates_long_body_at_char_boundary() {
        // Build a string longer than the cap with a multi-byte codepoint
        // straddling the cap so we can verify the truncation snaps to a
        // char boundary instead of splitting the codepoint.
        let head = "a".repeat(BODY_PREFIX_MAX_BYTES - 1);
        let body = format!("{head}é tail"); // é = 2 bytes, lands across the cap
        let out = truncate_for_diagnostic(&body, BODY_PREFIX_MAX_BYTES);
        // The truncated form ends with the ellipsis and is valid UTF-8.
        assert!(out.ends_with('…'));
        assert!(out.len() < body.len());
        // `é` straddled the cap; the truncator should have stepped back
        // to before the multi-byte codepoint instead of slicing it.
        assert!(!out.contains('é'));
    }

    #[test]
    fn truncation_no_op_when_short() {
        let s = "short body";
        let out = truncate_for_diagnostic(s, BODY_PREFIX_MAX_BYTES);
        assert_eq!(out, s);
        assert!(!out.ends_with('…'));
    }

    #[test]
    fn escapes_newlines_and_quotes() {
        let raw = "line1\nline2\t\"quoted\"\\back";
        let escaped = escape_for_diagnostic(raw);
        assert_eq!(escaped, r#"line1\nline2\t\"quoted\"\\back"#);
    }

    #[test]
    fn escapes_control_bytes() {
        // 0x07 (BEL) is a non-printable control char that should not
        // appear verbatim in log output. Embedded NUL is the load-bearing
        // case — a binary body containing a NUL byte must not truncate
        // downstream consumers that treat NUL as a terminator.
        let raw = "ok\x07\x00end";
        let escaped = escape_for_diagnostic(raw);
        assert_eq!(escaped, "ok\\x07\\x00end");
    }

    #[test]
    fn flatten_chain_walks_sources() {
        use std::error::Error;
        use std::fmt;

        #[derive(Debug)]
        struct Top;
        #[derive(Debug)]
        struct Mid;
        #[derive(Debug)]
        struct Bot;

        impl fmt::Display for Top {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "top error")
            }
        }
        impl fmt::Display for Mid {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "mid error")
            }
        }
        impl fmt::Display for Bot {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "bot error")
            }
        }
        impl Error for Bot {}
        impl Error for Mid {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                static BOT: Bot = Bot;
                Some(&BOT)
            }
        }
        impl Error for Top {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                static MID: Mid = Mid;
                Some(&MID)
            }
        }

        let s = flatten_error_chain(&Top);
        assert_eq!(s, "top error: mid error: bot error");
    }
}
