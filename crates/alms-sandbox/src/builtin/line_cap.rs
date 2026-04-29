//! Per-line allocation cap used by `fs_read` (#902) and `fs_grep` (#913).
//!
//! `BufReader::lines()` / `next_line()` allocate per line without any cap, so a
//! pathological file (e.g. a multi-GB minified bundle with no newlines) would
//! pull the entire file into one `String` and OOM the daemon process.  This
//! module provides a drop-in replacement that bounds buffered allocation to
//! [`MAX_LINE_BYTES`].

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Maximum bytes retained from a single line before truncation.
///
/// The cap is 256 KiB — half of `fs_read`'s 512 KiB output budget.  Sized so
/// that even a maximally-truncated single line plus its inline marker still
/// fits well within the response budget, leaving room for at least one more
/// line of useful context.  Higher caps were considered but rejected: a 512
/// KiB truncated line consumes the entire output budget on its own.
pub(super) const MAX_LINE_BYTES: usize = 256 * 1024;

/// Outcome of [`read_line_capped`].
pub(super) struct LineRead {
    /// The line contents, capped at [`MAX_LINE_BYTES`].  When `truncated` is
    /// true an inline marker has already been appended — this is the form
    /// `fs_read` displays to the model so it can see that bytes were dropped.
    pub line: String,
    /// True if the line exceeded the cap and was truncated (with the rest of
    /// the line drained so the next read starts at the following line).
    pub truncated: bool,
    /// Byte length of the captured-from-file portion of [`Self::line`],
    /// **excluding** any appended truncation marker.  When `truncated` is
    /// false this equals `line.len()`; when true it is strictly less.  Used
    /// by `fs_grep` to evaluate regex matches against real file content
    /// only — the marker text is itself regex-matchable, so a user pattern
    /// like `truncated` or `bytes` would otherwise spuriously hit the
    /// marker rather than file content (issue raised in Tim's review of
    /// PR #922).
    pub captured_byte_len: usize,
}

/// Read a single line from `reader`, capping the buffered bytes at
/// [`MAX_LINE_BYTES`].  If the line is longer than the cap, the surplus bytes
/// (up to the next `\n` or EOF) are drained and discarded, and an inline
/// `[line truncated to N bytes; M bytes discarded]` marker is appended to the
/// returned string.
///
/// Returns `Ok(None)` only at EOF before any byte was read.  UTF-8 invalid
/// sequences in the truncated buffer are replaced with `U+FFFD` via
/// [`String::from_utf8_lossy`] so we never split a multi-byte sequence.
pub(super) async fn read_line_capped<R>(reader: &mut R) -> std::io::Result<Option<LineRead>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();

    // Phase A: accumulate bytes up to the cap, watching for `\n`.
    while buf.len() < MAX_LINE_BYTES {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // Genuine EOF.  If we never wrote anything, signal end of file.
            if buf.is_empty() {
                return Ok(None);
            }
            // EOF after a final un-newlined line — return what we have.
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            let line = String::from_utf8_lossy(&buf).into_owned();
            let captured_byte_len = line.len();
            return Ok(Some(LineRead {
                line,
                truncated: false,
                captured_byte_len,
            }));
        }

        // Look for a newline within the remaining capacity.
        let remaining = MAX_LINE_BYTES - buf.len();
        let take = available.len().min(remaining);
        if let Some(nl_idx) = available[..take].iter().position(|&b| b == b'\n') {
            // Newline reached within cap — copy through it (excluding `\n`)
            // and consume the `\n` so the next call starts fresh.
            buf.extend_from_slice(&available[..nl_idx]);
            reader.consume(nl_idx + 1);
            // Strip a trailing `\r` for CRLF normalisation, matching
            // `BufReader::next_line()` behaviour.
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            let line = String::from_utf8_lossy(&buf).into_owned();
            let captured_byte_len = line.len();
            return Ok(Some(LineRead {
                line,
                truncated: false,
                captured_byte_len,
            }));
        }

        // No newline in this slice within the cap window — keep accumulating.
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
    }

    // Phase B: cap reached without seeing a newline — drain the rest of this
    // line (or hit EOF) without growing the buffer further.  The number of
    // bytes drained is reported back via the inline marker.
    let drained_bytes = drain_to_newline(reader).await?;

    // Strip a trailing `\r` if the cap landed right before the `\n` of CRLF.
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let mut line = String::from_utf8_lossy(&buf).into_owned();
    // Snapshot the byte length of the captured-from-file content *before*
    // appending the marker, so callers (`fs_grep`) can match regexes against
    // real file content only.
    let captured_byte_len = line.len();
    line.push_str(&format!(
        "[line truncated to {} bytes; {} bytes discarded]",
        buf.len(),
        drained_bytes
    ));

    Ok(Some(LineRead {
        line,
        truncated: true,
        captured_byte_len,
    }))
}

/// Read and discard bytes until the next `\n` (or EOF), returning the number
/// of bytes drained (excluding the terminating `\n`).  Used by
/// [`read_line_capped`] after the per-line cap is exhausted so the next read
/// positions at the start of the next line.
async fn drain_to_newline<R>(reader: &mut R) -> std::io::Result<u64>
where
    R: AsyncBufRead + Unpin,
{
    let mut drained: u64 = 0;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(drained);
        }
        if let Some(idx) = available.iter().position(|&b| b == b'\n') {
            drained += idx as u64;
            reader.consume(idx + 1);
            return Ok(drained);
        }
        let n = available.len();
        drained += n as u64;
        reader.consume(n);
    }
}
