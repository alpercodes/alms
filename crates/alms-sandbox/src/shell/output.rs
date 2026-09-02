// SPDX-License-Identifier: Apache-2.0

//! Output truncation for the shell tool.
//!
//! When command output exceeds the configured byte limit, we keep the first
//! N head lines and last M tail lines, inserting a `[K lines omitted]` marker
//! in between. This preserves the most useful context (initial output and
//! final errors/results) while staying within token budgets.

use super::types::{HEAD_LINES, MAX_OUTPUT_BYTES, TAIL_LINES};

/// Head+tail truncation for raw process output.
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
    fn test_truncate_output_bytes_short_unchanged() {
        let bytes = b"hello world";
        assert_eq!(truncate_output_bytes(bytes), bytes.to_vec());
    }

    #[test]
    fn test_truncate_output_bytes_at_limit_unchanged() {
        // Boundary: exactly MAX_OUTPUT_BYTES must pass through untouched.
        //
        // Ported from the `&str` twin's `test_within_limit_unchanged`, removed
        // with the twin itself. `short_unchanged` above uses an 11-byte input,
        // so on its own it does not pin the `<=` in the length guard --
        // flipping it to `<` still passes.
        //
        // The input has to be multi-line to catch that. A single 30_000-byte
        // line takes the `byte_truncate_bytes` fallback, which appends its note
        // only when it actually drops bytes, so a one-line boundary input
        // survives the off-by-one unchanged and asserts nothing. With 3001
        // lines the mutant takes the head+tail branch instead and the omission
        // marker shows up.
        let mut bytes = vec![b'x'; MAX_OUTPUT_BYTES];
        for (n, b) in bytes.iter_mut().enumerate() {
            if n % 10 == 0 {
                *b = b'\n';
            }
        }
        assert_eq!(bytes.len(), MAX_OUTPUT_BYTES);
        assert!(bytes.iter().filter(|b| **b == b'\n').count() > HEAD_LINES + TAIL_LINES);

        assert_eq!(truncate_output_bytes(&bytes), bytes);
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
}
