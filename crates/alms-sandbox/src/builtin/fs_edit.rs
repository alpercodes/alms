use crate::error::SandboxResult;
use crate::file_state_cache::{
    FileStateCache, GuardOutcome, check_guard_with_mtime, update_cache_after_write,
};
use crate::{SandboxError, Tool};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    check_sandbox_path_async, is_blocked_device_path, is_denied_path, normalize_unsandboxed_path,
    reject_unc_path,
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
        }
    }

    /// Attach a file state cache for read-before-edit tracking.
    pub fn with_cache(mut self, cache: Arc<FileStateCache>) -> Self {
        self.file_state_cache = Some(cache);
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

        // Deny-list check on raw path.
        if is_denied_path(Path::new(path)) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
                path
            )));
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

        // Deny-list check on resolved path.
        if is_denied_path(&resolved) {
            return Err(SandboxError::SandboxViolation(format!(
                "Access to '{}' is denied",
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

        // Strip trailing whitespace from new_string (except for Markdown).
        let is_md = is_markdown_file(path);
        let new_string_clean = strip_trailing_whitespace(new_string, is_md);
        let new_string = new_string_clean.as_str();

        // Count occurrences of the exact old_string.
        let count = content.matches(old_string).count();

        // If the direct match fails, try normalizing curly/smart quotes as a
        // fallback.  This handles the common case where an LLM emits curly
        // quotes while the file uses straight quotes (or vice versa).
        let (actual_old, count, quote_adapted) = if count == 0 {
            if let Some((original_match, norm_count)) = try_normalized_match(&content, old_string) {
                (original_match.to_string(), norm_count, true)
            } else {
                return Err(SandboxError::InvalidParameters(format!(
                    "old_string not found in '{}'",
                    path
                )));
            }
        } else {
            (old_string.to_string(), count, false)
        };

        if !replace_all && count > 1 {
            return Err(SandboxError::InvalidParameters(format!(
                "old_string appears {} times in '{}'; set replace_all to true or provide a more unique string",
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

        // Apply the same trailing-whitespace stripping as the normal edit path
        // so file creation and file editing have consistent whitespace behavior.
        let is_md = is_markdown_file(display_path);
        let cleaned = strip_trailing_whitespace(new_string, is_md);

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

/// Strip trailing whitespace (spaces and tabs) from each line.
///
/// Markdown files (`.md`, `.markdown`) are exempt because trailing spaces
/// are semantically meaningful as hard line breaks per CommonMark.
fn strip_trailing_whitespace(s: &str, is_markdown: bool) -> String {
    if is_markdown {
        return s.to_string();
    }
    let mut result = s
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    // str::lines() does not yield a trailing empty element for strings ending
    // with '\n', so .join("\n") silently drops the final newline.  Re-append
    // it so code files keep their POSIX-compliant trailing newline.
    if s.ends_with('\n') {
        result.push('\n');
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

    #[tokio::test]
    async fn test_fs_edit_denied_path() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets.json");
        std::fs::write(&secrets, r#"{"key": "value"}"#).unwrap();

        let tool = FsEditTool::new();
        let result = tool
            .execute(serde_json::json!({
                "path": secrets.to_str().unwrap(),
                "old_string": "value",
                "new_string": "hacked"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("denied"));
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

    #[test]
    fn test_fs_edit_not_auto_approved() {
        assert!(!FsEditTool::new().is_auto_approved());
    }

    #[test]
    fn test_fs_edit_description_nonempty() {
        assert!(!FsEditTool::new().description().is_empty());
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
            strip_trailing_whitespace("hello  \nworld\t\n  ok  ", false),
            "hello\nworld\n  ok"
        );
    }

    #[test]
    fn test_strip_trailing_whitespace_markdown_preserved() {
        let input = "hello  \nworld\t\n  ok  ";
        assert_eq!(strip_trailing_whitespace(input, true), input);
    }

    #[test]
    fn test_strip_trailing_whitespace_preserves_trailing_newline() {
        assert_eq!(
            strip_trailing_whitespace("hello  \nworld\n", false),
            "hello\nworld\n"
        );
        assert_eq!(
            strip_trailing_whitespace("hello  \r\nworld\r\n", false),
            "hello\nworld\n"
        );
        assert_eq!(
            strip_trailing_whitespace("hello  \nworld", false),
            "hello\nworld"
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
}
