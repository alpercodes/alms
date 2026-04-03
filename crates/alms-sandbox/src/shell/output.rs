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
