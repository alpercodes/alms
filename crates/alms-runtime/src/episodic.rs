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

/// Default maximum output tokens for the LLM summarizer call.
///
/// Set high enough to accommodate reasoning models that consume a large portion
/// of the budget on internal thinking before producing visible output.  The
/// actual summary text is typically 50-150 tokens, but reasoning models (e.g.
/// deepseek-r1, kimi-k2.6) may spend 200-800 tokens on thinking first.
///
/// Configurable via [`ContextConfig::summary_max_tokens`].
///
/// Kept as a reference constant; the runtime value is always provided via
/// `SummaryParams::summary_max_tokens` (sourced from config).
#[allow(dead_code)]
const LLM_MAX_OUTPUT_TOKENS: u32 = 1000;

/// Parameters for episodic summary generation.
pub struct SummaryParams {
    pub mode: RunSummaryMode,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub run_input: String,
    pub run_output: String,
    /// Extended-thinking trace from the final LLM turn, if any.
    ///
    /// Stripped from `run_output` before either summarizer mode ingests
    /// it (#1098). In the `[Thinking]`-only reasoning-promotion fallback
    /// the runtime surfaces the reasoning trace on BOTH `RunOutput.response`
    /// (so the UI has something to display) AND `RunOutput.reasoning`
    /// (so this strip path can drop it). Without this, the summarizer
    /// would faithfully reproduce the entire thinking trace as the
    /// session summary — see issue #1098.
    pub run_reasoning: Option<String>,
    pub context_id: String,
    /// Existing summary for this session (if any), loaded from the DB.
    pub existing_summary: Option<String>,
    /// Model to use for LLM mode.  Falls back to the LLM client's default.
    pub summary_model: Option<String>,
    /// Agent name — used to derive the peer in DM sessions.
    pub agent_name: String,
    /// Maximum output tokens for the LLM summarizer call.
    /// The gateway always provides this from [`ContextConfig::summary_max_tokens`].
    pub summary_max_tokens: u32,
}

/// Strip the agent's extended-thinking trace from `run_output` before it is
/// passed to either summarizer mode (#1098).
///
/// **Why:** when the runtime emits a `[Thinking]`-only turn (reasoning model
/// hit `max_tokens` before producing visible output), the trace is promoted
/// into `RunOutput.response` so the run still has something to display —
/// but the same string is ALSO surfaced on `RunOutput.reasoning` precisely
/// so this strip path can identify and drop it. Without the strip the
/// summarizer would treat the reasoning trace as the agent's "response"
/// and faithfully reproduce it in the session summary, polluting
/// `session_summaries.summary` with multi-paragraph internal deliberation.
///
/// **Behavior:**
///
/// - `run_reasoning = None` or empty → return `run_output` unchanged
///   (the common `[Text]` or `[Thinking, Text]` case).
/// - `run_output` equals `run_reasoning` → return `""` (the reasoning-
///   promotion fallback case from `finalize_content_and_reasoning`).
/// - `run_output` starts with the reasoning trace → strip the prefix.
///   This is **defense-in-depth, not exercised by any current provider**:
///   it covers a hypothetical future regression where a provider adapter
///   concatenates `reasoning_content` and `content` instead of keeping
///   them on separate channels. No regression test reproduces this case
///   in production today; the branch is here precisely because the cost
///   of carrying it is negligible and the cost of summary pollution from
///   a silent provider-side change is not.
/// - Otherwise → return `run_output` unchanged.
///
/// The function is intentionally conservative: it never strips arbitrary
/// substrings, only exact-equal or prefix matches against the known
/// reasoning trace. False positives would silently drop legitimate
/// response text from the summary, which is worse than a too-long
/// summary.
fn strip_reasoning_from_output(run_output: &str, run_reasoning: Option<&str>) -> String {
    let Some(reasoning) = run_reasoning else {
        return run_output.to_string();
    };
    if reasoning.is_empty() {
        return run_output.to_string();
    }
    if run_output == reasoning {
        return String::new();
    }
    if let Some(stripped) = run_output.strip_prefix(reasoning) {
        return stripped.trim_start().to_string();
    }
    run_output.to_string()
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
        RunSummaryMode::Llm => apply_llm_fallback(generate_llm(llm, params).await, params),
    }
}

/// Apply the LLM-mode fallback policy: if the LLM path produced a summary,
/// return it; otherwise fall back to the deterministic heuristic — but only
/// when there is no accumulated history to protect.
///
/// The LLM path returns `None` when the provider call errored, returned an
/// empty response, or both `run_input` and `run_output` were empty.  In all
/// of those cases we'd previously persist nothing, leaving the session
/// silently summary-less (#832).  The heuristic produces a deterministic
/// "input -> output" line and is lossy but never silent.
///
/// **Existing-summary safeguard (PR #884 follow-up):** when
/// `params.existing_summary` is non-empty, the heuristic is *not* invoked.
/// Concatenating a 60-byte heuristic line onto a 450-byte LLM-mode paragraph
/// and feeding it through `trim_oldest_lines(_, 500)` would evict the older
/// (LLM) line — silently dropping real accumulated history on every transient
/// LLM failure.  Returning `None` here means the upsert path is skipped and
/// the existing summary is preserved verbatim.  This is strictly safer than
/// risking eviction of real content for a deterministic stub.
///
/// The heuristic itself may also return `None` (e.g. excluded session via
/// `derive_source_label`, or empty `run_input`); in that case the silent-skip
/// behavior is preserved as before.
fn apply_llm_fallback(llm_result: Option<String>, params: &SummaryParams) -> Option<String> {
    if let Some(text) = llm_result {
        return Some(text);
    }
    // Existing summary is load-bearing -- don't risk it via trim eviction.
    if params
        .existing_summary
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        info!(
            "LLM summary path produced no output -- preserving existing summary (heuristic fallback skipped)"
        );
        return None;
    }
    // No accumulated history -- heuristic is strictly an upgrade over silent skip.
    info!("LLM summary path produced no output -- falling back to heuristic");
    generate_heuristic(params)
}

/// Parameters for the fire-and-forget summary generation + persistence.
pub struct PersistSummaryRequest {
    pub mode: RunSummaryMode,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub run_input: String,
    pub run_output: String,
    /// Extended-thinking trace from the final LLM turn, if any.  Stripped
    /// from `run_output` before the summarizer ingests it (#1098).
    pub run_reasoning: Option<String>,
    pub context_id: String,
    pub summary_model: Option<String>,
    /// Agent name — used to derive the peer in DM sessions.
    pub agent_name: String,
    /// Maximum output tokens for the LLM summarizer call.
    /// The gateway always provides this from [`ContextConfig::summary_max_tokens`].
    pub summary_max_tokens: u32,
}

/// Fire-and-forget summary generation + persistence.
///
/// Called from the gateway after a successful run.  Loads the existing summary
/// (if any), generates a new one, and upserts it to the database.  All errors
/// are logged and swallowed -- summary failure must never fail the run.
///
/// **Concurrent runs** on the same session are reconciled via optimistic
/// locking on the `last_run_id` sentinel (#1123).  The upsert only commits if
/// the stored `last_run_id` still matches the one observed when the base
/// summary was loaded; on conflict the summary is regenerated from the
/// reloaded base and the upsert is retried with exponential backoff (up to 3
/// attempts).  Exhausting the retries logs an `error!` and drops the update
/// rather than failing the run.
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
    // Extract req fields immediately to prevent ownership / move errors during retries.
    let req_mode = req.mode;
    let req_agent_id = req.agent_id;
    let req_session_id = req.session_id;
    let req_run_id = req.run_id;
    let req_run_input = req.run_input;
    let req_run_output = req.run_output;
    let req_run_reasoning = req.run_reasoning;
    let req_context_id = req.context_id;
    let req_summary_model = req.summary_model;
    let req_agent_name = req.agent_name;
    let req_summary_max_tokens = req.summary_max_tokens;

    // Check if the session type should be summarised.
    let source = match derive_source_label(&req_context_id, &req_agent_name) {
        Some(s) => s,
        None => {
            debug!(
                context_id = req_context_id.as_str(),
                "Skipping summary for excluded session type"
            );
            return;
        }
    };

    // Load existing summary from DB (if any) alongside last_run_id for optimistic locking.
    let (existing_summary, expected_last_run_id) = match session_manager.store() {
        Some(store) => match store.load_session_summary(req_agent_id, req_session_id) {
            Ok(Some(s)) => (Some(s.summary), s.last_run_id),
            Ok(None) => (None, None),
            Err(e) => {
                warn!("Failed to load existing session summary: {e}");
                (None, None)
            }
        },
        None => (None, None),
    };

    let mut current_existing_summary = existing_summary;
    let mut current_expected_last_run_id = expected_last_run_id;

    if let Some(store) = session_manager.store() {
        let mut persisted = false;

        // Exponential backoff: 50ms doubling per attempt (attempt 1 -> 50ms,
        // 2 -> 100ms, 3 -> 200ms). Used by every retry arm so no failure path
        // hot-loops the upsert.
        async fn backoff(attempt: i32) {
            let delay_ms = 50u64 * (1u64 << ((attempt.max(1) - 1) as u32));
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }

        // Retry up to 3 times on conflict with exponential backoff.
        for attempt in 1..=3 {
            let params = SummaryParams {
                mode: req_mode.clone(),
                agent_id: req_agent_id,
                session_id: req_session_id,
                run_id: req_run_id,
                run_input: req_run_input.clone(),
                run_output: req_run_output.clone(),
                run_reasoning: req_run_reasoning.clone(),
                context_id: req_context_id.clone(),
                existing_summary: current_existing_summary.clone(),
                summary_model: req_summary_model.clone(),
                agent_name: req_agent_name.clone(),
                summary_max_tokens: req_summary_max_tokens,
            };

            let summary_text = match generate_session_summary(llm, &params).await {
                Some(text) => text,
                None => {
                    debug!("Summary generation produced no result (attempt {attempt})");
                    return;
                }
            };

            info!(
                attempt,
                source_type = source.source_type.as_str(),
                source_label = source.source_label.as_str(),
                summary_len = summary_text.len(),
                "Session summary generated"
            );

            // Attempt optimistic upsert check
            match store.upsert_session_summary_optimistic(
                params.agent_id,
                params.session_id,
                &summary_text,
                Some(params.run_id),
                Some(&source.source_label),
                current_expected_last_run_id,
            ) {
                Ok(true) => {
                    persisted = true;
                    break;
                }
                Ok(false) => {
                    // Conflict detected: another run updated the summary in the meantime.
                    if attempt == 3 {
                        error!(
                            attempts = 3,
                            "Failed to persist session summary due to concurrent updates"
                        );
                        break;
                    }
                    warn!(
                        attempt,
                        "Conflict detected during session summary upsert, reloading and retrying..."
                    );
                    // Reload latest from DB and update expected_last_run_id +
                    // current_existing_summary so the next attempt carries the
                    // current sentinel.
                    match store.load_session_summary(req_agent_id, req_session_id) {
                        Ok(Some(s)) => {
                            current_existing_summary = Some(s.summary);
                            current_expected_last_run_id = s.last_run_id;
                        }
                        Ok(None) => {
                            current_existing_summary = None;
                            current_expected_last_run_id = None;
                        }
                        Err(e) => {
                            // Reloading the latest summary itself failed. We now
                            // hold a known-stale `expected_last_run_id`; re-running
                            // the optimistic upsert with it is guaranteed to
                            // conflict again, burning every remaining attempt for
                            // nothing. Surface the reload failure and stop rather
                            // than hot-looping on a sentinel we already know is
                            // stale — the next run on this session will retry from
                            // a fresh load.
                            error!(
                                attempt,
                                "Failed to reload session summary after conflict; aborting retries: {e}"
                            );
                            break;
                        }
                    }
                    backoff(attempt).await;
                }
                Err(e) if attempt < 3 => {
                    // SQLite busy/transient error: back off and retry the upsert
                    // with the same expected version (the row was not mutated).
                    warn!(
                        attempt,
                        "Transient failure persisting session summary, retrying: {e}"
                    );
                    backoff(attempt).await;
                }
                Err(e) => {
                    error!(
                        attempts = 3,
                        "Failed to persist session summary after transient errors: {e}"
                    );
                }
            }
        }

        if persisted {
            debug!("Session summary persisted successfully");
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

    // #1098: strip the extended-thinking trace from the run output so
    // the heuristic summary never quotes reasoning content. See
    // `strip_reasoning_from_output` for the matching rules.
    let sanitized_output =
        strip_reasoning_from_output(&params.run_output, params.run_reasoning.as_deref());

    // Build the new entry line.
    let in_snippet = truncate_to_char_boundary(&params.run_input, HEURISTIC_INPUT_BYTES);
    let in_ellipsis = if params.run_input.len() > in_snippet.len() {
        "..."
    } else {
        ""
    };

    // Include the agent's response when available (#434, Bug 2).
    let entry = if !sanitized_output.is_empty() {
        let out_snippet = truncate_to_char_boundary(&sanitized_output, HEURISTIC_OUTPUT_BYTES);
        let out_ellipsis = if sanitized_output.len() > out_snippet.len() {
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

/// Extract the peer agent name from a DM `context_id`.
///
/// Delegates to [`alms_core::dm_peer`].  Returns `None` for non-DM sessions
/// or malformed context IDs.
fn extract_dm_peer(context_id: &str, agent_name: &str) -> Option<String> {
    alms_core::dm_peer(context_id, agent_name).map(|s| s.to_string())
}

/// Generate a summary via a lightweight LLM call.
async fn generate_llm(llm: &LlmClient, params: &SummaryParams) -> Option<String> {
    // #1098: strip the extended-thinking trace from the run output before
    // anything else so the summarizer LLM never sees reasoning content as
    // "agent response". The strip happens up front so the early-return
    // emptiness check below evaluates against the sanitized output and a
    // reasoning-only run produces no summary (instead of an LLM-rephrased
    // reasoning dump).
    let sanitized_output =
        strip_reasoning_from_output(&params.run_output, params.run_reasoning.as_deref());

    if params.run_input.is_empty() && sanitized_output.is_empty() {
        return None;
    }

    let system_prompt = include_str!("../prompts/session_summarizer.md").trim();

    // Detect DM sessions and extract the peer agent name so the summarizer
    // uses the actual agent name instead of "user".
    let dm_peer = extract_dm_peer(&params.context_id, &params.agent_name);

    let mut user_content = String::new();

    // For DM sessions, prepend context so the LLM knows who the participants
    // are and uses actual agent names instead of generic "user"/"agent".
    if let Some(ref peer) = dm_peer {
        user_content.push_str(&format!(
            "Context: This is a DM conversation between agent \"{}\" and agent \"{peer}\". \
             Use their actual names in the summary, not \"user\" or \"agent\".\n\n",
            params.agent_name
        ));
    }

    // Include existing summary for the LLM to extend.
    if let Some(ref existing) = params.existing_summary
        && !existing.is_empty()
    {
        user_content.push_str("Existing session summary:\n");
        user_content.push_str(existing);
        user_content.push_str("\n\n---\n\nNew interaction to incorporate:\n\n");
    }

    // Truncated run input.
    // For DM sessions, label with the peer's name instead of "User".
    let input_snippet = truncate_to_char_boundary(&params.run_input, LLM_CONTEXT_BYTES);
    let input_label = match &dm_peer {
        Some(peer) => format!("{peer}'s message"),
        None => "User input".to_string(),
    };
    user_content.push_str(&format!("{input_label}:\n"));
    user_content.push_str(input_snippet);
    if params.run_input.len() > input_snippet.len() {
        user_content.push_str("\n[...truncated]");
    }

    // Truncated run output.
    // For DM sessions, label with this agent's name instead of "Agent".
    // #1098: use the sanitized output (with reasoning stripped) so the
    // summarizer LLM never quotes thinking traces.
    if !sanitized_output.is_empty() {
        let output_snippet = truncate_to_char_boundary(&sanitized_output, LLM_CONTEXT_BYTES);
        let output_label = match &dm_peer {
            Some(_) => format!("{}'s response", params.agent_name),
            None => "Agent response".to_string(),
        };
        user_content.push_str(&format!("\n\n{output_label}:\n"));
        user_content.push_str(output_snippet);
        if sanitized_output.len() > output_snippet.len() {
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
        .with_max_tokens(params.summary_max_tokens);

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
    // I8: Reserve tokens for the XML wrapper tags that delimit episodic data.
    let xml_overhead_tokens =
        estimate_tokens("<episodic_memory>\n") + estimate_tokens("\n</episodic_memory>");
    let header_tokens = estimate_tokens(header) + xml_overhead_tokens;
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

        // Format: **<source_label> — session <id8> (last active: <date>)**\n<summary>
        //
        // The 8-char prefix of the SessionId UUID is sufficient for the agent
        // to plug into `read_session` (paired with one `list_my_sessions` call
        // for prefix verification if needed). The full UUID would add ~28
        // chars per entry; the 8-char form keeps the token overhead small
        // while making episodic memory actionable (#1106).
        let updated = summary.updated_at.0.format("%Y-%m-%d %H:%M UTC");
        let session_id_str = summary.session_id.0.to_string();
        let id_prefix = &session_id_str[..session_id_str.len().min(8)];
        let entry = format!(
            "\n**{label} — session {id_prefix} (last active: {updated})**\n{}",
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

    // I8: Wrap the entire episodic block in XML tags to clearly delimit it as
    // data (not instructions).  This reduces the risk of a compromised
    // summariser injecting content that the LLM treats as system instructions.
    let open_tag = "<episodic_memory>\n";
    let close_tag = "\n</episodic_memory>";

    // Pre-allocate: budget_tokens * 3 approximates the char capacity since
    // estimate_tokens uses chars/3.
    let mut result = String::with_capacity(budget_tokens * 3);
    result.push_str(open_tag);
    result.push_str(header);
    for entry in &entries {
        result.push_str(entry);
    }
    result.push_str(close_tag);

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
            run_reasoning: None,
            context_id: context_id.to_string(),
            existing_summary: existing.map(|s| s.to_string()),
            summary_model: None,
            agent_name: agent_name.to_string(),
            summary_max_tokens: 1000,
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

    // -- #1098 reasoning-strip regression -----------------------------------
    //
    // These tests validate the fix for issue #1098: the episodic summarizer
    // must NOT ingest extended-thinking traces emitted by reasoning models.
    //
    // The leak path: when the runtime takes the `[Thinking]`-only fallback
    // in `finalize_content_and_reasoning` (reasoning model exhausted
    // `max_tokens` before producing visible output), the entire reasoning
    // trace is promoted into `RunOutput.response`. Pre-fix that string
    // would flow to `run_output` and the summarizer (both heuristic and
    // LLM modes) would faithfully quote / paraphrase the reasoning into
    // `session_summaries.summary`. Post-fix the runtime ALSO surfaces
    // the same trace on `RunOutput.reasoning`, the gateway threads it
    // through as `SummaryParams.run_reasoning`, and `strip_reasoning_from_output`
    // drops it before either summarizer mode sees it.

    /// Pure-function unit tests for the strip helper.  Walks every branch.
    #[test]
    fn test_strip_reasoning_none_passes_through() {
        assert_eq!(
            strip_reasoning_from_output("plain response", None),
            "plain response"
        );
    }

    #[test]
    fn test_strip_reasoning_empty_passes_through() {
        // Empty reasoning means the final turn had no thinking deltas.
        assert_eq!(
            strip_reasoning_from_output("plain response", Some("")),
            "plain response"
        );
    }

    #[test]
    fn test_strip_reasoning_exact_match_returns_empty() {
        // The `[Thinking]`-only fallback case: response IS the reasoning.
        let trace = "Let me reason about this. Step 1... step 2... step 3...";
        assert_eq!(strip_reasoning_from_output(trace, Some(trace)), "");
    }

    #[test]
    fn test_strip_reasoning_prefix_match_strips_prefix() {
        // Defense-in-depth: if a future provider regression concatenates
        // reasoning + visible content into one string, strip the prefix.
        let response = "Let me think step by step. Final answer is 42.";
        let reasoning = "Let me think step by step.";
        assert_eq!(
            strip_reasoning_from_output(response, Some(reasoning)),
            "Final answer is 42."
        );
    }

    #[test]
    fn test_strip_reasoning_non_prefix_passes_through() {
        // Conservative: don't strip arbitrary substrings.
        assert_eq!(
            strip_reasoning_from_output(
                "The final answer is 42.",
                Some("internal thinking about the problem"),
            ),
            "The final answer is 42."
        );
    }

    /// Heuristic regression: pre-fix, an assistant turn that fell back to
    /// reasoning-as-response would put the whole reasoning trace in the
    /// heuristic summary. Post-fix the heuristic shows the input only.
    #[test]
    fn test_heuristic_strips_reasoning_when_response_is_reasoning_fallback() {
        // Simulate `RunOutput` from the `[Thinking]`-only fallback path:
        // both response and reasoning carry the same trace.
        let reasoning_trace = "I need to think carefully. The user is asking \
            for X. Let me consider Y. Actually wait, maybe Z. \
            Hmm, let me reconsider — yes, Z is correct because...";
        let mut params = heuristic_params("What's the answer to my question?", "web-chat-1", None);
        params.run_output = reasoning_trace.to_string();
        params.run_reasoning = Some(reasoning_trace.to_string());

        let result = generate_heuristic(&params).expect("should produce a summary");

        assert!(
            !result.contains("think carefully"),
            "Heuristic summary must not include reasoning trace text, got: {result}"
        );
        assert!(
            !result.contains("reconsider"),
            "Heuristic summary must not include reasoning trace text, got: {result}"
        );
        // Input is still summarized — only the reasoning-as-output is dropped.
        assert!(
            result.contains("What's the answer to my question?"),
            "Heuristic summary must still include the user input, got: {result}"
        );
        // Output side should be empty (no `->` separator).
        assert!(
            !result.contains("->"),
            "Heuristic summary must not include an output arrow when response is reasoning, got: {result}"
        );
    }

    /// Heuristic happy path: `[Thinking, Text]` turns where response and
    /// reasoning differ — the strip must NOT remove legitimate response text.
    #[test]
    fn test_heuristic_preserves_response_when_reasoning_is_distinct() {
        let mut params = heuristic_params("How do I configure CORS?", "web-chat-1", None);
        params.run_output = "Set the Access-Control-Allow-Origin header.".to_string();
        params.run_reasoning = Some("The user wants CORS help. CORS is about cross-origin requests. The header is the right answer.".to_string());

        let result = generate_heuristic(&params).expect("should produce a summary");

        assert!(
            result.contains("Set the Access-Control-Allow-Origin header"),
            "Heuristic must preserve legitimate response, got: {result}"
        );
        // The reasoning text must not leak in.
        assert!(
            !result.contains("cross-origin requests"),
            "Reasoning content must not leak into the heuristic summary, got: {result}"
        );
    }

    /// LLM-mode regression: pre-fix, when the runtime emits a `[Thinking]`-
    /// only fallback the summarizer LLM would see the reasoning trace as
    /// the "Agent response" and faithfully paraphrase it. Post-fix the
    /// summarizer prompt omits the response section entirely.
    #[tokio::test]
    async fn test_llm_summary_strips_reasoning_when_response_is_reasoning_fallback() {
        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();
        let reasoning_trace = "Let me think... I'll consider option A vs B vs C. Going with B.";
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "Pick an option for me".into(),
            run_output: reasoning_trace.to_string(),
            run_reasoning: Some(reasoning_trace.to_string()),
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        // The mock LLM echoes back the user_content sent to it. After the
        // strip the user_content should not contain the reasoning trace
        // (no "Agent response" section).
        let result = generate_session_summary(&llm, &params).await;
        let summary = result.expect("should produce a summary");
        // The mock prefixes with "[mock] " and returns the assembled user
        // content verbatim. If the strip did its job, the assembled
        // content has only the input section, no output section, and
        // therefore no reasoning text.
        assert!(
            !summary.contains("option A vs B vs C"),
            "LLM summary must not quote reasoning trace, got: {summary}"
        );
        assert!(
            !summary.contains("Agent response"),
            "LLM summarizer must not emit an Agent response section when response is reasoning, got: {summary}"
        );
        // The user input section should still be present.
        assert!(
            summary.contains("User input"),
            "LLM summarizer must still include the user input section, got: {summary}"
        );
    }

    /// LLM-mode happy path: distinct response and reasoning — the response
    /// must reach the summarizer.
    #[tokio::test]
    async fn test_llm_summary_preserves_response_when_reasoning_is_distinct() {
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
            run_input: "How do I configure CORS?".into(),
            run_output: "Set the Access-Control-Allow-Origin header.".into(),
            run_reasoning: Some(
                "Thinking about CORS — it's about cross-origin requests.".to_string(),
            ),
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        let summary = generate_session_summary(&llm, &params)
            .await
            .expect("should produce a summary");
        // The legitimate response must reach the summarizer (and the mock
        // echoes it back).
        assert!(
            summary.contains("Set the Access-Control-Allow-Origin header"),
            "LLM summary must preserve legitimate response, got: {summary}"
        );
        // The reasoning text must not leak in.
        assert!(
            !summary.contains("cross-origin requests"),
            "Reasoning content must not leak into the LLM summary, got: {summary}"
        );
    }

    /// Heuristic: empty input + reasoning-as-output should produce nothing.
    /// Pre-fix the heuristic would happily quote the reasoning trace; post-fix
    /// the run_input check fires first (heuristic returns None on empty
    /// input) — verify the strip is order-safe.
    #[test]
    fn test_heuristic_empty_input_with_reasoning_fallback_returns_none() {
        let mut params = heuristic_params("", "web-chat-1", None);
        params.run_output = "internal reasoning trace".to_string();
        params.run_reasoning = Some("internal reasoning trace".to_string());
        assert!(generate_heuristic(&params).is_none());
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
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
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
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
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
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
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
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: Some("Debugged CORS issue in gateway.rs.".into()),
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
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
                run_reasoning: None,
                context_id: ctx.to_string(),
                existing_summary: None,
                summary_model: None,
                agent_name: "myagent".into(),
                summary_max_tokens: 1000,
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
            run_reasoning: None,
            context_id: "dm:alice:bob".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "alice".into(),
            summary_max_tokens: 1000,
        };
        let result = generate_session_summary(&llm, &params).await;
        assert!(result.is_some(), "DM sessions should now produce summaries");
        let text = result.unwrap();
        assert!(
            text.starts_with('"'),
            "DM summary should not include source_label prefix, got: {text}"
        );
    }

    // -- LLM-mode fallback to heuristic (#832) -----------------------------

    /// LLM path errors -> apply_llm_fallback invokes the heuristic and
    /// produces a heuristic-shaped string instead of silently dropping the
    /// summary.
    #[test]
    fn test_llm_fallback_on_error_uses_heuristic() {
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "How do I configure CORS?".into(),
            run_output: "Set Access-Control-Allow-Origin.".into(),
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        // Simulate the LLM path having errored out (returns None, mirroring
        // the `error!("LLM summarizer call failed: {e}")` branch).
        let result = apply_llm_fallback(None, &params);
        let text = result.expect("fallback should produce a heuristic summary");
        // Heuristic shape: `"<input>" -> "<output>"`
        assert_eq!(
            text,
            "\"How do I configure CORS?\" -> \"Set Access-Control-Allow-Origin.\""
        );
    }

    /// LLM path returns empty content -> apply_llm_fallback invokes the
    /// heuristic and produces a heuristic-shaped string instead of silently
    /// dropping the summary.
    #[test]
    fn test_llm_fallback_on_empty_uses_heuristic() {
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "Help me debug this error".into(),
            run_output: "The issue is in your config file.".into(),
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        // Simulate the LLM path having returned empty content (returns None,
        // mirroring the `warn!("LLM summarizer returned empty response")`
        // branch -- e.g. a reasoning model that exhausted summary_max_tokens
        // on thinking with no visible output, or a transient provider hiccup).
        let result = apply_llm_fallback(None, &params);
        let text = result.expect("fallback should produce a heuristic summary");
        assert_eq!(
            text,
            "\"Help me debug this error\" -> \"The issue is in your config file.\""
        );
    }

    /// When the LLM path succeeds, the fallback is a no-op pass-through and
    /// the heuristic is NOT invoked.
    #[test]
    fn test_llm_fallback_passthrough_on_success() {
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "anything".into(),
            run_output: "anything".into(),
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        let llm_text = "LLM-generated summary text".to_string();
        let result = apply_llm_fallback(Some(llm_text.clone()), &params);
        assert_eq!(result, Some(llm_text));
    }

    /// When the LLM path returns None AND the heuristic would also return
    /// None (e.g. excluded subagent session), the fallback returns None as
    /// well -- the silent-skip path is preserved for sessions that genuinely
    /// shouldn't be summarized.
    #[test]
    fn test_llm_fallback_returns_none_when_heuristic_also_skips() {
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "hello".into(),
            run_output: "world".into(),
            run_reasoning: None,
            // Subagent context is excluded by derive_source_label, so the
            // heuristic returns None.
            context_id: "subagent_task_1".into(),
            existing_summary: None,
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        assert!(apply_llm_fallback(None, &params).is_none());
    }

    /// Regression test for PR #884 follow-up: when the LLM path fails *and*
    /// `existing_summary` is non-empty (a typical LLM-mode paragraph), the
    /// fallback must NOT invoke the heuristic.  Otherwise the heuristic would
    /// concatenate a short heuristic line onto the long existing paragraph and
    /// `trim_oldest_lines(_, 500)` would evict the paragraph — silently
    /// dropping real history on every transient LLM failure.
    ///
    /// Expected behavior: `apply_llm_fallback` returns `None`, the upsert
    /// path is skipped, and the existing summary stays intact in the DB.
    #[test]
    fn test_llm_fallback_preserves_existing_summary_on_failure() {
        // 450-byte LLM-mode paragraph -- representative of accumulated history.
        let existing = "x".repeat(450);
        let params = SummaryParams {
            mode: RunSummaryMode::Llm,
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            run_input: "Follow-up question after a failure".into(),
            run_output: "Some new response".into(),
            run_reasoning: None,
            context_id: "web-chat-1".into(),
            existing_summary: Some(existing.clone()),
            summary_model: None,
            agent_name: "myagent".into(),
            summary_max_tokens: 1000,
        };
        // Simulate LLM failure (provider call errored or empty content).
        let result = apply_llm_fallback(None, &params);
        assert!(
            result.is_none(),
            "Must return None to preserve existing summary, got: {result:?}"
        );

        // Sanity check: verify the bug we're guarding against -- the heuristic,
        // if invoked, would have evicted the 450-byte paragraph and persisted
        // only the new heuristic-shaped line, silently losing the history.
        let heuristic_result = generate_heuristic(&params).expect("heuristic produces a string");
        assert!(
            !heuristic_result.contains(&existing),
            "Heuristic eviction precondition: with a 450-byte existing summary + heuristic line, \
             trim_oldest_lines(_, 500) drops the older line. If this assertion ever fails, the \
             eviction risk is gone and the safeguard above can be reconsidered."
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
        // I8: output is wrapped in XML tags
        assert!(
            result.starts_with("<episodic_memory>\n[Your conversation history across sessions")
        );
        assert!(result.ends_with("</episodic_memory>"));
        assert!(result.contains("Debugged CORS issue"));
        // #1106: header now includes session_id prefix between label and date.
        assert!(result.contains("**User chat — session "));
        assert!(result.contains("(last active:"));
        // Verify the actual 8-char prefix of the session_id is in the header.
        let session_id_str = summaries[0].session_id.0.to_string();
        let id_prefix = &session_id_str[..8];
        assert!(
            result.contains(&format!("**User chat — session {id_prefix} (last active:")),
            "Header must contain the 8-char session_id prefix between label and date \
             (got: {result})"
        );
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
        // Each summary entry is roughly:
        //   "\n**User chat — session xxxxxxxx (last active: ...)**\n<text>"
        // which is about 90-100 chars => ~30 tokens (per #1106 the header now
        // carries a 19-char session_id segment).  Header + XML wrapper is
        // ~34 tokens.  Budget of 95 should fit header + wrapper + 1-2 entries
        // but not all 3.
        let result = format_episodic_for_injection(&summaries, 95).unwrap();
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
        // #1106: header format is `**<label> — session <id8> (last active: ...)`
        assert!(
            result.contains("**User chat — session "),
            "Should use 'User chat' label (got: {result})"
        );
        assert!(
            result.contains("**Telegram chat — session "),
            "Should use 'Telegram chat' label (got: {result})"
        );
    }

    // -- #1106: regression -- session_id is included in every entry's header --

    /// Regression for #1106 — every labelled episodic entry must carry its
    /// session_id (truncated to the first 8 chars of the UUID) in the
    /// header so the agent can directly feed it into `read_session` without
    /// first calling `list_my_sessions` to map labels back to ids.
    ///
    /// Pre-fix, `format_episodic_for_injection` emitted
    /// `**<label> (last active: <date>)**\n<summary>` with no session
    /// handle. This test fails on that shape: it builds three summaries
    /// across mixed session types (user-facing, Telegram, DM), each with
    /// distinct session_ids, and asserts every one of those ids appears as
    /// its 8-char prefix in the formatted output.
    #[test]
    fn test_format_episodic_includes_session_id_in_each_entry() {
        let summaries = vec![
            make_summary_with_label("Web session content.", 5, Some("User chat")),
            make_summary_with_label("Telegram session content.", 10, Some("Telegram chat")),
            make_summary_with_label("DM session content.", 15, Some("DM with bob")),
        ];
        let result = format_episodic_for_injection(&summaries, 5000).unwrap();

        for summary in &summaries {
            let id_prefix = &summary.session_id.0.to_string()[..8];
            assert!(
                result.contains(id_prefix),
                "Formatted episodic memory must contain session_id prefix {id_prefix} \
                 for summary {:?}. Without it, agents cannot dereference an episodic \
                 entry into a concrete session_id for read_session (#1106). Got:\n{result}",
                summary.summary
            );
        }

        // And specifically each must appear adjacent to its own label, not
        // merely somewhere in the output — guards against accidental shape
        // changes that would make the id present but unparseable.
        let web_id_prefix = &summaries[0].session_id.0.to_string()[..8];
        let tg_id_prefix = &summaries[1].session_id.0.to_string()[..8];
        let dm_id_prefix = &summaries[2].session_id.0.to_string()[..8];
        assert!(result.contains(&format!("**User chat — session {web_id_prefix} ")));
        assert!(result.contains(&format!("**Telegram chat — session {tg_id_prefix} ")));
        assert!(result.contains(&format!("**DM with bob — session {dm_id_prefix} ")));
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

        // Verify structure.  I8: output wrapped in XML tags.
        assert!(
            formatted.starts_with("<episodic_memory>\n[Your conversation history across sessions"),
            "Should start with XML tag and header"
        );
        assert!(
            formatted.ends_with("</episodic_memory>"),
            "Should end with closing XML tag"
        );
        // #1106: header format is `**<label> — session <id8> (last active: ...)`.
        assert!(
            formatted.contains("**Telegram chat — session "),
            "Telegram summary should have correct source label (got: {formatted})"
        );
        assert!(
            formatted.contains("**User chat — session "),
            "Web summary should have correct source label (got: {formatted})"
        );
        // Verify the actual 8-char session_id prefixes are present in the output.
        let web_id_prefix = &s_web.id.0.to_string()[..8];
        let tg_id_prefix = &s_tg.id.0.to_string()[..8];
        assert!(
            formatted.contains(&format!(
                "**User chat — session {web_id_prefix} (last active:"
            )),
            "Web entry must include its session_id prefix"
        );
        assert!(
            formatted.contains(&format!(
                "**Telegram chat — session {tg_id_prefix} (last active:"
            )),
            "Telegram entry must include its session_id prefix"
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
        // #1106: DM header also carries the session_id prefix.
        let dm_id_prefix = &s_dm.id.0.to_string()[..8];
        assert!(
            formatted2.contains(&format!(
                "**DM with bob — session {dm_id_prefix} (last active:"
            )),
            "DM entry should have correct source label header with session_id (got: {formatted2})"
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

    // -- extract_dm_peer -------------------------------------------------------

    #[test]
    fn test_extract_dm_peer_alice_sees_bob() {
        assert_eq!(
            extract_dm_peer("dm:alice:bob", "alice"),
            Some("bob".to_string())
        );
    }

    #[test]
    fn test_extract_dm_peer_bob_sees_alice() {
        assert_eq!(
            extract_dm_peer("dm:alice:bob", "bob"),
            Some("alice".to_string())
        );
    }

    #[test]
    fn test_extract_dm_peer_non_dm_returns_none() {
        assert_eq!(extract_dm_peer("web-chat-123", "alice"), None);
    }

    #[test]
    fn test_extract_dm_peer_malformed_dm_returns_none() {
        assert_eq!(extract_dm_peer("dm:alice", "alice"), None);
    }

    #[test]
    fn test_extract_dm_peer_agent_not_in_context_returns_none() {
        assert_eq!(extract_dm_peer("dm:alice:bob", "charlie"), None);
    }

    #[tokio::test]
    async fn test_concurrent_summary_race() {
        use alms_session::Session;

        let store = alms_session::SqliteStore::open_in_memory().unwrap();
        let session_manager = std::sync::Arc::new(
            SessionManager::with_store(alms_session::SessionConfig::default(), store).unwrap(),
        );
        let store = session_manager.store().unwrap().clone();
        let agent_id = AgentId::new();
        let s = Session::new(agent_id, "web-chat-race");
        store.save_session(&s).unwrap();

        let llm = LlmClient::new(crate::llm_types::LlmConfig {
            mock: true,
            ..Default::default()
        })
        .unwrap();

        let req_a = PersistSummaryRequest {
            mode: RunSummaryMode::Heuristic,
            agent_id,
            session_id: s.id,
            run_id: RunId::new(),
            run_input: "Input A".to_string(),
            run_output: "Output A".to_string(),
            run_reasoning: None,
            context_id: "web-chat-race".to_string(),
            summary_model: None,
            agent_name: "myagent".to_string(),
            summary_max_tokens: 1000,
        };

        let req_b = PersistSummaryRequest {
            mode: RunSummaryMode::Heuristic,
            agent_id,
            session_id: s.id,
            run_id: RunId::new(),
            run_input: "Input B".to_string(),
            run_output: "Output B".to_string(),
            run_reasoning: None,
            context_id: "web-chat-race".to_string(),
            summary_model: None,
            agent_name: "myagent".to_string(),
            summary_max_tokens: 1000,
        };

        let handle_a = tokio::spawn({
            let sm = session_manager.clone();
            let llm = llm.clone();
            async move {
                generate_and_persist_summary(&sm, &llm, req_a).await;
            }
        });

        let handle_b = tokio::spawn({
            let sm = session_manager.clone();
            let llm = llm.clone();
            async move {
                generate_and_persist_summary(&sm, &llm, req_b).await;
            }
        });

        let _ = tokio::join!(handle_a, handle_b);

        let final_summary = store.load_session_summary(agent_id, s.id).unwrap().unwrap();
        println!("Concurrent race final summary: {}", final_summary.summary);

        assert!(
            final_summary.summary.contains("Input A") && final_summary.summary.contains("Input B"),
            "Race condition lost one of the summaries! got: {}",
            final_summary.summary
        );
    }
}
