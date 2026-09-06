// SPDX-License-Identifier: Apache-2.0

use crate::error::SandboxResult;
use crate::file_state_cache::{
    FileStateCache, GuardOutcome, check_guard_with_mtime, update_cache_after_write,
};
use crate::{SandboxError, Tool};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    check_sandbox_path_async, is_blocked_device_path, normalize_unsandboxed_path, reject_unc_path,
};

/// Search-and-replace editing tool for files.
///
/// Performs exact string matching (no regex). By default, the search string
/// must appear exactly once in the file (uniqueness guard). Set `replace_all`
/// to replace every occurrence.
///
/// Special case: when `old_string` is empty and the file does not exist (or
/// exists but is empty), the file is created/overwritten with `new_string`.
#[derive(Debug, Clone, Default)]
pub struct FsEditTool {
    sandbox_root: Option<PathBuf>,
    file_state_cache: Option<Arc<FileStateCache>>,
    /// When `true`, an additional multi-stage fuzzy-match cascade
    /// (line-trimmed + indentation-flexible) runs before the existing
    /// curly-quote / CRLF fallback. Opt-in per agent — default `false`
    /// preserves the historical "exact string, uniqueness-enforced"
    /// contract. See issue #755.
    fuzzy_match: bool,
}

impl FsEditTool {
    /// Create an unrestricted fs_edit tool (no sandbox check).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a sandboxed fs_edit tool. Paths must resolve within `root`.
    pub fn sandboxed(root: PathBuf) -> Self {
        Self {
            sandbox_root: Some(root),
            file_state_cache: None,
            fuzzy_match: false,
        }
    }

    /// Attach a file state cache for read-before-edit tracking.
    pub fn with_cache(mut self, cache: Arc<FileStateCache>) -> Self {
        self.file_state_cache = Some(cache);
        self
    }

    /// Enable or disable the fuzzy-match replacer cascade (issue #755).
    ///
    /// When `true`, the tool retries failed exact matches through a
    /// two-stage cascade (line-trimmed, then indentation-flexible)
    /// before falling back to the existing curly-quote / CRLF
    /// normalization. The uniqueness guard is preserved at every
    /// stage.
    pub fn with_fuzzy_match(mut self, enabled: bool) -> Self {
        self.fuzzy_match = enabled;
        self
    }
}

#[async_trait::async_trait]
impl Tool for FsEditTool {
    fn name(&self) -> &str {
        "fs_edit"
    }

    fn description(&self) -> &str {
        "Search and replace text in a file. Finds an exact string and replaces it. \
         By default the search string must appear exactly once (uniqueness guard). \
         Set replace_all to true to replace every occurrence. \
         When old_string is empty and the file does not exist, creates the file with new_string."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text (must differ from old_string)."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "When true, replace all occurrences instead of requiring uniqueness. Default: false."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'path' is required".to_string()))?;

        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'old_string' is required".to_string())
            })?;

        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SandboxError::InvalidParameters("'new_string' is required".to_string())
            })?;

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Block UNC paths that could leak NTLM credentials via SMB.
        // (Checked before parameter validation so UNC paths always get the
        // security error, not a misleading "old_string and new_string must
        // differ" response.)
        reject_unc_path(path)?;

        // Block device paths that could cause system damage.
        if is_blocked_device_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot edit device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        // No-op rejection: old_string == new_string
        if old_string == new_string {
            return Err(SandboxError::InvalidParameters(
                "old_string and new_string must differ".to_string(),
            ));
        }

        // Resolve path within sandbox (or normalize when unsandboxed).
        let resolved: PathBuf = if let Some(ref root) = self.sandbox_root {
            check_sandbox_path_async(path, root).await?
        } else {
            normalize_unsandboxed_path(path).await
        };

        // Also check the resolved path for device paths.
        if is_blocked_device_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Cannot edit device path '{}' — this is a system device, not a regular file",
                path
            )));
        }

        // Special case: empty old_string means "create file" — bypasses the
        // read-before-edit guard since there is no existing content to protect.
        if old_string.is_empty() {
            return self
                .handle_empty_old_string(&resolved, path, new_string)
                .await;
        }

        // Non-regular-file, size, and read-before-edit guards.
        // We capture the mtime here to avoid a redundant stat in the guard.
        const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;
        if !meta.is_file() {
            return Err(SandboxError::InvalidParameters(format!(
                "Path '{}' is not a regular file",
                path
            )));
        }
        if meta.len() > MAX_EDIT_BYTES {
            return Err(SandboxError::InvalidParameters(format!(
                "File too large for fs_edit ({} bytes, max {}). Use fs_write for full file replacement.",
                meta.len(),
                MAX_EDIT_BYTES
            )));
        }

        // Read-before-edit guard: the file must have been read via fs_read
        // before it can be edited. This prevents agents from blindly modifying
        // files they have not inspected.
        if let Some(ref cache) = self.file_state_cache
            && let Some(mtime) = meta.modified().ok()
        {
            match check_guard_with_mtime(cache, &resolved, mtime).await {
                GuardOutcome::Allowed => {}
                GuardOutcome::NotRead => {
                    return Err(SandboxError::InvalidParameters(
                        "File has not been read yet. Use fs_read to read the file \
                         before editing it."
                            .to_string(),
                    ));
                }
                GuardOutcome::StaleRead { reason } => {
                    return Err(SandboxError::InvalidParameters(reason));
                }
            }
        }

        // Read existing file content.
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to read '{}': {}", path, e)))?;

        // Detect the dominant line ending in the existing content so that a
        // CRLF-authored file keeps CRLF endings even if the agent supplies an
        // LF-only `new_string`.  Without this, splicing an LF replacement into
        // a CRLF file would produce mixed line endings — a common foot-gun on
        // Windows repos.
        let line_ending = detect_line_ending(&content);

        // Strip trailing whitespace from new_string (except for Markdown).
        let is_md = is_markdown_file(path);
        let new_string_clean = strip_trailing_whitespace(new_string, is_md, line_ending);
        let new_string = new_string_clean.as_str();

        // Count occurrences of the exact old_string.
        let count = content.matches(old_string).count();

        // If the direct match fails, walk the cascade in cheapest-first order.
        // The fuzzy-match cascade (line-trimmed, indentation-flexible) runs
        // only when the per-agent `fuzzy_match` flag is on (issue #755) — off
        // by default so existing agents keep the exact-string-only contract.
        //
        // Stage order:
        //   1. line-ending CRLF adaptation  (issue #733)
        //   2. line-trimmed match            (#755, fuzzy_match only)
        //   3. indentation-flexible match    (#755, fuzzy_match only)
        //   4. curly-quote / smart-quote normalization
        //
        // The resulting `actual_old` is always a real substring of `content`
        // so `content.replace` / `content.replacen` can splice the
        // replacement in without further translation.
        //
        // `fuzzy_hit` records whether stage 2 or 3 produced the match. The
        // fuzzy stages return a *count* of normalized-equivalent windows but
        // the returned `actual_old` is the byte slice of only the first
        // window. `content.replace(&actual_old, ...)` would then only match
        // byte-for-byte against that first window (the others typically have
        // different trailing whitespace / different indent and do not match
        // literally), so honoring `replace_all` here would silently lie about
        // the replacement count. We reject `replace_all=true` for fuzzy hits
        // when `count > 1` below.
        let (actual_old, count, quote_adapted, fuzzy_hit) = if count == 0 {
            let crlf_hit = line_ending_adapted_old(&content, old_string).and_then(|crlf_old| {
                let c = content.matches(&crlf_old).count();
                if c > 0 { Some((crlf_old, c)) } else { None }
            });

            if let Some((crlf_old, crlf_count)) = crlf_hit {
                (crlf_old, crlf_count, false, false)
            } else if self.fuzzy_match
                && let Some((matched, trimmed_count)) = try_line_trimmed_match(&content, old_string)
            {
                (matched.to_string(), trimmed_count, false, true)
            } else if self.fuzzy_match
                && let Some((matched, indent_count)) =
                    try_indentation_flexible_match(&content, old_string)
            {
                (matched.to_string(), indent_count, false, true)
            } else if let Some((original_match, norm_count)) =
                try_normalized_match(&content, old_string)
            {
                // Curly-quote-normalization fallback — handles the common case
                // where an LLM emits curly quotes while the file uses straight
                // quotes (or vice versa).
                (original_match.to_string(), norm_count, true, false)
            } else {
                return Err(SandboxError::InvalidParameters(format!(
                    "old_string not found in '{}'",
                    path
                )));
            }
        } else {
            (old_string.to_string(), count, false, false)
        };

        if !replace_all && count > 1 {
            return Err(SandboxError::InvalidParameters(format!(
                "old_string appears {} times in '{}'; set replace_all to true or provide a more unique string",
                count, path
            )));
        }

        // Fuzzy stages (line-trimmed, indentation-flexible) return a count of
        // normalized-equivalent windows but only the first window's raw byte
        // slice. A downstream `content.replace(&actual_old, ...)` would match
        // only that slice byte-for-byte, so honoring `replace_all=true` here
        // with `count > 1` would over-report the number of replacements. The
        // simple, honest rule: fuzzy stages are unique-or-fail, regardless of
        // `replace_all`. Exact / CRLF / curly-quote stages are unaffected.
        if fuzzy_hit && replace_all && count > 1 {
            return Err(SandboxError::InvalidParameters(format!(
                "fuzzy match found {} candidates in '{}'; replace_all is not supported for fuzzy stages — tighten the old_string for a unique match",
                count, path
            )));
        }

        // When using the quote-normalization fallback, adapt the new_string to
        // use the same quote style as the original matched text.
        let effective_new = if quote_adapted {
            adapt_quotes_to_original(new_string, &actual_old)
        } else {
            new_string.to_string()
        };

        // Perform the replacement.
        //
        // Note: when the quote-normalization path is active with replace_all,
        // `actual_old` is the exact original text from the *first* normalized
        // match. This assumes all occurrences share the same original quote
        // style (e.g. all use U+201C/U+201D). If a file mixed left and right
        // curly quotes inconsistently (e.g. U+201D used as an opening quote),
        // the reported replacement count could exceed the actual replacements.
        // This is effectively impossible in well-formed text.
        let new_content = if replace_all {
            content.replace(&actual_old, &effective_new)
        } else {
            // Replace only the first (and only) occurrence.
            content.replacen(&actual_old, &effective_new, 1)
        };

        // Write back.
        tokio::fs::write(&resolved, &new_content)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to write '{}': {}", path, e)))?;

        // Update the cache so subsequent edits/writes to the same file
        // pass the guard without requiring a re-read.
        if let Some(ref cache) = self.file_state_cache {
            update_cache_after_write(cache, &resolved).await;
        }

        let replacements = if replace_all { count } else { 1 };
        Ok(serde_json::json!({
            "ok": true,
            "path": path,
            "replacements": replacements
        }))
    }
}

impl FsEditTool {
    /// Handle the special case where `old_string` is empty.
    ///
    /// - File does not exist: create it with `new_string`.
    /// - File exists and is empty: write `new_string`.
    /// - File exists and has content: error.
    async fn handle_empty_old_string(
        &self,
        resolved: &Path,
        display_path: &str,
        new_string: &str,
    ) -> SandboxResult<Value> {
        let file_exists = tokio::fs::metadata(resolved).await.is_ok();

        if file_exists {
            // Reject directories (and other non-regular files) early with a
            // clear message instead of letting read_to_string surface an
            // OS-level I/O error.
            let meta = tokio::fs::metadata(resolved).await.map_err(|e| {
                SandboxError::Io(format!("Failed to read '{}': {}", display_path, e))
            })?;
            if !meta.is_file() {
                return Err(SandboxError::InvalidParameters(format!(
                    "Path '{}' is not a regular file",
                    display_path
                )));
            }

            let content = tokio::fs::read_to_string(resolved).await.map_err(|e| {
                SandboxError::Io(format!("Failed to read '{}': {}", display_path, e))
            })?;

            if !content.is_empty() {
                return Err(SandboxError::InvalidParameters(format!(
                    "old_string is empty but '{}' has existing content; \
                     use a non-empty old_string to edit, or clear the file first",
                    display_path
                )));
            }
        }

        // Create parent directories if needed.
        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                SandboxError::Io(format!(
                    "Failed to create dirs for '{}': {}",
                    display_path, e
                ))
            })?;
        }

        // New files have no existing content to sniff for a line-ending
        // convention, so default to LF.  `strip_trailing_whitespace` still
        // trims-and-rejoins with LF (and on Markdown paths normalizes line
        // endings only, preserving trailing whitespace), matching the
        // pre-existing file-creation behavior: any CRLF in `new_string` gets
        // converted to LF on new-file creation.
        let is_md = is_markdown_file(display_path);
        let cleaned = strip_trailing_whitespace(new_string, is_md, "\n");

        tokio::fs::write(resolved, &cleaned)
            .await
            .map_err(|e| SandboxError::Io(format!("Failed to write '{}': {}", display_path, e)))?;

        // Update the cache so subsequent writes/edits to the newly created file
        // pass the guard without requiring an fs_read.
        if let Some(ref cache) = self.file_state_cache {
            update_cache_after_write(cache, resolved).await;
        }

        Ok(serde_json::json!({
            "ok": true,
            "path": display_path,
            "replacements": 1
        }))
    }
}

// ── fs_edit helpers ────────────────────────────────────────────────────────

/// Normalize curly/smart quotes to straight ASCII equivalents.
///
/// Replaces:
/// - U+2018 (left single) and U+2019 (right single / apostrophe) -> `'`
/// - U+201C (left double) and U+201D (right double) -> `"`
fn normalize_quotes(s: &str) -> String {
    s.replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
}

/// Return the byte length a character occupies after quote normalization,
/// without allocating a String.  Curly quotes (3 bytes in UTF-8) normalize
/// to their straight equivalents (1 byte); all other characters are unchanged.
fn normalized_char_len(ch: char) -> usize {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' => 1,
        _ => ch.len_utf8(),
    }
}

/// Detect the dominant line ending in `content`.
///
/// Returns `"\r\n"` if CRLF occurrences outnumber or equal bare-LF occurrences
/// among all newlines, otherwise `"\n"`.  For files with no newlines at all
/// (or pure-LF content) the result is `"\n"`.
///
/// The goal is to preserve the file's existing line-ending convention when
/// writing back — splicing an LF replacement into a CRLF file produces mixed
/// line endings, which is a foot-gun on Windows.
fn detect_line_ending(content: &str) -> &'static str {
    let crlf_count = content.matches("\r\n").count();
    // Every "\r\n" also matches "\n", so subtract to get bare-LF-only count.
    let lf_count = content.matches('\n').count().saturating_sub(crlf_count);
    if crlf_count > 0 && crlf_count >= lf_count {
        "\r\n"
    } else {
        "\n"
    }
}

/// If `content` uses CRLF line endings and `old_string` contains bare LFs (no
/// CRLF), return a copy of `old_string` with every bare `\n` rewritten to
/// `\r\n` so it can match the file's actual bytes.
///
/// Returns `None` when the rewrite would be a no-op (e.g. the file is pure-LF,
/// or `old_string` already contains `\r\n`, or `old_string` has no newlines
/// at all).  Callers should fall back to other matching strategies in that
/// case.
///
/// Motivation: `fs_read` strips `\r` when buffering for the LLM, so models
/// tend to emit LF-only `old_string` arguments even when the file on disk is
/// CRLF — without this fallback, a perfectly reasonable edit request fails
/// with "old_string not found".  See issue #733.
fn line_ending_adapted_old(content: &str, old_string: &str) -> Option<String> {
    if detect_line_ending(content) != "\r\n" {
        return None;
    }
    if !old_string.contains('\n') {
        return None;
    }
    if old_string.contains("\r\n") {
        // Already using CRLF — nothing to rewrite.  (A mixed LF/CRLF
        // `old_string` is effectively user error; we don't try to massage it.)
        return None;
    }
    Some(old_string.replace('\n', "\r\n"))
}

/// Rejoin every logical line of `s` with `line_ending`, normalizing any
/// pre-existing CRLF or bare-LF separators to the target convention.
///
/// This is the line-ending half of the replacement pipeline, factored out so
/// it can run for *every* file type — including Markdown, where the
/// trailing-whitespace strip is deliberately skipped but line-ending
/// normalization is still required (otherwise an LF `new_string` spliced into
/// a CRLF `.md` file would produce mixed endings; see issue #733).
///
/// `str::lines()` consumes both `\r\n` and `\n` separators and does not yield
/// a trailing empty element for strings ending in a newline, so we re-append
/// `line_ending` when the input had a trailing newline.  Embedded bare `\r`
/// (e.g. within a line, not as a separator) is preserved because `lines()`
/// does not treat bare `\r` as a line break on its own.
fn normalize_line_endings(s: &str, line_ending: &str) -> String {
    let mut result = s.lines().collect::<Vec<_>>().join(line_ending);
    if s.ends_with('\n') {
        result.push_str(line_ending);
    }
    result
}

/// Strip trailing whitespace (spaces and tabs) from each line, rejoining with
/// `line_ending` so the replacement preserves the surrounding file's
/// line-ending convention.
///
/// Markdown files (`.md`, `.markdown`) are exempt from the *trailing-whitespace*
/// strip because trailing spaces are semantically meaningful as hard line
/// breaks per CommonMark.  They are **not** exempt from line-ending
/// normalization — see `normalize_line_endings` above — so a CRLF-authored
/// `.md` file edited with an LF `new_string` still ends up uniformly CRLF.
///
/// We deliberately use `trim_end_matches([' ', '\t'])` instead of
/// `trim_end()` so that any intentional `\r` embedded in a line survives —
/// `trim_end()` classifies `\r` as whitespace and would drop it.
fn strip_trailing_whitespace(s: &str, is_markdown: bool, line_ending: &str) -> String {
    // Line-ending normalization runs for every file type, including Markdown.
    // Only the per-line trailing-whitespace strip is skipped for Markdown.
    if is_markdown {
        return normalize_line_endings(s, line_ending);
    }
    let trimmed_lines: Vec<&str> = s
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect();
    let mut result = trimmed_lines.join(line_ending);
    // str::lines() does not yield a trailing empty element for strings ending
    // with '\n' (or "\r\n"), so the join silently drops the final newline.
    // Re-append it (using the detected line ending) so code files keep their
    // trailing newline in the file's native convention.
    if s.ends_with('\n') {
        result.push_str(line_ending);
    }
    result
}

/// Returns `true` if `path` refers to a Markdown file (`.md` or `.markdown`).
fn is_markdown_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// Attempt a quote-normalized match when the exact `old_string` is not found.
///
/// Returns `Some((original_match_text, normalized_match_count))` if a match is
/// found after normalizing both the content and the search string.  The returned
/// `original_match_text` is the **un-normalized** text from the file that
/// corresponds to the first normalized match, so the replacement preserves the
/// file's existing quote style.
fn try_normalized_match<'a>(content: &'a str, old_string: &str) -> Option<(&'a str, usize)> {
    let norm_content = normalize_quotes(content);
    let norm_old = normalize_quotes(old_string);

    // If normalization didn't change anything, skip (direct match already
    // failed, so the normalized version would fail too).
    if norm_content == content && norm_old == old_string {
        return None;
    }

    let count = norm_content.matches(&norm_old).count();
    if count == 0 {
        return None;
    }

    // Find the byte offset of the first match in the *normalized* content, then
    // map that back to the original content.  Because curly quotes are multi-byte
    // (3 bytes in UTF-8) while straight quotes are single-byte, we need to walk
    // both strings in parallel to compute the correct original offset.
    let norm_offset = norm_content.find(&norm_old)?;

    // Map normalized byte offset -> original byte offset.
    // Walk char-by-char through the original and normalized simultaneously.
    let mut orig_byte = 0;
    let mut norm_byte = 0;
    for ch in content.chars() {
        if norm_byte >= norm_offset {
            break;
        }
        let norm_ch_len = normalized_char_len(ch);
        orig_byte += ch.len_utf8();
        norm_byte += norm_ch_len;
    }

    // Now compute the length of the original text that corresponds to the
    // normalized match.  Walk from orig_byte forward while consuming
    // norm_old.len() normalized bytes.
    let mut orig_end = orig_byte;
    let mut consumed_norm = 0;
    let target_norm_len = norm_old.len();
    for ch in content[orig_byte..].chars() {
        if consumed_norm >= target_norm_len {
            break;
        }
        let norm_ch_len = normalized_char_len(ch);
        orig_end += ch.len_utf8();
        consumed_norm += norm_ch_len;
    }

    let original_match = &content[orig_byte..orig_end];
    Some((original_match, count))
}

/// Adapt `new_string` to use the same quote style as the matched original text.
///
/// For each straight quote in `new_string`, if the original matched text
/// contained curly quotes in corresponding positions, convert straight quotes
/// in `new_string` to the curly style found in the original.
///
/// This is best-effort: when the original used curly quotes, we convert all
/// straight single quotes to right-single-quote (U+2019, the most common for
/// apostrophes and single-quoted text) and all straight double quotes to
/// left-double-quote (U+201C) / right-double-quote (U+201D) alternating.
fn adapt_quotes_to_original(new_string: &str, original_match: &str) -> String {
    // If the original contains no curly quotes, return new_string unchanged.
    let has_curly_single =
        original_match.contains('\u{2018}') || original_match.contains('\u{2019}');
    let has_curly_double =
        original_match.contains('\u{201C}') || original_match.contains('\u{201D}');

    if !has_curly_single && !has_curly_double {
        return new_string.to_string();
    }

    let mut result = String::with_capacity(new_string.len() * 2);
    let mut double_quote_open = true; // toggle for left/right double quotes

    for ch in new_string.chars() {
        match ch {
            '\'' if has_curly_single => {
                // Use right single quote (U+2019) — works for apostrophes and
                // closing single quotes, which are the overwhelmingly common case.
                result.push('\u{2019}');
            }
            '"' if has_curly_double => {
                if double_quote_open {
                    result.push('\u{201C}');
                } else {
                    result.push('\u{201D}');
                }
                double_quote_open = !double_quote_open;
            }
            _ => result.push(ch),
        }
    }

    result
}

// ── Fuzzy-match replacer cascade (issue #755) ──────────────────────────────
//
// These helpers implement the two stages of the fuzzy replacer cascade
// that we deemed the highest-value ports (see the issue body). They are
// invoked only when the per-agent `fuzzy_match` flag is on — when it is off
// they are never called and `fs_edit` retains its exact-string-only behavior.
//
// Each stage returns `Some((matched_slice, count))` where `matched_slice` is
// a &str pointing back into the original `content` so the caller can splice
// via `content.replace` / `content.replacen` unchanged. `count` is the total
// number of distinct candidate matches; the outer uniqueness guard in
// `execute()` enforces `count == 1` (unless `replace_all` is true).

/// Stage 2: line-trimmed match.
///
/// Split both `content` and `old_string` into lines, right-trim each line
/// (spaces + tabs), then walk `content`'s lines looking for a contiguous
/// window whose right-trimmed sequence equals the needle's right-trimmed
/// sequence. Returns the exact original slice of `content` spanning the
/// first such match, plus the total number of non-overlapping candidate
/// windows (used by the outer uniqueness guard).
///
/// Catches the extremely common LLM foot-gun where the model's `old_string`
/// includes or omits trailing whitespace that the file does not match.
fn try_line_trimmed_match<'a>(content: &'a str, old_string: &str) -> Option<(&'a str, usize)> {
    // Byte offsets of each line start + its trailing line-terminator length
    // (0 for the final line if the file has no trailing newline). Using byte
    // offsets lets us return a `&str` into `content` at the end.
    let content_lines = split_lines_with_offsets(content);
    let needle_lines = split_lines_with_offsets(old_string);

    if needle_lines.is_empty() {
        return None;
    }

    // Trim right of each line for comparison.
    let content_trimmed: Vec<&str> = content_lines
        .iter()
        .map(|(_, line, _)| line.trim_end_matches([' ', '\t']))
        .collect();
    let needle_trimmed: Vec<&str> = needle_lines
        .iter()
        .map(|(_, line, _)| line.trim_end_matches([' ', '\t']))
        .collect();

    // If the trimmed sequences would be a no-op (identical to un-trimmed on
    // both sides), the exact match would already have succeeded, so skip.
    let content_unchanged = content_lines
        .iter()
        .zip(content_trimmed.iter())
        .all(|((_, orig, _), trimmed)| orig == trimmed);
    let needle_unchanged = needle_lines
        .iter()
        .zip(needle_trimmed.iter())
        .all(|((_, orig, _), trimmed)| orig == trimmed);
    if content_unchanged && needle_unchanged {
        return None;
    }

    // Slide the needle across content, recording every matching window.
    let n = needle_trimmed.len();
    let mut matches: Vec<(usize, usize)> = Vec::new();
    if content_trimmed.len() < n {
        return None;
    }
    for start_line in 0..=content_trimmed.len().saturating_sub(n) {
        if content_trimmed[start_line..start_line + n] == needle_trimmed[..] {
            // Compute the original byte range for this window:
            // start = byte offset of line `start_line`
            // end   = byte offset of line `start_line + n - 1` + its raw line length
            //         (without its trailing line terminator — we want to match
            //          the span that corresponds to the needle's lines, not
            //          include the separator after the final line of the
            //          window).
            let (start_byte, _, _) = content_lines[start_line];
            let (last_line_start, last_line, _term_len) = content_lines[start_line + n - 1];
            let end_byte = last_line_start + last_line.len();
            matches.push((start_byte, end_byte));
        }
    }

    if matches.is_empty() {
        return None;
    }

    let (first_start, first_end) = matches[0];
    Some((&content[first_start..first_end], matches.len()))
}

/// Stage 3: indentation-flexible match.
///
/// Strips the common leading indent (shared by every non-blank line) from the
/// needle, then for each candidate window in `content` strips the window's
/// own common leading indent and compares the two normalized sequences. This
/// catches "LLM emitted 2-space indent while the file uses 4" without
/// disturbing relative indentation inside the block.
///
/// Returns the exact original slice of `content` so the caller can splice the
/// replacement while leaving the surrounding file byte-identical outside the
/// matched range.
fn try_indentation_flexible_match<'a>(
    content: &'a str,
    old_string: &str,
) -> Option<(&'a str, usize)> {
    let content_lines = split_lines_with_offsets(content);
    let needle_lines = split_lines_with_offsets(old_string);

    if needle_lines.is_empty() {
        return None;
    }

    // Strip common leading indent from the needle's non-blank lines.
    let needle_raw: Vec<&str> = needle_lines.iter().map(|(_, line, _)| *line).collect();
    let needle_indent = common_leading_indent(&needle_raw);
    let needle_stripped: Vec<String> = needle_raw
        .iter()
        .map(|line| strip_prefix_if_present(line, &needle_indent).to_string())
        .collect();

    // Note: even when `needle_indent` is empty, this stage is still useful —
    // the *content* may have more indent than the needle (e.g. needle has 0
    // leading spaces, content block has 4). We always continue past this
    // point and let the per-window stripping below catch that case.

    let n = needle_stripped.len();
    if content_lines.len() < n {
        return None;
    }

    let mut matches: Vec<(usize, usize)> = Vec::new();
    for start_line in 0..=content_lines.len().saturating_sub(n) {
        let window_raw: Vec<&str> = content_lines[start_line..start_line + n]
            .iter()
            .map(|(_, line, _)| *line)
            .collect();
        let window_indent = common_leading_indent(&window_raw);
        let window_stripped: Vec<String> = window_raw
            .iter()
            .map(|line| strip_prefix_if_present(line, &window_indent).to_string())
            .collect();

        if window_stripped == needle_stripped {
            let (start_byte, _, _) = content_lines[start_line];
            let (last_line_start, last_line, _term_len) = content_lines[start_line + n - 1];
            let end_byte = last_line_start + last_line.len();
            matches.push((start_byte, end_byte));
        }
    }

    if matches.is_empty() {
        return None;
    }

    let (first_start, first_end) = matches[0];
    Some((&content[first_start..first_end], matches.len()))
}

/// Split `s` into `(line_start_byte, line_content_without_terminator, terminator_len)`
/// tuples. Recognizes both `\n` and `\r\n` as line terminators. The final
/// line is emitted even when `s` does not end with a newline (terminator_len
/// is zero in that case). An empty input yields an empty vec — callers that
/// need "at least one line" semantics must handle that explicitly.
fn split_lines_with_offsets(s: &str) -> Vec<(usize, &str, usize)> {
    let mut out: Vec<(usize, &str, usize)> = Vec::new();
    if s.is_empty() {
        return out;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut line_start = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            // Detect CRLF vs bare LF by peeking behind.
            let (content_end, term_len) = if i > 0 && bytes[i - 1] == b'\r' {
                (i - 1, 2)
            } else {
                (i, 1)
            };
            // SAFETY: byte slice boundaries align with UTF-8 code points
            // because `\r` and `\n` are single-byte ASCII and we slice only
            // at those markers.
            let line = &s[line_start..content_end];
            out.push((line_start, line, term_len));
            line_start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    // Trailing line without a terminator.
    if line_start < bytes.len() {
        out.push((line_start, &s[line_start..], 0));
    }
    out
}

/// Common leading indent across all **non-blank** lines (spaces + tabs).
///
/// Blank lines are ignored because they typically contain no indent and
/// would otherwise force the common indent to the empty string. When every
/// line is blank the returned indent is `""`.
fn common_leading_indent(lines: &[&str]) -> String {
    let non_blank: Vec<&str> = lines
        .iter()
        .filter(|l| !l.chars().all(|c| c == ' ' || c == '\t' || c == '\r'))
        .copied()
        .collect();
    if non_blank.is_empty() {
        return String::new();
    }

    // Start the candidate from the first non-blank line's leading indent,
    // then shrink it down to the common prefix with every other non-blank
    // line's leading indent.
    let mut prefix: String = leading_indent(non_blank[0]).to_string();
    for line in &non_blank[1..] {
        let other = leading_indent(line);
        // Truncate `prefix` to the longest shared prefix with `other`.
        let shared_len = prefix
            .bytes()
            .zip(other.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(shared_len);
        if prefix.is_empty() {
            break;
        }
    }
    prefix
}

/// Return the leading run of space+tab characters at the start of `line`.
fn leading_indent(line: &str) -> &str {
    let end = line
        .bytes()
        .position(|b| b != b' ' && b != b'\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// Return `line` with `prefix` stripped from its front if present; otherwise
/// return `line` unchanged. Blank lines that don't contain `prefix` are
/// passed through as-is so they normalize to `""`-ish representation.
fn strip_prefix_if_present<'a>(line: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        line
    } else if let Some(rest) = line.strip_prefix(prefix) {
        rest
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fs_edit_unique_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn test_fs_edit_reject_multiple_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "aaa bbb aaa ccc aaa").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "xxx"
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("3 times"),
            "error should mention count, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "aaa bbb aaa ccc aaa").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "xxx",
                "replace_all": true
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 3);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "xxx bbb xxx ccc xxx"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "missing",
                "new_string": "replacement"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_fs_edit_noop_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "world"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must differ"));
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.txt");
        assert!(!path.exists());

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "brand new content"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "brand new content");
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_on_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "new content"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_on_nonempty_file_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "existing content").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "replacement"
            }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("existing content"));
    }

    #[tokio::test]
    async fn test_fs_edit_sandbox_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tool = FsEditTool::sandboxed(root);
        let result = tool
            .execute(serde_json::json!({
                "path": "../../etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("outside sandbox"));
    }

    /// Regression guard for #744: the hardcoded denied-filename check was
    /// removed from `fs_edit`. Editing a file whose basename is
    /// `secrets.json` must no longer be rejected for that reason alone.
    /// The read-before-edit guard still applies, so this test uses
    /// `old_string == ""` (the create-file special case) to prove the
    /// deny check itself is gone without fighting the guard.
    #[tokio::test]
    async fn test_fs_edit_no_hardcoded_secrets_json_deny() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": secrets.to_str().unwrap(),
                "old_string": "",
                "new_string": "{\"k\": \"v\"}"
            }))
            .await;
        // Empty old_string creates the file; must succeed now that
        // the hardcoded secrets.json deny check is gone.
        assert!(
            result.is_ok(),
            "fs_edit create of 'secrets.json' must no longer be \
             blocked by a hardcoded deny list (got: {:?})",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_fs_edit_preserves_surrounding_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "before TARGET after").unwrap();

        let tool = FsEditTool::new();
        tool.execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "TARGET",
            "new_string": "REPLACED"
        }))
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "before REPLACED after"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "line1\nold_line2\nold_line3\nline4").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "old_line2\nold_line3",
                "new_string": "new_line2\nnew_line3\nnew_extra"
            }))
            .await
            .unwrap();

        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nnew_line2\nnew_line3\nnew_extra\nline4"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unicode.txt");
        std::fs::write(&path, "Hej v\u{00e4}rlden! \u{1f600} foo").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "v\u{00e4}rlden! \u{1f600}",
                "new_string": "\u{4e16}\u{754c}"
            }))
            .await
            .unwrap();

        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Hej \u{4e16}\u{754c} foo"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_rejects_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let content = "x".repeat(2 * 1024 * 1024 + 1);
        std::fs::write(&path, &content).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "x",
                "new_string": "y",
                "replace_all": true
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "error should mention size limit, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_rejects_unc_path() {
        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": "\\\\server\\share\\file.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("UNC paths"), "error should mention UNC: {err}");

        let result = tool
            .execute(serde_json::json!({
                "path": "//server/share/file.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("UNC paths"));
    }

    #[tokio::test]
    async fn test_fs_edit_blocked_device_path() {
        let device = if cfg!(unix) { "/dev/zero" } else { "NUL" };
        let result = FsEditTool::new()
            .execute(serde_json::json!({
                "path": device,
                "old_string": "foo",
                "new_string": "bar"
            }))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cannot edit device path"),
            "expected device path error, got: {err_msg}"
        );
        assert!(
            err_msg.contains("system device"),
            "expected 'system device' in error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_directory_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = FsEditTool::new()
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(),
                "old_string": "foo",
                "new_string": "bar"
            }))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not a regular file"),
            "expected 'not a regular file', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_string_directory_returns_not_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = FsEditTool::new()
            .execute(serde_json::json!({
                "path": dir.path().to_str().unwrap(),
                "old_string": "",
                "new_string": "content"
            }))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not a regular file"),
            "expected 'not a regular file', got: {err_msg}"
        );
    }

    // ── Quote normalization tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_fs_edit_curly_old_matches_straight_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, r#"let msg = "hello world";"#).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "let msg = \u{201C}hello world\u{201D};",
                "new_string": "let msg = \"goodbye world\";"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"let msg = "goodbye world";"#
        );
    }

    #[tokio::test]
    async fn test_fs_edit_straight_old_matches_curly_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prose.txt");
        std::fs::write(&path, "She said \u{201C}hello\u{201D} to him.").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "She said \"hello\" to him.",
                "new_string": "She said \"goodbye\" to him."
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "She said \u{201C}goodbye\u{201D} to him."
        );
    }

    #[tokio::test]
    async fn test_fs_edit_quote_normalization_preserves_original_style() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "It\u{2019}s a fine day.").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "It's a fine day.",
                "new_string": "It's a great day."
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "It\u{2019}s a great day."
        );
    }

    #[tokio::test]
    async fn test_fs_edit_quote_normalization_enforces_uniqueness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(
            &path,
            "said \u{201C}hello\u{201D} and said \u{201C}hello\u{201D}",
        )
        .unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "said \"hello\"",
                "new_string": "said \"goodbye\""
            }))
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("2 times"),
            "should mention 2 matches, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_direct_match_takes_priority() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "say \"hello\" and \u{201C}hello\u{201D}").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "say \"hello\"",
                "new_string": "say \"bye\""
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "say \"bye\" and \u{201C}hello\u{201D}"
        );
    }

    // ── Trailing whitespace stripping tests ─────────────────────────────────

    #[tokio::test]
    async fn test_fs_edit_strips_trailing_whitespace_rs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, "fn main() {}").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "fn main() {}",
                "new_string": "fn main() {  \n    println!(\"hi\");  \n}  "
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"hi\");\n}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_preserves_trailing_whitespace_md() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "# Title").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "# Title",
                "new_string": "# Title  \nSome text  "
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Title  \nSome text  "
        );
    }

    #[tokio::test]
    async fn test_fs_edit_strips_trailing_whitespace_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[package]").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "[package]",
                "new_string": "[package]  \nname = \"test\"  "
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[package]\nname = \"test\""
        );
    }

    #[tokio::test]
    async fn test_fs_edit_replace_all_with_quote_normalization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(
            &path,
            "say \u{201C}hi\u{201D} then \u{201C}hi\u{201D} then \u{201C}hi\u{201D}",
        )
        .unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "\"hi\"",
                "new_string": "\"bye\"",
                "replace_all": true
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 3);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "say \u{201C}bye\u{201D} then \u{201C}bye\u{201D} then \u{201C}bye\u{201D}"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_strips_trailing_whitespace_preserves_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");
        std::fs::write(&path, "fn main() {}").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "fn main() {}",
                "new_string": "fn main() {  \n    println!(\"hi\");  \n}  \n"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"hi\");\n}\n"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_normalization_and_stripping_combined() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "let msg = \u{201C}hello\u{201D};").unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "let msg = \"hello\";",
                "new_string": "let msg = \"world\";  "
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "let msg = \u{201C}world\u{201D};"
        );
    }

    #[tokio::test]
    async fn test_fs_edit_empty_old_string_strips_trailing_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.rs");

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "",
                "new_string": "fn main() {  \n    println!(\"hi\");  \n}  \n"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"hi\");\n}\n"
        );
    }

    // ── Helper unit tests ─────────────────────────────────────────────────

    #[test]
    fn test_normalize_quotes() {
        assert_eq!(normalize_quotes("\u{201C}hi\u{201D}"), "\"hi\"");
        assert_eq!(normalize_quotes("it\u{2019}s"), "it's");
        assert_eq!(normalize_quotes("\u{2018}a\u{2019}"), "'a'");
        assert_eq!(normalize_quotes("plain text"), "plain text");
    }

    #[test]
    fn test_strip_trailing_whitespace_non_markdown() {
        assert_eq!(
            strip_trailing_whitespace("hello  \nworld\t\n  ok  ", false, "\n"),
            "hello\nworld\n  ok"
        );
    }

    #[test]
    fn test_strip_trailing_whitespace_markdown_preserved() {
        let input = "hello  \nworld\t\n  ok  ";
        assert_eq!(strip_trailing_whitespace(input, true, "\n"), input);
    }

    #[test]
    fn test_strip_trailing_whitespace_preserves_trailing_newline() {
        assert_eq!(
            strip_trailing_whitespace("hello  \nworld\n", false, "\n"),
            "hello\nworld\n"
        );
        // CRLF input with CRLF line_ending: trailing whitespace stripped, CRLF preserved.
        assert_eq!(
            strip_trailing_whitespace("hello  \r\nworld\r\n", false, "\r\n"),
            "hello\r\nworld\r\n"
        );
        assert_eq!(
            strip_trailing_whitespace("hello  \nworld", false, "\n"),
            "hello\nworld"
        );
    }

    #[test]
    fn test_strip_trailing_whitespace_crlf_normalizes_lf_input() {
        // LF-authored replacement + CRLF target file ⇒ output should be CRLF so
        // splicing into the CRLF file does not produce mixed endings.
        assert_eq!(
            strip_trailing_whitespace("ALPHA\nBETA\n", false, "\r\n"),
            "ALPHA\r\nBETA\r\n"
        );
    }

    #[test]
    fn test_detect_line_ending_pure_lf() {
        assert_eq!(detect_line_ending("alpha\nbeta\ngamma\n"), "\n");
    }

    #[test]
    fn test_detect_line_ending_pure_crlf() {
        assert_eq!(detect_line_ending("alpha\r\nbeta\r\ngamma\r\n"), "\r\n");
    }

    #[test]
    fn test_detect_line_ending_no_newlines() {
        assert_eq!(detect_line_ending("no newlines here"), "\n");
        assert_eq!(detect_line_ending(""), "\n");
    }

    #[test]
    fn test_detect_line_ending_mixed_crlf_dominant() {
        // 2 CRLF vs 1 bare LF ⇒ CRLF wins.
        assert_eq!(detect_line_ending("a\r\nb\r\nc\nd"), "\r\n");
    }

    #[test]
    fn test_detect_line_ending_mixed_lf_dominant() {
        // 1 CRLF vs 2 bare LF ⇒ LF wins.
        assert_eq!(detect_line_ending("a\r\nb\nc\nd"), "\n");
    }

    #[test]
    fn test_line_ending_adapted_old_rewrites_lf_to_crlf() {
        let content = "alpha\r\nbeta\r\ngamma\r\n";
        let rewritten = line_ending_adapted_old(content, "alpha\nbeta").unwrap();
        assert_eq!(rewritten, "alpha\r\nbeta");
    }

    #[test]
    fn test_line_ending_adapted_old_skips_pure_lf_content() {
        // LF-authored content ⇒ nothing to rewrite; direct match already works.
        assert!(line_ending_adapted_old("alpha\nbeta\n", "alpha\nbeta").is_none());
    }

    #[test]
    fn test_line_ending_adapted_old_skips_when_old_string_has_no_newlines() {
        assert!(line_ending_adapted_old("alpha\r\nbeta\r\n", "alpha").is_none());
    }

    #[test]
    fn test_line_ending_adapted_old_skips_when_old_string_already_crlf() {
        // If the agent already supplied CRLF, direct match would have worked —
        // rewriting again would double-CRLF it.  Bail out.
        assert!(line_ending_adapted_old("alpha\r\nbeta\r\n", "alpha\r\nbeta").is_none());
    }

    #[tokio::test]
    async fn test_fs_edit_preserves_crlf_line_endings() {
        // Regression test for issue #733: fs_edit must preserve the file's
        // existing line-ending convention when the agent supplies an LF-only
        // new_string against a CRLF-authored file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crlf.txt");
        let initial = "alpha\r\nbeta\r\ngamma\r\n";
        std::fs::write(&path, initial).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "alpha\nbeta",
                "new_string": "ALPHA\nBETA"
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written, "ALPHA\r\nBETA\r\ngamma\r\n",
            "expected uniformly-CRLF output, got: {:?}",
            written
        );
        // Explicitly assert no mixed endings — every LF must be preceded by CR.
        assert!(
            !written
                .char_indices()
                .any(|(i, c)| c == '\n' && !written[..i].ends_with('\r')),
            "file contains mixed line endings: {:?}",
            written
        );
    }

    #[tokio::test]
    async fn test_fs_edit_preserves_lf_line_endings() {
        // Inverse regression: pure-LF file stays pure-LF even if the agent's
        // new_string contains no newlines.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lf.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let tool = FsEditTool::new();
        tool.execute(serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "beta",
            "new_string": "BETA"
        }))
        .await
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "alpha\nBETA\ngamma\n");
        assert!(!written.contains('\r'));
    }

    #[tokio::test]
    async fn test_fs_edit_markdown_crlf_preserves_endings_and_trailing_whitespace() {
        // Regression for Tim's review on PR #737: Markdown files used to bypass
        // line-ending normalization entirely, so a CRLF-authored `.md` file
        // edited with an LF `new_string` would produce mixed line endings —
        // the exact failure mode #733 was filed to fix, just hitting the
        // Markdown path instead.  Now Markdown skips only the per-line
        // trailing-whitespace strip and still normalizes line endings to the
        // file's detected convention.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        // "# Heading  " has two trailing spaces (CommonMark hard line break).
        let initial = "# Heading  \r\nparagraph with trailing  \r\nlast\r\n";
        std::fs::write(&path, initial).unwrap();

        let tool = FsEditTool::new();
        let new_string_lf = "para\nupdated"; // LF-only, as a model would emit
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "paragraph with trailing",
                "new_string": new_string_lf
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["replacements"], 1);
        let written = std::fs::read_to_string(&path).unwrap();

        // (a) Output must be uniformly CRLF — no mixed endings.
        assert!(
            !written
                .char_indices()
                .any(|(i, c)| c == '\n' && !written[..i].ends_with('\r')),
            "file contains mixed line endings: {:?}",
            written
        );
        // The LF separator inside new_string must have been normalized to CRLF.
        assert!(
            written.contains("para\r\nupdated"),
            "expected replacement rejoined with CRLF, got: {:?}",
            written
        );

        // (b) Trailing whitespace on the heading must be preserved (the
        // per-line strip is skipped for Markdown).
        assert!(
            written.contains("# Heading  \r\n"),
            "expected Markdown hard-break trailing spaces preserved, got: {:?}",
            written
        );
        // Concretely, the exact expected file content:
        assert_eq!(written, "# Heading  \r\npara\r\nupdated  \r\nlast\r\n");
    }

    #[test]
    fn test_normalize_line_endings_lf_to_crlf() {
        assert_eq!(
            normalize_line_endings("alpha\nbeta\n", "\r\n"),
            "alpha\r\nbeta\r\n"
        );
    }

    #[test]
    fn test_normalize_line_endings_crlf_to_lf() {
        assert_eq!(
            normalize_line_endings("alpha\r\nbeta\r\n", "\n"),
            "alpha\nbeta\n"
        );
    }

    #[test]
    fn test_normalize_line_endings_preserves_trailing_whitespace() {
        // `normalize_line_endings` must only touch separators, not per-line content.
        assert_eq!(
            normalize_line_endings("a  \nb\t\n", "\r\n"),
            "a  \r\nb\t\r\n"
        );
    }

    #[test]
    fn test_normalize_line_endings_no_trailing_newline() {
        assert_eq!(
            normalize_line_endings("alpha\nbeta", "\r\n"),
            "alpha\r\nbeta"
        );
    }

    #[test]
    fn test_strip_trailing_whitespace_markdown_normalizes_endings() {
        // Markdown path still normalizes CRLF ↔ LF even though trailing
        // whitespace is preserved (regression test for PR #737 review).
        assert_eq!(
            strip_trailing_whitespace("a  \nb  \n", true, "\r\n"),
            "a  \r\nb  \r\n"
        );
    }

    #[test]
    fn test_is_markdown_file() {
        assert!(is_markdown_file("notes.md"));
        assert!(is_markdown_file("README.MD"));
        assert!(is_markdown_file("docs/file.markdown"));
        assert!(!is_markdown_file("code.rs"));
        assert!(!is_markdown_file("data.toml"));
    }

    #[test]
    fn test_try_normalized_match_curly_to_straight() {
        let content = r#"She said "hello" to him."#;
        let old_string = "She said \u{201C}hello\u{201D} to him.";
        let result = try_normalized_match(content, old_string);
        assert!(result.is_some());
        let (matched, count) = result.unwrap();
        assert_eq!(matched, r#"She said "hello" to him."#);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_try_normalized_match_straight_to_curly() {
        let content = "She said \u{201C}hello\u{201D} to him.";
        let old_string = r#"She said "hello" to him."#;
        let result = try_normalized_match(content, old_string);
        assert!(result.is_some());
        let (matched, count) = result.unwrap();
        assert_eq!(matched, "She said \u{201C}hello\u{201D} to him.");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_try_normalized_match_no_match() {
        let content = "no quotes here";
        let old_string = "missing text";
        assert!(try_normalized_match(content, old_string).is_none());
    }

    #[test]
    fn test_adapt_quotes_preserves_straight_when_original_is_straight() {
        let result = adapt_quotes_to_original("say \"hi\"", "say \"hi\"");
        assert_eq!(result, "say \"hi\"");
    }

    #[test]
    fn test_adapt_quotes_converts_to_curly_when_original_is_curly() {
        let result = adapt_quotes_to_original("say \"goodbye\"", "say \u{201C}hello\u{201D}");
        assert_eq!(result, "say \u{201C}goodbye\u{201D}");
    }

    // ── Fuzzy-match cascade (issue #755) ──────────────────────────────────

    /// Acceptance criterion #1: indent-drift match HIT.
    ///
    /// File uses 4-space indent; the agent supplies an `old_string` with
    /// 2-space indent.  With `fuzzy_match = true`, the indentation-flexible
    /// stage detects the common-leading-indent delta and succeeds with a
    /// unique match.  Replacement preserves the file's original 4-space
    /// indentation for surrounding context (we splice only the matched byte
    /// range).
    #[tokio::test]
    async fn test_fs_edit_fuzzy_indent_drift_hit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.py");
        // File uses 4-space indentation.
        let original = "def greet():\n    print(\"hi\")\n    return 1\n";
        std::fs::write(&path, original).unwrap();

        // Agent emits the same block with 2-space indent — classic LLM drift.
        let old_2sp = "  print(\"hi\")\n  return 1";
        let new_2sp = "  print(\"hello\")\n  return 2";

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": old_2sp,
                "new_string": new_2sp,
            }))
            .await;
        assert!(
            result.is_ok(),
            "indent-drift match should succeed with fuzzy_match on, got: {:?}",
            result.err()
        );
        let result = result.unwrap();
        assert_eq!(result["replacements"], 1);

        // The matched byte-range is the file's 4-space-indented block, so
        // splicing the 2-space new_string replaces exactly that range. The
        // file's surrounding lines are untouched.
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "def greet():\n  print(\"hello\")\n  return 2\n");
    }

    /// Acceptance criterion #2: indent-drift match MISS / AMBIGUOUS.
    ///
    /// The normalized needle matches two distinct blocks in the file after
    /// indent-stripping; the uniqueness guard must still fire and return the
    /// "ambiguous match" error.
    #[tokio::test]
    async fn test_fs_edit_fuzzy_indent_drift_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.py");
        // Two distinct blocks that both normalize to the same stripped form.
        let content = concat!(
            "if cond:\n",
            "    print(\"hi\")\n",
            "    return 1\n",
            "else:\n",
            "        print(\"hi\")\n",
            "        return 1\n",
        );
        std::fs::write(&path, content).unwrap();

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                // 2-space-indented form that matches both 4-space and 8-space blocks
                "old_string": "  print(\"hi\")\n  return 1",
                "new_string": "  print(\"bye\")\n  return 2",
            }))
            .await;

        assert!(result.is_err(), "ambiguous fuzzy match must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("appears 2 times"),
            "expected uniqueness-guard error, got: {err}"
        );
    }

    /// Acceptance criterion #3: trailing-whitespace HIT.
    ///
    /// File's line has trailing whitespace that the agent's `old_string`
    /// lacks (or vice versa). Exact match fails; the line-trimmed stage
    /// succeeds.
    #[tokio::test]
    async fn test_fs_edit_fuzzy_trailing_whitespace_hit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.txt");
        // Trailing whitespace on the *interior* line makes the exact
        // substring match impossible: "key = value\nnext" is NOT a
        // substring of "key = value  \nnext" because the two spaces sit
        // between "value" and "\n".
        let content = "alpha\nkey = value  \nnext = thing\ngamma\n";
        std::fs::write(&path, content).unwrap();

        // Confirm exact match *fails* here (fuzzy off) so we are actually
        // exercising the line-trimmed stage below, not the existing pipeline.
        let off_tool = FsEditTool::new().with_fuzzy_match(false);
        let off_result = off_tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "key = value\nnext = thing",
                "new_string": "key = new\nnext = thang",
            }))
            .await;
        assert!(
            off_result.is_err(),
            "sanity: exact substring match must fail when file has \
             trailing spaces on the interior line (fuzzy off); got: {:?}",
            off_result.ok()
        );

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "key = value\nnext = thing",
                "new_string": "key = new\nnext = thang",
            }))
            .await;
        assert!(
            result.is_ok(),
            "trailing-whitespace line-trimmed match should succeed with fuzzy_match on, got: {:?}",
            result.err()
        );
        let result = result.unwrap();
        assert_eq!(result["replacements"], 1);

        // The matched slice spans the file's original two lines (including
        // the `  ` trailing on the first line), which `content.replacen`
        // splices out wholesale. Our non-Markdown trailing-whitespace
        // strip leaves the replacement without any trailing whitespace.
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "alpha\nkey = new\nnext = thang\ngamma\n");
    }

    /// Acceptance criterion #4: fuzzy OFF — exact match still works AND
    /// a fuzzy-needed match still FAILS with the original error (no silent
    /// cascade when fuzzy is disabled).
    #[tokio::test]
    async fn test_fs_edit_fuzzy_off_exact_works_and_fuzzy_silently_fails() {
        let dir = tempfile::tempdir().unwrap();

        // (a) Exact match still works with fuzzy off.
        let exact_path = dir.path().join("exact.txt");
        std::fs::write(&exact_path, "hello world").unwrap();
        let tool_off = FsEditTool::new().with_fuzzy_match(false);
        let result = tool_off
            .execute(serde_json::json!({
                "path": exact_path.to_str().unwrap(),
                "old_string": "hello world",
                "new_string": "goodbye world",
            }))
            .await
            .expect("exact match must still work with fuzzy off");
        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&exact_path).unwrap(),
            "goodbye world"
        );

        // (b) A fuzzy-needed match (indent drift) must FAIL when fuzzy is
        // off — no silent cascade.
        let fuzzy_path = dir.path().join("main.py");
        std::fs::write(
            &fuzzy_path,
            "def greet():\n    print(\"hi\")\n    return 1\n",
        )
        .unwrap();
        let result = tool_off
            .execute(serde_json::json!({
                "path": fuzzy_path.to_str().unwrap(),
                // 2-space indent against 4-space file — only a fuzzy match would work.
                "old_string": "  print(\"hi\")\n  return 1",
                "new_string": "  print(\"hello\")\n  return 2",
            }))
            .await;
        assert!(
            result.is_err(),
            "fuzzy-needed match MUST NOT silently succeed when fuzzy_match=false"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "expected original 'not found' error, got: {err}"
        );
    }

    /// Tim review follow-up (#760): `replace_all=true` combined with a fuzzy
    /// cascade hit that reports `count > 1` must fail explicitly rather than
    /// silently over-report replacements.
    ///
    /// The line-trimmed stage returns a count of normalized-equivalent
    /// windows but only the first window's raw byte slice; a downstream
    /// `content.replace(&actual_old, ...)` only matches byte-for-byte, so the
    /// second window (with different trailing whitespace) would *not* be
    /// replaced even though the tool previously claimed `replacements: 2`.
    /// The simple contract: fuzzy stages are unique-or-fail, regardless of
    /// `replace_all`.
    #[tokio::test]
    async fn test_fs_edit_fuzzy_replace_all_multiple_windows_fails_safely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.txt");
        // Two multi-line windows that match the needle `key = val\nother`
        // after `trim_end` but have *different* interior trailing whitespace
        // on the "key = val" line: window 1 has two trailing spaces, window
        // 2 has a trailing tab. This forces the exact match to fail (the
        // needle is not a literal substring anywhere because of the
        // in-the-middle trailing whitespace) and drives the line-trimmed
        // stage to return count=2. The subsequent `content.replace` would
        // only literally match window 1; honoring `replace_all=true` would
        // over-report replacements and leave window 2 untouched.
        let original = "key = val  \nother\nfiller\nkey = val\t\nother\n";
        std::fs::write(&path, original).unwrap();

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "key = val\nother",
                "new_string": "key = new\nother",
                "replace_all": true,
            }))
            .await;

        assert!(
            result.is_err(),
            "fuzzy match with replace_all and count>1 must fail explicitly, got: {:?}",
            result.ok()
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("fuzzy match found 2 candidates"),
            "expected explicit fuzzy+replace_all rejection, got: {err}"
        );

        // File must be unchanged — no silent partial edit.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "file must be unchanged on rejection");
    }

    /// Tim review follow-up (#760): two distinct lines that are identical
    /// after `trim_end` must be rejected by the uniqueness guard. The
    /// helper-level `test_line_trimmed_match_ambiguous_returns_count` asserts
    /// the internal count but this test exercises the full tool surface
    /// (error message shape + file left untouched).
    #[tokio::test]
    async fn test_fs_edit_fuzzy_trailing_whitespace_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ambig.txt");
        // Two windows that both match "alpha\nbeta" after trimming trailing
        // whitespace. Exact match fails (interior trailing whitespace);
        // line-trimmed stage reports count=2.
        let original = "alpha  \nbeta\t\ngap\nalpha\t\nbeta  \n";
        std::fs::write(&path, original).unwrap();

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "alpha\nbeta",
                "new_string": "A\nB",
                // replace_all defaults to false — uniqueness guard must fire.
            }))
            .await;

        assert!(
            result.is_err(),
            "ambiguous trailing-whitespace fuzzy match must be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("appears 2 times"),
            "expected uniqueness-guard error, got: {err}"
        );

        // File must be unchanged.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "file must be unchanged on rejection");
    }

    /// Tim review follow-up (#760): an all-whitespace `old_string` (e.g. a
    /// bare `"   "`) against a file with multiple blank / whitespace-only
    /// lines must surface the ambiguity error cleanly — not panic, not
    /// silently edit. This test locks the contract rather than fixing a
    /// bug: the existing uniqueness guard already handles this case, but
    /// asserting it prevents future regressions.
    #[tokio::test]
    async fn test_fs_edit_fuzzy_all_whitespace_old_string_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.txt");
        // Multiple lines with three trailing spaces that also appear as
        // standalone whitespace-only content. The exact string "   " is
        // a three-space substring that appears many times in this file.
        let original = "a   \nb   \nc   \n";
        std::fs::write(&path, original).unwrap();

        let tool = FsEditTool::new().with_fuzzy_match(true);
        let result = tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "   ",
                "new_string": "X",
            }))
            .await;

        assert!(
            result.is_err(),
            "all-whitespace needle with many matches must be rejected by uniqueness guard"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("appears") && err.contains("times"),
            "expected uniqueness-guard 'appears N times' error, got: {err}"
        );

        // File must be unchanged — guard-only, no silent edit.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "file must be unchanged on rejection");
    }

    // ── Additional helper-level unit tests for the cascade ──────────────

    #[test]
    fn test_line_trimmed_match_basic_hit() {
        let content = "alpha  \nbeta\t\ngamma\n";
        // Needle without the trailing whitespace.
        let (matched, count) = try_line_trimmed_match(content, "beta").unwrap();
        assert_eq!(matched, "beta\t");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_line_trimmed_match_multiline() {
        let content = "line1\nalpha  \nbeta\t\nline4\n";
        let (matched, count) = try_line_trimmed_match(content, "alpha\nbeta").unwrap();
        assert_eq!(matched, "alpha  \nbeta\t");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_line_trimmed_match_ambiguous_returns_count() {
        // Two independent windows both match after trimming.
        let content = "key\t\nval  \nother\nkey\nval\n";
        let result = try_line_trimmed_match(content, "key\nval");
        let (_, count) = result.unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_line_trimmed_match_no_op_skips() {
        // Nothing to trim on either side → would be a duplicate of exact
        // match; helper should return None to leave the existing exact /
        // CRLF pipeline in charge.
        let content = "alpha\nbeta\ngamma\n";
        assert!(try_line_trimmed_match(content, "beta").is_none());
    }

    #[test]
    fn test_indentation_flexible_match_needle_less_indented() {
        let content = "    foo\n    bar\n    baz\n";
        let (matched, count) = try_indentation_flexible_match(content, "  foo\n  bar").unwrap();
        assert_eq!(matched, "    foo\n    bar");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_indentation_flexible_match_needle_no_indent_content_indented() {
        let content = "        foo\n        bar\n";
        let (matched, count) = try_indentation_flexible_match(content, "foo\nbar").unwrap();
        assert_eq!(matched, "        foo\n        bar");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_indentation_flexible_match_preserves_relative_indent() {
        // The needle's inner indent should be preserved after stripping
        // the common leading indent.  Here the needle's two lines have
        // indent "  " and "    " respectively — common prefix is "  ",
        // stripped form is "foo\n  bar".  The content's matching block
        // has "    foo\n      bar" → common "    ", stripped
        // "foo\n  bar" — equal.  Unique match.
        let content = "    foo\n      bar\n";
        let (matched, count) = try_indentation_flexible_match(content, "  foo\n    bar").unwrap();
        assert_eq!(matched, "    foo\n      bar");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_common_leading_indent_basic() {
        assert_eq!(common_leading_indent(&["    a", "    b"]), "    ");
        assert_eq!(common_leading_indent(&["  a", "    b"]), "  ");
        assert_eq!(common_leading_indent(&["a", "  b"]), "");
    }

    #[test]
    fn test_common_leading_indent_ignores_blank_lines() {
        // Blank lines should not drag the indent down to "".
        assert_eq!(common_leading_indent(&["    a", "", "    b"]), "    ");
        assert_eq!(common_leading_indent(&["", ""]), "");
    }

    #[test]
    fn test_split_lines_with_offsets_lf() {
        let lines = split_lines_with_offsets("alpha\nbeta\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], (0, "alpha", 1));
        assert_eq!(lines[1], (6, "beta", 1));
    }

    #[test]
    fn test_split_lines_with_offsets_crlf() {
        let lines = split_lines_with_offsets("alpha\r\nbeta\r\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], (0, "alpha", 2));
        assert_eq!(lines[1], (7, "beta", 2));
    }

    #[test]
    fn test_split_lines_with_offsets_no_trailing_newline() {
        let lines = split_lines_with_offsets("alpha\nbeta");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], (0, "alpha", 1));
        assert_eq!(lines[1], (6, "beta", 0));
    }
}
