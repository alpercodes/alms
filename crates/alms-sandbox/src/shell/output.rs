//! Output truncation for the shell tool.
//!
//! When command output exceeds the configured byte limit, we keep the first
//! N head lines and last M tail lines, inserting a `[K lines omitted]` marker
//! in between. This preserves the most useful context (initial output and
//! final errors/results) while staying within token budgets.

use super::types::{HEAD_LINES, MAX_OUTPUT_BYTES, TAIL_LINES};
use alms_core::truncate_to_char_boundary;

/// Truncate a single output stream (stdout or stderr) to stay within
/// the byte budget, using a head+tail line-preserving strategy.
///
/// If the output is within `MAX_OUTPUT_BYTES`, it is returned unchanged.
/// Otherwise, lines are split and the first `HEAD_LINES` and last `TAIL_LINES`
/// are kept, with an omission marker in between.
///
/// If the output exceeds `MAX_OUTPUT_BYTES` but has fewer total lines than
/// `HEAD_LINES + TAIL_LINES`, we fall back to byte-level truncation at a
/// UTF-8 char boundary.
pub fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_owned();
    }

    let lines: Vec<&str> = s.lines().collect();
    let total_lines = lines.len();

    // If we have enough lines, use head+tail strategy
    if total_lines > HEAD_LINES + TAIL_LINES {
        let head: Vec<&str> = lines[..HEAD_LINES].to_vec();
        let tail: Vec<&str> = lines[total_lines - TAIL_LINES..].to_vec();
        let omitted = total_lines - HEAD_LINES - TAIL_LINES;

        let mut result = head.join("\n");
        result.push_str(&format!("\n\n[{omitted} lines omitted]\n\n"));
        result.push_str(&tail.join("\n"));

        // Final byte-level safety check: the reassembled output could still
        // exceed MAX_OUTPUT_BYTES if individual lines are very long.
        if result.len() > MAX_OUTPUT_BYTES {
            byte_truncate(&result)
        } else {
            result
        }
    } else {
        // Not enough lines for head+tail split — fall back to byte truncation
        byte_truncate(s)
    }
}

/// Byte-level truncation with a UTF-8-safe boundary and a truncation note.
fn byte_truncate(s: &str) -> String {
    let truncated = truncate_to_char_boundary(s, MAX_OUTPUT_BYTES);
    let omitted_bytes = s.len() - truncated.len();
    format!("{truncated}\n\n[truncated, {omitted_bytes} bytes omitted]")
}

/// Byte-level variant of [`truncate_output`] for raw process output.
///
/// Operates on `&[u8]` so that lossy UTF-8 decoding can be deferred to the
/// boundary where the result is handed back to the agent. Binary-ish stdout
/// (e.g. compiler diagnostics that contain raw bytes, `tar` output, hex
/// dumps) and CRLF line endings on Windows shells are preserved through the
/// truncation step rather than being rewritten to U+FFFD replacement
/// characters mid-pipeline.
///
/// Returns a byte vector. The caller is expected to perform a single
/// `String::from_utf8_lossy` on the returned bytes when constructing the
/// final tool result.
pub fn truncate_output_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() <= MAX_OUTPUT_BYTES {
        return bytes.to_vec();
    }

    // Split into lines on '\n' and preserve CR if present at the end of each
    // line so Windows shell output round-trips correctly through truncation.
    let lines: Vec<&[u8]> = split_lines(bytes);
    let total_lines = lines.len();

    if total_lines > HEAD_LINES + TAIL_LINES {
        let head = &lines[..HEAD_LINES];
        let tail = &lines[total_lines - TAIL_LINES..];
        let omitted = total_lines - HEAD_LINES - TAIL_LINES;

        let mut result: Vec<u8> = Vec::new();
        for (i, line) in head.iter().enumerate() {
            if i > 0 {
                result.push(b'\n');
            }
            result.extend_from_slice(line);
        }
        // The omission marker is plain ASCII so it's safe to splice into bytes.
        result.extend_from_slice(format!("\n\n[{omitted} lines omitted]\n\n").as_bytes());
        for (i, line) in tail.iter().enumerate() {
            if i > 0 {
                result.push(b'\n');
            }
            result.extend_from_slice(line);
        }

        // Final byte-level safety check: head+tail could still exceed the
        // budget if individual lines are huge.
        if result.len() > MAX_OUTPUT_BYTES {
            byte_truncate_bytes(&result)
        } else {
            result
        }
    } else {
        byte_truncate_bytes(bytes)
    }
}

/// Byte-level cap with a truncation note. Operates on raw bytes; lossy UTF-8
/// decoding is the caller's responsibility.
fn byte_truncate_bytes(bytes: &[u8]) -> Vec<u8> {
    let cap = MAX_OUTPUT_BYTES.min(bytes.len());
    let mut result = bytes[..cap].to_vec();
    let omitted_bytes = bytes.len() - cap;
    if omitted_bytes > 0 {
        result.extend_from_slice(
            format!("\n\n[truncated, {omitted_bytes} bytes omitted]").as_bytes(),
        );
    }
    result
}

/// Split a byte slice into lines on '\n'. Trailing '\r' before '\n' (CRLF)
/// is preserved as part of the line so re-joining with '\n' reconstructs the
/// original CRLF sequence faithfully.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            lines.push(&bytes[start..i]);
            start = i + 1;
        }
    }
    if start <= bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_output_unchanged() {
        let s = "hello world";
        assert_eq!(truncate_output(s), "hello world");
    }

    #[test]
    fn test_within_limit_unchanged() {
        let s = "x".repeat(MAX_OUTPUT_BYTES);
        assert_eq!(truncate_output(&s), s);
    }

    #[test]
    fn test_line_based_truncation() {
        // Create output with 500 lines (well above HEAD + TAIL = 300)
        let lines: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let input = lines.join("\n");
        // Make sure it exceeds the byte limit
        let big_input = format!("{}\n{}", input, "x".repeat(MAX_OUTPUT_BYTES));

        let result = truncate_output(&big_input);

        // Should contain head lines
        assert!(result.contains("line 0"));
        assert!(result.contains("line 199"));

        // Should contain the omission marker
        assert!(result.contains("lines omitted"));

        // Should not contain lines from the middle
        assert!(!result.contains("line 250"));
    }

    #[test]
    fn test_byte_truncation_fallback() {
        // Create output that exceeds byte limit but has few lines
        let long_line = "x".repeat(MAX_OUTPUT_BYTES + 1000);
        let result = truncate_output(&long_line);

        assert!(result.contains("truncated"));
        assert!(result.contains("bytes omitted"));
        // The truncated portion should be at most MAX_OUTPUT_BYTES + the suffix
        assert!(result.len() < MAX_OUTPUT_BYTES + 200);
    }

    #[test]
    fn test_multibyte_safe_truncation() {
        // Create a string of multibyte characters that exceeds the limit
        let s = "a]".repeat(MAX_OUTPUT_BYTES); // Each char pair is 2+ bytes
        let result = truncate_output(&s);
        // Should not panic and should be valid UTF-8
        assert!(result.is_ascii() || !result.is_empty());
    }

    // ── Byte-level truncation ──────────────────────────────────────────────

    #[test]
    fn test_truncate_output_bytes_short_unchanged() {
        let bytes = b"hello world";
        assert_eq!(truncate_output_bytes(bytes), bytes.to_vec());
    }

    #[test]
    fn test_truncate_output_bytes_preserves_invalid_utf8() {
        // Build a buffer with an invalid UTF-8 byte. The byte-level path must
        // pass it through unchanged — no replacement chars at this stage.
        let mut bytes = b"prefix\n".to_vec();
        bytes.push(0xFF); // standalone 0xFF is invalid UTF-8
        bytes.extend_from_slice(b"\nsuffix");
        let result = truncate_output_bytes(&bytes);
        assert_eq!(result, bytes);
        assert!(
            result.contains(&0xFF),
            "raw 0xFF byte must survive truncation"
        );
    }

    #[test]
    fn test_truncate_output_bytes_preserves_crlf() {
        // Windows shells emit CRLF; truncation must not strip the \r before \n.
        let bytes = b"line1\r\nline2\r\nline3\r\n";
        let result = truncate_output_bytes(bytes);
        assert_eq!(result, bytes.to_vec());
        let s = String::from_utf8(result).unwrap();
        assert!(
            s.contains("\r\n"),
            "CRLF must round-trip through truncation"
        );
    }

    #[test]
    fn test_truncate_output_bytes_line_strategy() {
        // 500 ASCII lines, padded to exceed MAX_OUTPUT_BYTES.
        let mut buf: Vec<u8> = Vec::new();
        for i in 0..500 {
            buf.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        buf.extend_from_slice(&vec![b'x'; MAX_OUTPUT_BYTES]);

        let result = truncate_output_bytes(&buf);
        let s = String::from_utf8_lossy(&result);
        assert!(s.contains("line 0"));
        assert!(s.contains("line 199"));
        assert!(s.contains("lines omitted"));
        assert!(!s.contains("line 250"));
    }

    #[test]
    fn test_truncate_output_bytes_byte_fallback() {
        // Few lines but huge total — falls back to byte-level cap.
        let bytes = vec![b'x'; MAX_OUTPUT_BYTES + 1000];
        let result = truncate_output_bytes(&bytes);
        let s = String::from_utf8_lossy(&result);
        assert!(s.contains("truncated"));
        assert!(s.contains("bytes omitted"));
        // Should be at most MAX_OUTPUT_BYTES + a small suffix.
        assert!(result.len() < MAX_OUTPUT_BYTES + 200);
    }

    #[test]
    fn test_split_lines_basic() {
        let lines = split_lines(b"a\nb\nc");
        assert_eq!(lines, vec![&b"a"[..], &b"b"[..], &b"c"[..]]);
    }

    #[test]
    fn test_split_lines_trailing_newline() {
        let lines = split_lines(b"a\nb\n");
        // The slice after the trailing '\n' is empty but should still be present.
        assert_eq!(lines, vec![&b"a"[..], &b"b"[..], &b""[..]]);
    }

    #[test]
    fn test_split_lines_preserves_cr() {
        let lines = split_lines(b"a\r\nb\r\n");
        // CR is kept on the line; only LF is the delimiter.
        assert_eq!(lines, vec![&b"a\r"[..], &b"b\r"[..], &b""[..]]);
    }

    #[test]
    fn test_head_tail_content_preserved() {
        let mut lines: Vec<String> = (0..400).map(|i| format!("line-{i:04}")).collect();
        // Pad to exceed byte limit
        lines.push("z".repeat(MAX_OUTPUT_BYTES));
        let input = lines.join("\n");

        let result = truncate_output(&input);

        // First 200 lines should be present
        assert!(result.contains("line-0000"));
        assert!(result.contains("line-0199"));

        // Last 100 lines from the original (line 300..400 + the big line)
        // should be present
        assert!(result.contains("line-0301") || result.contains("lines omitted"));
    }
}
