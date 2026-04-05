//! Episodic summary generation for cross-session memory.
//!
//! At the end of each successful run, the gateway may generate or update a
//! per-session summary.  The summary mode is controlled by
//! [`RunSummaryMode`](alms_core::config::RunSummaryMode) from the context
//! config.
//!
//! Two modes are supported:
//!
//! - **Heuristic**: deterministic, no LLM call.  Produces a one-liner from
//!   the first ~120 bytes of the run input and ~80 bytes of the run output
//!   (when available).  When an existing summary exists, the new entry is
//!   appended and old entries are trimmed to keep total length under ~500
//!   characters.
//!
//! - **LLM**: makes a lightweight completion call with the run input, the
//!   agent's final response, and the existing summary (if any).  The LLM
//!   produces a concise evolving summary of the session.

use crate::llm_client::LlmClient;
use crate::llm_types::{CompletionRequest, LlmMessage};
use alms_core::config::RunSummaryMode;
use alms_core::source_label::{derive_source_label, truncate_to_char_boundary};
use alms_core::{AgentId, RunId, SessionId};
use alms_session::SessionManager;
use tracing::{debug, error, info, instrument, warn};

/// Maximum byte length for the heuristic input snippet.
///
/// This is a byte budget, not a character count.  For ASCII text the two are
/// equivalent; multi-byte UTF-8 sequences may result in fewer characters.
/// `truncate_to_char_boundary` ensures we never split mid-codepoint.
const HEURISTIC_INPUT_BYTES: usize = 120;

/// Maximum byte length for the heuristic *output* snippet appended after the
/// input.  Kept shorter than the input budget so a single entry line stays
/// compact.  Only included when `run_output` is non-empty.
const HEURISTIC_OUTPUT_BYTES: usize = 80;

/// Maximum total byte length for accumulated heuristic summaries.
/// Oldest entries are trimmed when the total exceeds this.
const HEURISTIC_MAX_TOTAL_BYTES: usize = 500;

/// Maximum byte length for run input/output sent to the LLM summarizer.
///
/// Same byte-budget caveat as [`HEURISTIC_INPUT_BYTES`].
const LLM_CONTEXT_BYTES: usize = 2000;

/// Maximum output tokens for the LLM summarizer call.
///
/// Set high enough to accommodate reasoning models that consume some of the
/// budget on internal thinking before producing visible output.  The actual
/// summary text is typically 50-100 tokens, but models like minimax-m2.5 or
/// deepseek-r1 may spend 100-200 tokens on reasoning first.
const LLM_MAX_OUTPUT_TOKENS: u32 = 300;

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
    /// Agent name — used to derive the peer in DM sessions.
    pub agent_name: String,
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
    /// Agent name — used to derive the peer in DM sessions.
    pub agent_name: String,
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
    let source = match derive_source_label(&req.context_id, &req.agent_name) {
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
        agent_name: req.agent_name,
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

    // Upsert to database (with source label for injection formatting).
    if let Some(store) = session_manager.store() {
        if let Err(e) = store.upsert_session_summary(
            params.agent_id,
            params.session_id,
            &summary_text,
            Some(params.run_id),
            Some(&source.source_label),
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
/// Format: `"<input snippet>" -> "<output snippet>"` when `run_output` is
/// available, otherwise `"<input snippet>"` alone.  Snippets are truncated
/// to [`HEURISTIC_INPUT_BYTES`] and [`HEURISTIC_OUTPUT_BYTES`] respectively.
///
/// When an existing summary exists, the new entry is appended on a new line.
/// The total is trimmed line-by-line (oldest first) to stay under
/// [`HEURISTIC_MAX_TOTAL_BYTES`].
fn generate_heuristic(params: &SummaryParams) -> Option<String> {
    if params.run_input.is_empty() {
        return None;
    }

    // Still call derive_source_label to filter out subagent sessions (returns
    // None for excluded session types, triggering early return via `?`).
    let _source = derive_source_label(&params.context_id, &params.agent_name)?;

    // Build the new entry line.
    let in_snippet = truncate_to_char_boundary(&params.run_input, HEURISTIC_INPUT_BYTES);
    let in_ellipsis = if params.run_input.len() > in_snippet.len() {
        "..."
    } else {
        ""
    };

    // Include the agent's response when available (#434, Bug 2).
    let entry = if !params.run_output.is_empty() {
        let out_snippet = truncate_to_char_boundary(&params.run_output, HEURISTIC_OUTPUT_BYTES);
        let out_ellipsis = if params.run_output.len() > out_snippet.len() {
            "..."
        } else {
            ""
        };
        format!("\"{in_snippet}{in_ellipsis}\" -> \"{out_snippet}{out_ellipsis}\"")
    } else {
        format!("\"{in_snippet}{in_ellipsis}\"")
    };

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

// truncate_to_char_boundary has been moved to alms_core::source_label and is
// imported at the top of this file.

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
            let choice = response.choices.into_iter().next();
            let text = choice.as_ref().and_then(|c| {
                // Primary: use `content`.
                // Fallback: use `reasoning_content` -- reasoning models (e.g.
                // minimax-m2.5, deepseek-r1) may consume all max_tokens on
                // thinking before producing output, leaving `content` as null
                // while `reasoning_content` holds useful text.
                c.message
                    .content
                    .as_deref()
                    .or(c.message.reasoning_content.as_deref())
            });
            match text {
                Some(t) if !t.trim().is_empty() => {
                    // If we fell back to reasoning_content, note it in logs.
                    if choice.as_ref().is_some_and(|c| c.message.content.is_none()) {
                        info!("Used reasoning_content as summary (model returned null content)");
                    }
                    Some(t.trim().to_string())
                }
                _ => {
                    warn!(
                        has_content = choice.as_ref().is_some_and(|c| c.message.content.is_some()),
                        has_reasoning = choice
                            .as_ref()
                            .is_some_and(|c| c.message.reasoning_content.is_some()),
                        has_tool_calls = choice
                            .as_ref()
                            .is_some_and(|c| c.message.tool_calls.is_some()),
                        finish_reason = choice
                            .as_ref()
                            .and_then(|c| c.finish_reason.as_deref())
                            .unwrap_or("unknown"),
                        "LLM summarizer returned empty response"
                    );
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
// Context injection formatting
// ---------------------------------------------------------------------------

use crate::context::estimate_tokens;
use alms_session::SessionSummary;

/// Format a list of [`SessionSummary`] records into the episodic injection
/// string, respecting a token budget.
///
/// Summaries are expected to arrive in `updated_at DESC` order (most recent
/// first).  This function preserves that order so the most recently active
/// session appears closest to the current conversation in the context window,
/// giving it the strongest LLM recency bias.
///
/// When the formatted summaries would exceed `budget_tokens`, the oldest
/// summaries (at the end of the list) are dropped first.  If even the header
/// alone exceeds the budget, `None` is returned.
///
/// Returns `None` when `summaries` is empty or all summaries are empty strings.
///
/// **Defense-in-depth filtering:** Entries whose `source_label` is `None` are
/// silently skipped.  Under normal operation subagent sessions never get a
/// summary row, but this guard prevents manually-inserted or corrupt rows from
/// leaking into the episodic context.
///
/// **Budget contract:** The caller is responsible for ensuring `budget_tokens`
/// is already clamped to the configured percentage cap (15% of
/// `max_input_tokens` by default, enforced in
/// [`ContextConfig::normalize_episodic`]).  This function does not re-enforce
/// the cap — it trusts the budget it receives.
pub fn format_episodic_for_injection(
    summaries: &[SessionSummary],
    budget_tokens: usize,
) -> Option<String> {
    if summaries.is_empty() || budget_tokens == 0 {
        return None;
    }

    let header = "[Your conversation history across sessions — most recent first]\n";
    let header_tokens = estimate_tokens(header);
    if header_tokens >= budget_tokens {
        return None;
    }

    let mut remaining = budget_tokens - header_tokens;
    let mut entries: Vec<String> = Vec::new();

    for summary in summaries {
        if summary.summary.is_empty() {
            continue;
        }

        // S2: Defense-in-depth -- skip entries without a source_label.
        // Under normal operation subagent sessions never get a summary row,
        // but if one is manually inserted (or exists from before source_label
        // was added), filtering here prevents it from appearing in the context.
        let label = match &summary.source_label {
            Some(l) if !l.is_empty() => l.as_str(),
            _ => continue,
        };

        // Format: **<source_label> (last active: <date>)**\n<summary>
        let updated = summary.updated_at.0.format("%Y-%m-%d %H:%M UTC");
        let entry = format!(
            "\n**{label} (last active: {updated})**\n{}",
            summary.summary
        );

        let entry_tokens = estimate_tokens(&entry);
        if entry_tokens > remaining {
            // Budget exhausted -- stop adding entries.
            break;
        }
        remaining -= entry_tokens;
        entries.push(entry);
    }

    if entries.is_empty() {
        return None;
    }

    // Pre-allocate: budget_tokens * 3 approximates the char capacity since
    // estimate_tokens uses chars/3.
    let mut result = String::with_capacity(budget_tokens * 3);
    result.push_str(header);
    for entry in &entries {
        result.push_str(entry);
    }

    Some(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alms_core::config::RunSummaryMode;

    // derive_source_label tests have moved to alms_core::source_label::tests.

    // -- heuristic mode -----------------------------------------------------

    fn heuristic_params(input: &str, context_id: &str, existing: Option<&str>) -> SummaryParams {
        heuristic_params_with_name(input, context_id, existing, "myagent")
    }

    fn heuristic_params_with_name(
        input: &str,
        context_id: &str,
        existing: Option<&str>,
        agent_name: &str,
    ) -> SummaryParams {
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
            agent_name: agent_name.to_string(),
        }
    }

    #[test]
    fn test_heuristic_basic_format() {
        let params = heuristic_params("How do I configure CORS?", "web-chat-123", None);
        let result = generate_heuristic(&params).unwrap();
        // run_output is "Some output" so the entry includes `-> "Some output"`
        assert_eq!(result, "\"How do I configure CORS?\" -> \"Some output\"");
    }

    #[test]
    fn test_heuristic_no_output() {
        // When run_output is empty, only the input is shown.
        let mut params = heuristic_params("How do I configure CORS?", "web-chat-123", None);
        params.run_output = String::new();
        let result = generate_heuristic(&params).unwrap();
        assert_eq!(result, "\"How do I configure CORS?\"");
    }

    #[test]
    fn test_heuristic_truncates_long_input() {
        let long_input = "a".repeat(200);
        let params = heuristic_params(&long_input, "web-chat-123", None);
        let result = generate_heuristic(&params).unwrap();
        // The result now includes `-> "Some output"` after the input snippet.
        assert!(result.contains("...\""), "input should be truncated");
        assert!(
            result.contains("-> \"Some output\""),
            "output should be present"
        );
    }

    #[test]
    fn test_heuristic_empty_input_returns_none() {
        let params = heuristic_params("", "web-chat-123", None);
        assert!(generate_heuristic(&params).is_none());
    }

    #[test]
    fn test_heuristic_appends_to_existing() {
        let existing = "\"First question about CORS\"";
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
                "\"Question number {i} about something fairly long to fill space\""
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
    fn test_heuristic_dm_session_included() {
        let params = heuristic_params_with_name("Hello", "dm:alice:bob", None, "alice");
        let result = generate_heuristic(&params).unwrap();
        assert!(
            result.starts_with('"'),
            "DM session summary should not include source_label prefix, got: {result}"
        );
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
        assert!(
            result.starts_with('"'),
            "Telegram summary should not include source_label prefix, got: {result}"
        );
    }

    #[test]
    fn test_heuristic_job_label() {
        let params = heuristic_params("Generate daily report", "job_daily-report", None);
        let result = generate_heuristic(&params).unwrap();
        assert!(
            result.starts_with('"'),
            "Job summary should not include source_label prefix, got: {result}"
        );
    }

    #[test]
    fn test_heuristic_dm_includes_run_output() {
        // Bug 2 regression: DM summaries must include the agent's reply.
        let mut params =
            heuristic_params_with_name("Hey, can you help?", "dm:alice:bob", None, "alice");
        params.run_output = "Sure, what do you need?".to_string();
        let result = generate_heuristic(&params).unwrap();
        assert!(
            result.contains("Hey, can you help?"),
            "Should contain input: {result}"
        );
        assert!(
            result.contains("Sure, what do you need?"),
            "Should contain output: {result}"
        );
        assert!(
            result.contains("->"),
            "Should contain arrow separator: {result}"
        );
    }

    #[test]
    fn test_heuristic_truncates_long_output() {
        let long_output = "b".repeat(200);
        let mut params = heuristic_params("short input", "web-chat-123", None);
        params.run_output = long_output;
        let result = generate_heuristic(&params).unwrap();
        assert!(
            result.contains("-> \""),
            "Should contain output section: {result}"
        );
        assert!(
            result.ends_with("...\""),
            "Long output should be truncated: {result}"
        );
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

    // truncate_to_char_boundary tests have moved to alms_core::source_label::tests.

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
            agent_name: "myagent".into(),
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
            agent_name: "myagent".into(),
        };
        let result = generate_session_summary(&llm, &params).await.unwrap();
        assert!(result.contains("How do I set up CORS?"));
        assert!(
            result.starts_with('"'),
            "Heuristic summary should not include source_label prefix, got: {result}"
        );
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
            agent_name: "myagent".into(),
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
            agent_name: "myagent".into(),
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
        // DM sessions are now included (not excluded), only subagent and episodic
        // sessions should return None.
        for ctx in &["subagent_task_1", "episodic:myagent"] {
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
                agent_name: "myagent".into(),
            };
            assert!(
                generate_session_summary(&llm, &params).await.is_none(),
                "Expected None for context_id={ctx}"
            );
        }
    }

    #[tokio::test]
    async fn test_summary_dm_session_produces_summary() {
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
            run_input: "hello from DM".into(),
            run_output: "hi back".into(),
            context_id: "dm:alice:bob".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "alice".into(),
        };
        let result = generate_session_summary(&llm, &params).await;
        assert!(result.is_some(), "DM sessions should now produce summaries");
        let text = result.unwrap();
        assert!(
            text.starts_with('"'),
            "DM summary should not include source_label prefix, got: {text}"
        );
    }

    // -- format_episodic_for_injection ----------------------------------------

    use alms_core::Timestamp;
    use alms_session::SessionSummary;

    fn make_summary(summary: &str, minutes_ago: i64) -> SessionSummary {
        make_summary_with_label(summary, minutes_ago, Some("User chat"))
    }

    fn make_summary_with_label(
        summary: &str,
        minutes_ago: i64,
        label: Option<&str>,
    ) -> SessionSummary {
        let ts = chrono::Utc::now() - chrono::Duration::minutes(minutes_ago);
        SessionSummary {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            summary: summary.to_string(),
            last_run_id: None,
            updated_at: Timestamp(ts),
            source_label: label.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_format_episodic_empty_input() {
        assert!(format_episodic_for_injection(&[], 1000).is_none());
    }

    #[test]
    fn test_format_episodic_zero_budget() {
        let summaries = vec![make_summary("Hello world", 5)];
        assert!(format_episodic_for_injection(&summaries, 0).is_none());
    }

    #[test]
    fn test_format_episodic_single_summary() {
        let summaries = vec![make_summary("Debugged CORS issue in gateway.rs.", 10)];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();
        assert!(result.starts_with("[Your conversation history across sessions"));
        assert!(result.contains("Debugged CORS issue"));
        assert!(result.contains("**User chat (last active:"));
    }

    #[test]
    fn test_format_episodic_multiple_summaries_preserves_order() {
        let summaries = vec![
            make_summary("Most recent session activity.", 5),
            make_summary("Older session activity.", 60),
        ];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();
        // Most recent should appear before older in the string
        let pos_recent = result.find("Most recent").unwrap();
        let pos_older = result.find("Older session").unwrap();
        assert!(
            pos_recent < pos_older,
            "Most recent summary should appear first in the output"
        );
    }

    #[test]
    fn test_format_episodic_budget_trims_oldest() {
        // Create summaries where fitting all of them would exceed a small budget.
        let summaries = vec![
            make_summary("Summary A with some text.", 5),
            make_summary("Summary B with some text.", 10),
            make_summary("Summary C with some text.", 15),
        ];
        // Each summary entry is roughly: "\n**Session (last active: ...)**\n<text>"
        // which is about 70-80 chars => ~25 tokens.  Header is ~20 tokens.
        // Budget of 70 should fit header + 1-2 entries but not all 3.
        let result = format_episodic_for_injection(&summaries, 70).unwrap();
        assert!(result.contains("Summary A"), "Newest must be included");
        // Oldest (C) should be dropped
        assert!(
            !result.contains("Summary C"),
            "Oldest summary should be dropped when budget is tight"
        );
    }

    #[test]
    fn test_format_episodic_skips_empty_summaries() {
        let summaries = vec![
            make_summary("", 5),
            make_summary("Real summary here.", 10),
            make_summary("", 15),
        ];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();
        assert!(result.contains("Real summary here."));
        // Only the header and one entry -- no blank entries
        let session_count = result.matches("**User chat").count();
        assert_eq!(session_count, 1, "Only non-empty summaries should appear");
    }

    #[test]
    fn test_format_episodic_all_empty_returns_none() {
        let summaries = vec![make_summary("", 5), make_summary("", 10)];
        assert!(format_episodic_for_injection(&summaries, 5000).is_none());
    }

    // -- S1: source label in headers ------------------------------------------

    #[test]
    fn test_format_episodic_uses_source_label_in_header() {
        let summaries = vec![
            make_summary_with_label("Web session.", 5, Some("User chat")),
            make_summary_with_label("Telegram session.", 10, Some("Telegram chat")),
        ];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();
        assert!(
            result.contains("**User chat (last active:"),
            "Should use 'User chat' label"
        );
        assert!(
            result.contains("**Telegram chat (last active:"),
            "Should use 'Telegram chat' label"
        );
    }

    // -- S2: defense-in-depth: entries without source_label are skipped --------

    #[test]
    fn test_format_episodic_skips_no_source_label() {
        let summaries = vec![
            make_summary_with_label("Labelled entry.", 5, Some("User chat")),
            make_summary_with_label("No label entry.", 10, None),
            make_summary_with_label("Empty label entry.", 15, Some("")),
        ];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();
        assert!(result.contains("Labelled entry."));
        assert!(
            !result.contains("No label entry."),
            "Entries without source_label should be filtered out"
        );
        assert!(
            !result.contains("Empty label entry."),
            "Entries with empty source_label should be filtered out"
        );
    }

    #[test]
    fn test_format_episodic_all_missing_labels_returns_none() {
        let summaries = vec![
            make_summary_with_label("Entry A.", 5, None),
            make_summary_with_label("Entry B.", 10, None),
        ];
        assert!(
            format_episodic_for_injection(&summaries, 5000).is_none(),
            "All entries without labels should result in None"
        );
    }

    // -- S4: integration test (DB -> format -> inject) -------------------------

    /// Integration test: insert summaries into a real (in-memory) SQLite store,
    /// load them, format them, and verify the episodic content that would be
    /// injected into the context window.
    #[test]
    fn test_integration_db_to_formatted_injection() {
        use alms_session::Session;
        use alms_session::sqlite::SqliteStore;

        let store = SqliteStore::open_in_memory().unwrap();
        let agent_id = AgentId::new();

        // Create three sessions from different channels.
        let s_web = Session::new(agent_id, "web-chat-1");
        let s_tg = Session::new(agent_id, "telegram_bot_123");
        let s_current = Session::new(agent_id, "web-chat-2");
        store.save_session(&s_web).unwrap();
        store.save_session(&s_tg).unwrap();
        store.save_session(&s_current).unwrap();

        // Insert summaries with source labels (as the generation code would).
        store
            .upsert_session_summary(
                agent_id,
                s_web.id,
                "Debugged CORS issue in gateway config.",
                None,
                Some("User chat"),
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .upsert_session_summary(
                agent_id,
                s_tg.id,
                "Discussed deployment on VPS.",
                None,
                Some("Telegram chat"),
            )
            .unwrap();

        // Load summaries excluding the current session.
        let summaries = store
            .load_session_summaries(agent_id, 50, Some(&s_current.id))
            .unwrap();
        assert_eq!(
            summaries.len(),
            2,
            "Should load 2 summaries (current excluded)"
        );

        // Format for injection.
        let formatted =
            format_episodic_for_injection(&summaries, 5000).expect("Should produce formatted text");

        // Verify structure.
        assert!(
            formatted.starts_with("[Your conversation history across sessions"),
            "Should start with the header"
        );
        assert!(
            formatted.contains("**Telegram chat (last active:"),
            "Telegram summary should have correct source label"
        );
        assert!(
            formatted.contains("**User chat (last active:"),
            "Web summary should have correct source label"
        );
        assert!(
            formatted.contains("Debugged CORS issue"),
            "Web summary content should be present"
        );
        assert!(
            formatted.contains("Discussed deployment"),
            "Telegram summary content should be present"
        );

        // Verify ordering: most recent (Telegram, inserted second) should appear first.
        let tg_pos = formatted.find("Discussed deployment").unwrap();
        let web_pos = formatted.find("Debugged CORS").unwrap();
        assert!(
            tg_pos < web_pos,
            "Most recent summary (Telegram) should appear before older one (web)"
        );

        // Verify that DM sessions WITH a proper source label ARE included.
        let s_dm = Session::new(agent_id, "dm:alice:bob");
        store.save_session(&s_dm).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .upsert_session_summary(
                agent_id,
                s_dm.id,
                "Discussed project architecture with bob.",
                None,
                Some("DM with bob"),
            )
            .unwrap();

        let summaries2 = store
            .load_session_summaries(agent_id, 50, Some(&s_current.id))
            .unwrap();
        assert_eq!(
            summaries2.len(),
            3,
            "Should load 3 summaries (web + tg + dm)"
        );

        let formatted2 = format_episodic_for_injection(&summaries2, 5000)
            .expect("Should produce text including DM entry");
        assert!(
            formatted2.contains("Discussed project architecture with bob"),
            "DM entry with source_label should be included"
        );
        assert!(
            formatted2.contains("**DM with bob (last active:"),
            "DM entry should have correct source label header"
        );
        // Three labelled entries should produce headers.
        let header_count = formatted2.matches("(last active:").count();
        assert_eq!(
            header_count, 3,
            "All three labelled entries should produce headers"
        );

        // Defense-in-depth: entries WITHOUT source_label are still filtered out.
        // Simulate a corrupt or manually-inserted row with no label.
        let s_orphan = Session::new(agent_id, "orphan-session");
        store.save_session(&s_orphan).unwrap();
        store
            .upsert_session_summary(
                agent_id,
                s_orphan.id,
                "Orphan content that should not appear.",
                None,
                None,
            )
            .unwrap();

        let summaries3 = store
            .load_session_summaries(agent_id, 50, Some(&s_current.id))
            .unwrap();
        assert_eq!(summaries3.len(), 4);

        let formatted3 = format_episodic_for_injection(&summaries3, 5000)
            .expect("Should still produce text from labelled entries");
        assert!(
            !formatted3.contains("Orphan content that should not appear"),
            "Entry without source_label must be filtered out"
        );
        // Only three labelled entries should produce headers.
        let header_count3 = formatted3.matches("(last active:").count();
        assert_eq!(
            header_count3, 3,
            "Only labelled entries should produce headers"
        );
    }
}
