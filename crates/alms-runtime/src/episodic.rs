//! Episodic summary generation for cross-session memory.
//!
//! At the end of each successful run, the gateway may generate or update a
//! per-session summary.  The summary mode is controlled by
//! [`RunSummaryMode`](alms_core::config::RunSummaryMode) from the context
//! config.
//!
//! Two modes are supported:
//!
//! - **Heuristic**: deterministic, no LLM call.  Produces a source-labelled
//!   one-liner from the first ~120 characters of the run input.  When an
//!   existing summary exists, the new entry is appended and old entries are
//!   trimmed to keep total length under ~500 characters.
//!
//! - **LLM**: makes a lightweight completion call with the run input, the
//!   agent's final response, and the existing summary (if any).  The LLM
//!   produces a concise evolving summary of the session.

use crate::llm_client::LlmClient;
use crate::llm_types::{CompletionRequest, LlmMessage};
use alms_core::config::RunSummaryMode;
use alms_core::{AgentId, RunId, SessionId};
use alms_session::SessionManager;
use tracing::{debug, error, info, instrument, warn};

/// Maximum byte length for the heuristic input snippet.
///
/// This is a byte budget, not a character count.  For ASCII text the two are
/// equivalent; multi-byte UTF-8 sequences may result in fewer characters.
/// `truncate_to_char_boundary` ensures we never split mid-codepoint.
const HEURISTIC_INPUT_BYTES: usize = 120;

/// Maximum total byte length for accumulated heuristic summaries.
/// Oldest entries are trimmed when the total exceeds this.
const HEURISTIC_MAX_TOTAL_BYTES: usize = 500;

/// Maximum byte length for run input/output sent to the LLM summarizer.
///
/// Same byte-budget caveat as [`HEURISTIC_INPUT_BYTES`].
const LLM_CONTEXT_BYTES: usize = 2000;

/// Maximum output tokens for the LLM summarizer call.
const LLM_MAX_OUTPUT_TOKENS: u32 = 150;

/// Source classification derived from a `context_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLabel {
    /// Machine-readable type: "web", "telegram", "dm", "job", etc.
    pub source_type: String,
    /// Human-readable label shown in summaries: "User chat", "DM from bob", etc.
    pub source_label: String,
}

/// Derive a human-readable source label from a `context_id`.
///
/// Returns `None` for session types that should be excluded from summary
/// generation (DM and subagent sessions).
pub fn derive_source_label(context_id: &str) -> Option<SourceLabel> {
    // DM sessions -- excluded
    if context_id.starts_with("dm:") {
        return None;
    }

    // Subagent sessions -- excluded
    if context_id.starts_with("subagent_") {
        return None;
    }

    // Episodic sessions -- excluded (internal)
    if context_id.starts_with("episodic:") {
        return None;
    }

    // Telegram sessions: "telegram_{agent}_{chatid}"
    if context_id.starts_with("telegram_") {
        return Some(SourceLabel {
            source_type: "telegram".into(),
            source_label: "Telegram chat".into(),
        });
    }

    // Job sessions: "job_{jobid}"
    if let Some(job_part) = context_id.strip_prefix("job_") {
        let label = if job_part.len() > 40 {
            let truncated = truncate_to_char_boundary(job_part, 37);
            format!("Scheduled job: {truncated}...")
        } else {
            format!("Scheduled job: {job_part}")
        };
        return Some(SourceLabel {
            source_type: "job".into(),
            source_label: label,
        });
    }

    // Default: web/API chat
    Some(SourceLabel {
        source_type: "web".into(),
        source_label: "User chat".into(),
    })
}

/// Parameters for episodic summary generation.
pub struct SummaryParams {
    pub mode: RunSummaryMode,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub run_input: String,
    pub run_output: String,
    pub context_id: String,
    /// Existing summary for this session (if any), loaded from the DB.
    pub existing_summary: Option<String>,
    /// Model to use for LLM mode.  Falls back to the LLM client's default.
    pub summary_model: Option<String>,
}

/// Generate or update a session summary.
///
/// Returns `Some(summary_text)` when a summary was produced, or `None` when
/// the mode is `Off`, the session type is excluded, or inputs are empty.
///
/// For **heuristic** mode no LLM call is made.  For **LLM** mode a lightweight
/// completion call is issued using the provided `LlmClient`.
///
/// This function is designed to be called in a fire-and-forget `tokio::spawn`
/// after a successful run.  Errors are logged but never propagated.
#[instrument(
    level = "debug",
    skip(llm, params),
    fields(
        agent_id = %params.agent_id.0,
        session_id = %params.session_id.0,
        run_id = %params.run_id.0,
        mode = %params.mode,
    )
)]
pub async fn generate_session_summary(llm: &LlmClient, params: &SummaryParams) -> Option<String> {
    match params.mode {
        RunSummaryMode::Off | RunSummaryMode::Unknown => {
            debug!("Summary generation skipped (mode=off)");
            return None;
        }
        RunSummaryMode::Heuristic => generate_heuristic(params),
        RunSummaryMode::Llm => generate_llm(llm, params).await,
    }
}

/// Parameters for the fire-and-forget summary generation + persistence.
pub struct PersistSummaryRequest {
    pub mode: RunSummaryMode,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub run_input: String,
    pub run_output: String,
    pub context_id: String,
    pub summary_model: Option<String>,
}

/// Fire-and-forget summary generation + persistence.
///
/// Called from the gateway after a successful run.  Loads the existing summary
/// (if any), generates a new one, and upserts it to the database.  All errors
/// are logged and swallowed -- summary failure must never fail the run.
///
/// **Known race condition:** Two concurrent runs for the same session can both
/// load the same base summary, generate independently, and the last writer
/// wins -- one update is silently lost.  This mirrors the existing within-run
/// rolling-summary race and is acceptable for MVP because concurrent runs on
/// the same session are rare.  A future fix could add optimistic locking
/// (check `last_run_id` before upsert, retry on conflict).
#[instrument(
    level = "info",
    skip(session_manager, llm, req),
    fields(
        agent_id = %req.agent_id.0,
        session_id = %req.session_id.0,
        run_id = %req.run_id.0,
        mode = %req.mode,
    )
)]
pub async fn generate_and_persist_summary(
    session_manager: &SessionManager,
    llm: &LlmClient,
    req: PersistSummaryRequest,
) {
    // Check if the session type should be summarised.
    let source = match derive_source_label(&req.context_id) {
        Some(s) => s,
        None => {
            debug!(
                context_id = req.context_id.as_str(),
                "Skipping summary for excluded session type"
            );
            return;
        }
    };

    // Load existing summary from DB (if any).
    let existing_summary = match session_manager.store() {
        Some(store) => match store.load_session_summary(req.agent_id, req.session_id) {
            Ok(Some(s)) => Some(s.summary),
            Ok(None) => None,
            Err(e) => {
                warn!("Failed to load existing session summary: {e}");
                None
            }
        },
        None => None,
    };

    let params = SummaryParams {
        mode: req.mode,
        agent_id: req.agent_id,
        session_id: req.session_id,
        run_id: req.run_id,
        run_input: req.run_input,
        run_output: req.run_output,
        context_id: req.context_id.clone(),
        existing_summary,
        summary_model: req.summary_model,
    };

    let summary_text = match generate_session_summary(llm, &params).await {
        Some(text) => text,
        None => return,
    };

    info!(
        source_type = source.source_type.as_str(),
        source_label = source.source_label.as_str(),
        summary_len = summary_text.len(),
        "Session summary generated"
    );

    // Upsert to database.
    if let Some(store) = session_manager.store() {
        if let Err(e) = store.upsert_session_summary(
            params.agent_id,
            params.session_id,
            &summary_text,
            Some(params.run_id),
        ) {
            error!("Failed to persist session summary: {e}");
        }
    } else {
        warn!("No SQLite store -- session summary not persisted");
    }
}

// ---------------------------------------------------------------------------
// Heuristic mode
// ---------------------------------------------------------------------------

/// Produce a deterministic summary from the source label and run input.
///
/// Format: `"{source_label}: \"{first ~120 chars of input}...\""` (or without
/// the ellipsis when the input fits entirely).
///
/// When an existing summary exists, the new entry is appended on a new line.
/// The total is trimmed line-by-line (oldest first) to stay under
/// [`HEURISTIC_MAX_TOTAL_BYTES`].
fn generate_heuristic(params: &SummaryParams) -> Option<String> {
    if params.run_input.is_empty() {
        return None;
    }

    let source = derive_source_label(&params.context_id)?;

    // Build the new entry line.
    let snippet = truncate_to_char_boundary(&params.run_input, HEURISTIC_INPUT_BYTES);
    let ellipsis = if params.run_input.len() > snippet.len() {
        "..."
    } else {
        ""
    };
    let entry = format!("{}: \"{snippet}{ellipsis}\"", source.source_label);

    // Append to existing summary (if any).
    let combined = match &params.existing_summary {
        Some(existing) if !existing.is_empty() => {
            format!("{existing}\n{entry}")
        }
        _ => entry,
    };

    // Trim oldest lines to keep total under the cap.
    Some(trim_oldest_lines(&combined, HEURISTIC_MAX_TOTAL_BYTES))
}

/// Trim a multi-line string by removing the oldest (first) lines until total
/// length is within `max_chars`.  Always keeps at least the last line.
fn trim_oldest_lines(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    // Walk from the end, accumulating lines until we would exceed the budget.
    let mut kept: Vec<&str> = Vec::new();
    let mut total = 0usize;
    for line in lines.iter().rev() {
        let line_cost = line.len() + 1; // +1 for the newline separator
        if total + line_cost > max_chars && !kept.is_empty() {
            break;
        }
        total += line_cost;
        kept.push(line);
    }
    kept.reverse();
    kept.join("\n")
}

/// Truncate a string to at most `max` characters, respecting UTF-8 char
/// boundaries.
fn truncate_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find the largest char boundary <= max.
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

// ---------------------------------------------------------------------------
// LLM mode
// ---------------------------------------------------------------------------

/// Generate a summary via a lightweight LLM call.
async fn generate_llm(llm: &LlmClient, params: &SummaryParams) -> Option<String> {
    if params.run_input.is_empty() && params.run_output.is_empty() {
        return None;
    }

    let system_prompt = include_str!("../prompts/session_summarizer.md").trim();

    let mut user_content = String::new();

    // Include existing summary for the LLM to extend.
    if let Some(ref existing) = params.existing_summary
        && !existing.is_empty()
    {
        user_content.push_str("Existing session summary:\n");
        user_content.push_str(existing);
        user_content.push_str("\n\n---\n\nNew interaction to incorporate:\n\n");
    }

    // Truncated run input.
    let input_snippet = truncate_to_char_boundary(&params.run_input, LLM_CONTEXT_BYTES);
    user_content.push_str("User input:\n");
    user_content.push_str(input_snippet);
    if params.run_input.len() > input_snippet.len() {
        user_content.push_str("\n[...truncated]");
    }

    // Truncated run output.
    if !params.run_output.is_empty() {
        let output_snippet = truncate_to_char_boundary(&params.run_output, LLM_CONTEXT_BYTES);
        user_content.push_str("\n\nAgent response:\n");
        user_content.push_str(output_snippet);
        if params.run_output.len() > output_snippet.len() {
            user_content.push_str("\n[...truncated]");
        }
    }

    let model = params
        .summary_model
        .as_deref()
        .unwrap_or_else(|| llm.default_model());

    let messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(user_content),
    ];

    let request = CompletionRequest::new(model)
        .with_messages(messages)
        .with_temperature(0.3)
        .with_max_tokens(LLM_MAX_OUTPUT_TOKENS);

    match llm.complete(request).await {
        Ok(response) => {
            let text = response
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content);
            match text {
                Some(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
                _ => {
                    warn!("LLM summarizer returned empty response");
                    None
                }
            }
        }
        Err(e) => {
            error!("LLM summarizer call failed: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::config::RunSummaryMode;

    // -- derive_source_label ------------------------------------------------

    #[test]
    fn test_source_label_web_chat() {
        let label = derive_source_label("web-chat-2026-03-25").unwrap();
        assert_eq!(label.source_type, "web");
        assert_eq!(label.source_label, "User chat");
    }

    #[test]
    fn test_source_label_telegram() {
        let label = derive_source_label("telegram_mybot_123456").unwrap();
        assert_eq!(label.source_type, "telegram");
        assert_eq!(label.source_label, "Telegram chat");
    }

    #[test]
    fn test_source_label_job() {
        let label = derive_source_label("job_abc123").unwrap();
        assert_eq!(label.source_type, "job");
        assert_eq!(label.source_label, "Scheduled job: abc123");
    }

    #[test]
    fn test_source_label_job_long_id() {
        let long_id = "a".repeat(50);
        let label = derive_source_label(&format!("job_{long_id}")).unwrap();
        assert_eq!(label.source_type, "job");
        // Should be truncated to 37 chars + "..."
        assert!(label.source_label.ends_with("..."));
        assert!(label.source_label.len() <= 55);
    }

    #[test]
    fn test_source_label_dm_excluded() {
        assert!(derive_source_label("dm:alice:bob").is_none());
    }

    #[test]
    fn test_source_label_subagent_excluded() {
        assert!(derive_source_label("subagent_task_123").is_none());
    }

    #[test]
    fn test_source_label_episodic_excluded() {
        assert!(derive_source_label("episodic:myagent").is_none());
    }

    #[test]
    fn test_source_label_unknown_defaults_to_web() {
        let label = derive_source_label("some-random-context").unwrap();
        assert_eq!(label.source_type, "web");
        assert_eq!(label.source_label, "User chat");
    }

    // -- heuristic mode -----------------------------------------------------

    fn heuristic_params(input: &str, context_id: &str, existing: Option<&str>) -> SummaryParams {
        SummaryParams {
            mode: RunSummaryMode::Heuristic,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: input.to_string(),
            run_output: "Some output".to_string(),
            context_id: context_id.to_string(),
            existing_summary: existing.map(|s| s.to_string()),
            summary_model: None,
        }
    }

    #[test]
    fn test_heuristic_basic_format() {
        let params = heuristic_params("How do I configure CORS?", "web-chat-123", None);
        let result = generate_heuristic(&params).unwrap();
        assert_eq!(result, "User chat: \"How do I configure CORS?\"");
    }

    #[test]
    fn test_heuristic_truncates_long_input() {
        let long_input = "a".repeat(200);
        let params = heuristic_params(&long_input, "web-chat-123", None);
        let result = generate_heuristic(&params).unwrap();
        assert!(result.ends_with("...\""));
        // The snippet inside quotes should be ~120 chars
        let inner = &result["User chat: \"".len()..result.len() - 4]; // strip trailing ...\"
        assert_eq!(inner.len(), 120);
    }

    #[test]
    fn test_heuristic_empty_input_returns_none() {
        let params = heuristic_params("", "web-chat-123", None);
        assert!(generate_heuristic(&params).is_none());
    }

    #[test]
    fn test_heuristic_appends_to_existing() {
        let existing = "User chat: \"First question about CORS\"";
        let params = heuristic_params("Second question about auth", "web-chat-123", Some(existing));
        let result = generate_heuristic(&params).unwrap();
        assert!(result.contains("First question about CORS"));
        assert!(result.contains("Second question about auth"));
        // Two lines
        assert_eq!(result.lines().count(), 2);
    }

    #[test]
    fn test_heuristic_trims_old_entries_when_over_budget() {
        // Create an existing summary that is already near the limit.
        let mut existing_lines: Vec<String> = Vec::new();
        for i in 0..10 {
            existing_lines.push(format!(
                "User chat: \"Question number {i} about something fairly long to fill space\""
            ));
        }
        let existing = existing_lines.join("\n");
        let params = heuristic_params(
            "New question after many old ones",
            "web-chat-123",
            Some(&existing),
        );
        let result = generate_heuristic(&params).unwrap();

        // trim_oldest_lines keeps lines from the end until the next line would
        // exceed the budget, with the guarantee that at least the last line is
        // always kept.  Therefore either:
        //   (a) the result fits within the budget, or
        //   (b) the result is exactly one line (the newest) which exceeds the
        //       budget by itself.
        let line_count = result.lines().count();
        if line_count > 1 {
            assert!(
                result.len() <= HEURISTIC_MAX_TOTAL_BYTES,
                "Multi-line result ({} bytes) should fit within budget ({})",
                result.len(),
                HEURISTIC_MAX_TOTAL_BYTES,
            );
        }
        // Must contain the newest entry
        assert!(result.contains("New question after many old ones"));
        // Must have trimmed at least some old entries (we started with 10).
        assert!(
            line_count < 11,
            "Expected some old entries trimmed, got {line_count} lines"
        );
    }

    #[test]
    fn test_heuristic_dm_session_skipped() {
        let params = heuristic_params("Hello", "dm:alice:bob", None);
        assert!(generate_heuristic(&params).is_none());
    }

    #[test]
    fn test_heuristic_subagent_session_skipped() {
        let params = heuristic_params("Hello", "subagent_task_42", None);
        assert!(generate_heuristic(&params).is_none());
    }

    #[test]
    fn test_heuristic_telegram_label() {
        let params = heuristic_params("Hello from telegram", "telegram_bot_12345", None);
        let result = generate_heuristic(&params).unwrap();
        assert!(result.starts_with("Telegram chat: "));
    }

    #[test]
    fn test_heuristic_job_label() {
        let params = heuristic_params("Generate daily report", "job_daily-report", None);
        let result = generate_heuristic(&params).unwrap();
        assert!(result.starts_with("Scheduled job: daily-report: "));
    }

    // -- trim_oldest_lines --------------------------------------------------

    #[test]
    fn test_trim_oldest_lines_no_trim_needed() {
        let text = "line1\nline2";
        assert_eq!(trim_oldest_lines(text, 100), text);
    }

    #[test]
    fn test_trim_oldest_lines_removes_oldest() {
        let text = "oldest line that is quite long\nmiddle line\nnewest line";
        let result = trim_oldest_lines(text, 30);
        assert!(!result.contains("oldest"));
        assert!(result.contains("newest line"));
    }

    #[test]
    fn test_trim_oldest_lines_keeps_at_least_last() {
        let text = "line1\nline2\nvery long last line that exceeds budget by itself";
        let result = trim_oldest_lines(text, 10);
        // Should keep the last line even though it exceeds budget
        assert!(result.contains("very long last line"));
        assert!(!result.contains("line1"));
    }

    // -- truncate_to_char_boundary ------------------------------------------

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate_to_char_boundary("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_no_op() {
        assert_eq!(truncate_to_char_boundary("hi", 10), "hi");
    }

    #[test]
    fn test_truncate_multibyte() {
        // Each emoji is 4 bytes. Truncating at 5 should give us just the first emoji.
        let s = "\u{1F600}\u{1F601}\u{1F602}"; // 3 emoji, 12 bytes
        let result = truncate_to_char_boundary(s, 5);
        assert_eq!(result, "\u{1F600}"); // 4 bytes, next boundary is at 8
    }

    // -- generate_session_summary (integration with mode dispatch) ----------

    #[tokio::test]
    async fn test_summary_off_mode_returns_none() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        let params = SummaryParams {
            mode: RunSummaryMode::Off,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "hello".into(),
            run_output: "world".into(),
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
        };
        assert!(generate_session_summary(&llm, &params).await.is_none());
    }

    #[tokio::test]
    async fn test_summary_heuristic_mode() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        let params = SummaryParams {
            mode: RunSummaryMode::Heuristic,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "How do I set up CORS?".into(),
            run_output: "You need to configure headers...".into(),
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
        };
        let result = generate_session_summary(&llm, &params).await.unwrap();
        assert!(result.contains("How do I set up CORS?"));
        assert!(result.starts_with("User chat:"));
    }

    #[tokio::test]
    async fn test_summary_llm_mode_with_mock() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "Help me debug this error".into(),
            run_output: "The issue is in your config file".into(),
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
        };
        // Mock LLM returns a canned response -- we just verify it doesn't panic
        // and returns Some.
        let result = generate_session_summary(&llm, &params).await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_summary_llm_mode_with_existing_summary() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "Follow-up question about CORS".into(),
            run_output: "Added the header, should work now".into(),
            context_id: "web-chat-1".into(),
            existing_summary: Some("Debugged CORS issue in gateway.rs.".into()),
            summary_model: None,
        };
        // Should not panic, should return Some
        let result = generate_session_summary(&llm, &params).await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_summary_excluded_session_returns_none() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        for ctx in &["dm:alice:bob", "subagent_task_1", "episodic:myagent"] {
            let params = SummaryParams {
                mode: RunSummaryMode::Heuristic,
                agent_id: AgentId::new(),
                session_id: SessionId::new(),
                run_id: RunId::new(),
                run_input: "hello".into(),
                run_output: "world".into(),
                context_id: ctx.to_string(),
                existing_summary: None,
                summary_model: None,
            };
            assert!(
                generate_session_summary(&llm, &params).await.is_none(),
                "Expected None for context_id={ctx}"
            );
        }
    }
}
