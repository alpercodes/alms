//! Shared in-loop tool-output truncation service (issue #851).
//!
//! Every tool's result is routed through [`truncate`] before it lands in the
//! agent loop's `Vec<LlmMessage>` and before it is persisted to the session
//! database. The cap is **head + tail preview, capped at
//! [`DEFAULT_MAX_BYTES`] OR [`DEFAULT_MAX_LINES`], whichever fires first**;
//! when truncation triggers, the *full* raw output is written to a per-run
//! spill file under `{data_dir}/tool-output/{run_id}/tool_<call_id>.txt` so
//! the agent can recover it via `fs_read` / `fs_grep`.
//!
//! ## Why at the loop boundary
//!
//! Pre-#851 each tool was responsible for capping its own output and the caps
//! varied wildly (`shell` 30 KB, `fs_grep` 20 KB, `fs_read` 512 KB, `http_get`
//! none at all, `read_session` none at all). A single 200 KB `fs_read` plus a
//! parallel 200 KB `http_get` could push a turn's input above 256 K tokens and
//! blow the provider's context window. Routing every tool's result through
//! one place — exactly like a single shared truncation service — gives a
//! predictable upper bound regardless of which tool produced the bytes.
//!
//! ## Wire shape
//!
//! - **No truncation needed** → returns the original string unchanged with
//!   `truncated == false` and `output_path == None`.
//! - **Truncation needed** → returns a head+tail preview that fits inside the
//!   byte and line caps, with an LLM-visible hint pointing at the spill file:
//!   `*The tool output was truncated to N bytes. Full output saved to: <rel
//!   path>. Use `fs_grep` to search the full content or `fs_read` with
//!   `offset`/`limit` to view specific sections.*`
//! - The spill file is the *exact* original bytes (no head/tail compression)
//!   so the agent's recovery path is byte-perfect.
//!
//! ## Retention
//!
//! Spill files are retained for `retention_days` (default 7).
//! [`sweep_expired`] is called once at gateway startup and removes any file
//! whose filesystem `mtime` is older than the retention window. Empty
//! per-run directories are also removed so the tree stays tidy. There is no
//! background ticker — the model mirrors `shell_output`'s retention, which
//! has been shipping since #756.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// The base directory under `{data_dir}` where in-loop tool output spills
/// live. Distinct from `shell_output/` (which holds the shell tool's
/// internal head+tail spill) so the two features can be tuned and swept
/// independently.
pub const TOOL_OUTPUT_DIR_NAME: &str = "tool-output";

/// Default byte cap for in-loop tool outputs (32 KB).
///
/// Picked to give the agent a useful preview of large outputs (about 8K
/// tokens worst case) without letting any single tool dominate the next
/// turn's context window. Matches the spec in #851 — comparable tools cap at 50 KB,
/// we cap at 32 KB because ALMS's per-turn budget is tighter on the
/// providers we support.
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024;

/// Default line cap for in-loop tool outputs.
///
/// Many tools (grep, list, log dumps) stay well under the byte cap but emit
/// thousands of one-line entries — the line cap fires first there and keeps
/// the LLM's "scan the result" cost bounded.
pub const DEFAULT_MAX_LINES: usize = 2000;

/// Default retention window in days for spill files.
pub const DEFAULT_RETENTION_DAYS: u32 = 7;

/// Per-[`AgentRuntime`][crate::AgentRuntime] truncation policy, baked in at
/// runtime construction.
///
/// `run_dir` is the absolute path of the per-run directory
/// (`{data_dir}/tool-output/{run_id}/`). When `enabled == false` or `run_dir`
/// is `None`, the service is a no-op: every tool's output passes through
/// unchanged regardless of size. This matches the `ShellSpillPolicy` shape
/// so the runtime can wire both features uniformly.
#[derive(Debug, Clone)]
pub struct ToolOutputTruncatePolicy {
    /// Whether the truncation service is active. `false` is a hard opt-out:
    /// no spill directory is created, no truncation happens.
    pub enabled: bool,

    /// Absolute path of the per-run spill directory. Created lazily on first
    /// truncation. `None` when the runtime has no run_id wired in (e.g.
    /// unit tests, ephemeral construction).
    pub run_dir: Option<PathBuf>,

    /// Hard byte cap on the preview returned to the agent. Outputs larger
    /// than this trigger truncation + spill.
    pub max_bytes: usize,

    /// Hard line cap on the preview returned to the agent. Outputs with
    /// more than this many lines trigger truncation + spill, even if they
    /// fit inside `max_bytes`.
    pub max_lines: usize,
}

impl ToolOutputTruncatePolicy {
    /// Return a disabled policy. Useful as an explicit "no truncation"
    /// marker in tests and in construction paths that don't yet have a
    /// run_id.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            run_dir: None,
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: DEFAULT_MAX_LINES,
        }
    }

    /// Return a policy that writes spills to `run_dir` with the default
    /// caps.
    pub fn with_run_dir(run_dir: PathBuf) -> Self {
        Self {
            enabled: true,
            run_dir: Some(run_dir),
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: DEFAULT_MAX_LINES,
        }
    }

    /// Whether this policy will truncate + spill for the current invocation.
    pub fn is_active(&self) -> bool {
        self.enabled && self.run_dir.is_some()
    }
}

impl Default for ToolOutputTruncatePolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Outcome of a single [`truncate`] call.
///
/// When the input was inside both caps, `truncated == false`, `content` is
/// the input string verbatim, and `output_path` is `None`. When truncation
/// fires, `content` is a head+tail preview with an LLM-visible hint
/// appended, `truncated == true`, and `output_path` carries the workspace-
/// relative path of the spill file when one was successfully written.
#[derive(Debug, Clone)]
pub struct TruncateOutcome {
    /// The (possibly truncated) text the agent sees. When truncation fired,
    /// this is the head+tail preview plus an appended "full output saved
    /// to: <rel_path>" hint so the LLM knows where to look.
    pub content: String,
    /// `true` when either cap fired. `false` means the input passed through
    /// unchanged.
    pub truncated: bool,
    /// Workspace-relative path of the spill file. `Some(...)` only when
    /// truncation fired AND the spill write succeeded. `None` on a no-op
    /// pass-through OR on a spill-write failure (which is non-fatal — the
    /// agent still gets the preview, just without the recovery file).
    pub output_path: Option<String>,
    /// Original byte count before truncation, for diagnostics.
    pub original_bytes: usize,
    /// Original line count before truncation, for diagnostics.
    pub original_lines: usize,
}

/// Truncate `raw` according to `policy` and write the full bytes to a spill
/// file when either cap fires.
///
/// `tool_call_id` is the LLM-provided ID of the call that produced `raw`;
/// it is sanitized into the spill filename so the file can be located
/// deterministically from logs / SSE events.
///
/// `workspace_root` is the agent's workspace directory (when one is
/// attached); the spill file path is rewritten relative to this root in the
/// hint so the agent's `fs_read` reaches the file via its existing sandbox
/// (the gateway widens the agent's `extra_read_roots` to include the
/// per-run spill dir, mirroring shell_output).
///
/// Errors are non-fatal: a spill-write failure produces an outcome with
/// `truncated == true` and `output_path == None` so the caller still emits
/// the preview to the agent.
pub fn truncate(
    raw: &str,
    policy: &ToolOutputTruncatePolicy,
    tool_call_id: &str,
    workspace_root: Option<&Path>,
) -> TruncateOutcome {
    let original_bytes = raw.len();
    let original_lines = count_lines(raw);

    // Fast path: no truncation needed and policy is well-formed. We still
    // return the line/byte counts so callers (and tests) can observe them
    // without re-walking the input.
    let bytes_exceeded = original_bytes > policy.max_bytes;
    let lines_exceeded = original_lines > policy.max_lines;

    if !policy.is_active() || (!bytes_exceeded && !lines_exceeded) {
        return TruncateOutcome {
            content: raw.to_string(),
            truncated: false,
            output_path: None,
            original_bytes,
            original_lines,
        };
    }

    // Either cap fired — write the full bytes to disk before any preview
    // computation so a panic in the preview path doesn't lose the recovery
    // file.
    let run_dir = policy
        .run_dir
        .as_deref()
        .expect("is_active() implies run_dir");
    let spill_path = match write_spill(run_dir, tool_call_id, raw.as_bytes()) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!(
                error = %e,
                run_dir = %run_dir.display(),
                tool_call_id = %tool_call_id,
                "Failed to write tool-output spill file; falling back to truncated preview only"
            );
            None
        }
    };

    let rel_path = spill_path
        .as_deref()
        .map(|p| relative_path(p, workspace_root));

    let preview = build_preview(
        raw,
        policy.max_bytes,
        policy.max_lines,
        original_bytes,
        original_lines,
        rel_path.as_deref(),
    );

    TruncateOutcome {
        content: preview,
        truncated: true,
        output_path: rel_path,
        original_bytes,
        original_lines,
    }
}

/// Count the number of lines in `s`. Treats a trailing newline as not
/// starting a new line, matching `wc -l` and the agent's intuition for
/// "this is N lines of output".
fn count_lines(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    let mut n = s.bytes().filter(|b| *b == b'\n').count();
    if !s.ends_with('\n') {
        n += 1;
    }
    n
}

/// Build the head+tail preview returned to the agent.
///
/// Strategy: take the first `head_lines` lines and the last `tail_lines`
/// lines (after the byte cap is split roughly 70/30 between head and tail),
/// then prepend a marker noting the original size and the spill path.
///
/// The exact split is not load-bearing for the LLM; what matters is:
///   1. The head shows the *kind* of output (so the agent recognises a
///      diff, a JSON blob, a directory listing, etc.).
///   2. The tail shows the *outcome* (which is where most build/test/log
///      tools put their failure summary).
///   3. The hint in the middle tells the LLM where the omitted bytes went.
fn build_preview(
    raw: &str,
    max_bytes: usize,
    max_lines: usize,
    original_bytes: usize,
    original_lines: usize,
    rel_spill_path: Option<&str>,
) -> String {
    // Reserve about 1 KB of the byte budget for the marker text so the
    // preview itself never overruns the cap by a large margin.
    let marker_budget: usize = 1024;
    let preview_budget = max_bytes.saturating_sub(marker_budget).max(2 * 1024);

    // Split the preview budget roughly 70/30 between head and tail. The
    // bias toward the head is intentional: most tools (file dumps, JSON
    // payloads, listings) put their identifying bytes near the start, while
    // the tail is most useful when the *outcome* of a long-running tool
    // (compilation errors, test summary, traceback) is at the bottom.
    let head_byte_budget = (preview_budget * 7) / 10;
    let tail_byte_budget = preview_budget - head_byte_budget;

    // Apply the same 70/30 split to the line cap.
    let head_line_budget = (max_lines * 7) / 10;
    let tail_line_budget = max_lines.saturating_sub(head_line_budget);

    let head = take_head(raw, head_byte_budget, head_line_budget);
    let tail = take_tail(raw, tail_byte_budget, tail_line_budget);

    let omitted_bytes = original_bytes.saturating_sub(head.len() + tail.len());
    let omitted_lines = original_lines.saturating_sub(count_lines(head) + count_lines(tail));

    let hint = match rel_spill_path {
        Some(rel) => format!(
            "\n\n[The tool output was truncated to {kb} KB. \
             Full output saved to: `{rel}` ({original_bytes} bytes, {original_lines} lines). \
             Use `fs_grep` to search the full content or `fs_read` with `offset`/`limit` \
             to view specific sections.]\n",
            kb = max_bytes / 1024,
            rel = rel,
            original_bytes = original_bytes,
            original_lines = original_lines,
        ),
        None => format!(
            "\n\n[The tool output was truncated to {kb} KB. \
             The full-output spill file could not be written; only this preview is available. \
             Original size: {original_bytes} bytes, {original_lines} lines.]\n",
            kb = max_bytes / 1024,
            original_bytes = original_bytes,
            original_lines = original_lines,
        ),
    };

    let middle = format!(
        "\n\n... [{omitted_bytes} bytes / {omitted_lines} lines omitted] ...\n\n",
        omitted_bytes = omitted_bytes,
        omitted_lines = omitted_lines,
    );

    let mut buf = String::with_capacity(head.len() + middle.len() + tail.len() + hint.len());
    buf.push_str(head);
    buf.push_str(&middle);
    buf.push_str(tail);
    buf.push_str(&hint);
    buf
}

/// Take the longest prefix of `s` that fits in both `byte_budget` and
/// `line_budget`. Always returns a UTF-8-valid &str (truncates at a char
/// boundary if the byte budget falls mid-codepoint).
fn take_head(s: &str, byte_budget: usize, line_budget: usize) -> &str {
    if s.is_empty() || byte_budget == 0 || line_budget == 0 {
        return "";
    }

    // Walk byte-by-byte, counting newlines. Stop at the earlier of:
    //   - the next char boundary at or above byte_budget
    //   - the line_budget'th newline
    let bytes = s.as_bytes();
    let mut end = 0usize;
    let mut lines = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            lines += 1;
            if lines >= line_budget {
                end = i + 1; // include the newline so the line is complete
                break;
            }
        }
        if i + 1 >= byte_budget {
            end = i + 1;
            break;
        }
        end = i + 1;
    }
    // Step back to a char boundary if the byte cap split a multi-byte char.
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Take the longest suffix of `s` that fits in both `byte_budget` and
/// `line_budget`. Symmetric counterpart to [`take_head`].
fn take_tail(s: &str, byte_budget: usize, line_budget: usize) -> &str {
    if s.is_empty() || byte_budget == 0 || line_budget == 0 {
        return "";
    }

    let bytes = s.as_bytes();
    let len = bytes.len();
    // Walk backwards, counting newlines.
    let mut start = len;
    let mut lines = 0usize;
    for i in (0..len).rev() {
        if bytes[i] == b'\n' {
            // Only count the newline as a line break when there is content
            // after it; this avoids the trailing-newline-of-input being
            // counted as its own line.
            if i + 1 < len {
                lines += 1;
            }
            if lines >= line_budget {
                start = i + 1;
                break;
            }
        }
        if len - i >= byte_budget {
            start = i;
            break;
        }
        start = i;
    }
    // Step forward to a char boundary if the byte cap split a multi-byte char.
    while start < len && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Write the full pre-truncation bytes for a single tool invocation to
/// `{run_dir}/tool_<sanitized_id>.txt`.
fn write_spill(run_dir: &Path, tool_call_id: &str, raw: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(run_dir)?;

    let filename = format!("tool_{}.txt", sanitize_tool_call_id(tool_call_id));
    let path = run_dir.join(filename);
    std::fs::write(&path, raw)?;
    debug!(
        path = %path.display(),
        bytes = raw.len(),
        "Wrote tool-output spill file"
    );
    Ok(path)
}

/// Sanitize `tool_call_id` for use as a filename fragment.
///
/// IDs from LLM providers are usually ASCII (`call_abc123`) but we
/// defensively strip anything that could escape the directory or create a
/// hidden file on POSIX.
fn sanitize_tool_call_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        format!("unknown_{ts}")
    } else {
        cleaned
    }
}

/// Compute a path relative to `workspace_root`, falling back to the
/// absolute path if the spill file is not inside the workspace root.
fn relative_path(spill_path: &Path, workspace_root: Option<&Path>) -> String {
    if let Some(root) = workspace_root
        && let Ok(rel) = spill_path.strip_prefix(root)
    {
        return rel.to_string_lossy().replace('\\', "/");
    }
    spill_path.to_string_lossy().replace('\\', "/")
}

/// Delete spilled tool-output files older than `retention_days` under
/// `{data_dir}/tool-output/`.
///
/// Mirrors [`crate::spill::sweep_expired`]: walks every per-run directory,
/// unlinks any file older than the cutoff, removes empty per-run dirs.
/// Returns the number of files deleted.
pub fn sweep_expired(data_dir: &Path, retention_days: u32) -> io::Result<u64> {
    let root = data_dir.join(TOOL_OUTPUT_DIR_NAME);
    if !root.exists() {
        return Ok(0);
    }

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut deleted: u64 = 0;
    let dir_iter = match std::fs::read_dir(&root) {
        Ok(it) => it,
        Err(e) => {
            warn!(path = %root.display(), error = %e, "Failed to read tool-output directory");
            return Err(e);
        }
    };

    for run_entry in dir_iter {
        let run_entry = match run_entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to read tool-output run entry");
                continue;
            }
        };
        let run_path = run_entry.path();
        if !run_path.is_dir() {
            continue;
        }

        let files = match std::fs::read_dir(&run_path) {
            Ok(it) => it,
            Err(e) => {
                warn!(path = %run_path.display(), error = %e, "Failed to read tool-output run dir");
                continue;
            }
        };

        let mut remaining: u64 = 0;
        for file_entry in files {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "Failed to read tool-output file entry");
                    continue;
                }
            };
            let file_path = file_entry.path();
            let mtime = file_entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime < cutoff {
                match std::fs::remove_file(&file_path) {
                    Ok(()) => {
                        deleted += 1;
                        debug!(path = %file_path.display(), "Removed expired tool-output spill file");
                    }
                    Err(e) => {
                        warn!(path = %file_path.display(), error = %e, "Failed to remove expired tool-output spill file");
                        remaining += 1;
                    }
                }
            } else {
                remaining += 1;
            }
        }

        if remaining == 0
            && let Err(e) = std::fs::remove_dir(&run_path)
        {
            debug!(path = %run_path.display(), error = %e, "Failed to remove empty tool-output run dir");
        }
    }

    if deleted > 0 {
        info!(
            deleted,
            tool_output_root = %root.display(),
            retention_days,
            "Swept expired tool-output spill files"
        );
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_dir(dir: &Path) -> ToolOutputTruncatePolicy {
        ToolOutputTruncatePolicy::with_run_dir(dir.to_path_buf())
    }

    #[test]
    fn pass_through_under_caps() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input = "hello world\nsecond line\n";
        let out = truncate(input, &policy, "call_a", None);
        assert!(!out.truncated);
        assert_eq!(out.content, input);
        assert!(out.output_path.is_none());
        assert_eq!(out.original_bytes, input.len());
        assert_eq!(out.original_lines, 2);
        // No spill file should be written when no truncation fired.
        let spills: Vec<_> = std::fs::read_dir(dir.path())
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        assert!(spills.is_empty());
    }

    #[test]
    fn disabled_policy_passes_through_even_when_huge() {
        let policy = ToolOutputTruncatePolicy::disabled();
        let input = "x".repeat(50 * 1024);
        let out = truncate(&input, &policy, "call_a", None);
        assert!(!out.truncated);
        assert_eq!(out.content.len(), input.len());
        assert!(out.output_path.is_none());
    }

    #[test]
    fn truncates_50kb_to_32kb_preview_with_spill() {
        // 50 KB input → preview clipped under 32 KB, spill file holds the
        // full bytes, metadata reports the original size.
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input = "a".repeat(50 * 1024);
        let out = truncate(&input, &policy, "call_50k", None);

        assert!(out.truncated);
        // Preview must fit inside the byte cap (with a small margin for the
        // hint text).
        assert!(
            out.content.len() <= DEFAULT_MAX_BYTES + 1024,
            "preview exceeded 32 KB cap by more than the marker budget: {}",
            out.content.len()
        );
        assert!(out.original_bytes == 50 * 1024);
        assert!(out.output_path.is_some());

        // Spill file exists and matches the original bytes.
        let spill_path = dir.path().join(format!("tool_{}.txt", "call_50k"));
        assert!(
            spill_path.exists(),
            "spill file must be written on truncation"
        );
        let spilled = std::fs::read_to_string(&spill_path).unwrap();
        assert_eq!(spilled, input);

        // The hint must mention the spill path (relative path).
        assert!(
            out.content.contains("call_50k"),
            "hint must reference the spill file"
        );
        assert!(
            out.content.contains("Use `fs_grep`"),
            "hint must include recovery instructions"
        );
    }

    #[test]
    fn does_not_truncate_1000_lines_under_byte_cap() {
        // 1000 single-character lines = ~2 KB, far under both caps.
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input: String = (0..1000).map(|_| "x\n").collect();
        let out = truncate(&input, &policy, "call_1k", None);
        assert!(!out.truncated);
        assert_eq!(out.original_lines, 1000);
    }

    #[test]
    fn truncates_3000_lines_under_byte_cap() {
        // 3000 single-character lines = ~6 KB (under 32 KB byte cap) but
        // line cap (2000) fires first.
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input: String = (0..3000).map(|_| "x\n").collect();
        let out = truncate(&input, &policy, "call_3k", None);
        assert!(out.truncated, "line cap must trigger truncation");
        assert!(
            out.original_bytes < DEFAULT_MAX_BYTES,
            "byte cap should NOT have fired"
        );
        assert_eq!(out.original_lines, 3000);
        assert!(out.output_path.is_some());
    }

    #[test]
    fn truncation_handles_unicode_safely() {
        // Multi-byte chars near the cap boundary must not panic and must
        // produce a valid UTF-8 preview.
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        // Each "🦀" is 4 bytes; 10000 of them = 40 KB.
        let input: String = "🦀".repeat(10_000);
        let out = truncate(&input, &policy, "call_crab", None);
        assert!(out.truncated);
        // Just by reaching this point with no panic the char-boundary
        // logic is correct; double-check the preview is valid UTF-8.
        assert!(std::str::from_utf8(out.content.as_bytes()).is_ok());
    }

    #[test]
    fn spill_filename_is_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input = "x".repeat(50 * 1024);
        // Path-traversal id — must NOT escape the run dir.
        let out = truncate(&input, &policy, "../../etc/passwd", None);
        assert!(out.truncated);
        // The sanitizer replaces every non-alphanumeric (other than `-`/`_`)
        // with a literal underscore, so `../../etc/passwd` (7 invalid chars)
        // turns into `_______etc_passwd`. Check the file exists in the run
        // dir and that the *only* path components are `tool_*.txt` — i.e.
        // the spill never escaped via `..`.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one spill file expected");
        let name = &entries[0];
        assert!(name.starts_with("tool_") && name.ends_with(".txt"));
        assert!(!name.contains('/') && !name.contains('\\') && !name.contains(".."));
        assert!(name.contains("etc") && name.contains("passwd"));
    }

    #[test]
    fn relative_path_strips_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_with_dir(dir.path());
        let input = "x".repeat(50 * 1024);
        let out = truncate(&input, &policy, "call_rel", Some(dir.path()));
        assert!(out.truncated);
        let rel = out.output_path.unwrap();
        assert!(
            !rel.starts_with('/') && !rel.contains(':'),
            "output_path must be relative when workspace_root is given: {rel}"
        );
        assert!(rel.contains("call_rel"));
    }

    #[test]
    fn sweep_expired_deletes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(TOOL_OUTPUT_DIR_NAME).join("run-old");
        std::fs::create_dir_all(&run_dir).unwrap();
        let old_file = run_dir.join("tool_old.txt");
        std::fs::write(&old_file, b"old bytes").unwrap();
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options()
            .write(true)
            .open(&old_file)
            .unwrap();
        f.set_modified(ten_days_ago).unwrap();
        drop(f);

        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 1);
        assert!(!old_file.exists(), "old file must be deleted");
    }

    #[test]
    fn sweep_expired_keeps_fresh_files() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join(TOOL_OUTPUT_DIR_NAME).join("run-fresh");
        std::fs::create_dir_all(&run_dir).unwrap();
        let fresh_file = run_dir.join("tool_fresh.txt");
        std::fs::write(&fresh_file, b"fresh").unwrap();

        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 0);
        assert!(
            fresh_file.exists(),
            "fresh file must survive retention sweep"
        );
    }

    #[test]
    fn sweep_expired_missing_root_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let deleted = sweep_expired(dir.path(), 7).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn count_lines_matches_wc_l_semantics() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\nb\n"), 2);
        assert_eq!(count_lines("\n"), 1);
        assert_eq!(count_lines("\n\n"), 2);
    }
}
