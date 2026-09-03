// SPDX-License-Identifier: Apache-2.0

//! Pure SSE event-block parsers used by the streaming pipeline.
//!
//! Extracted from `mod.rs` (#793) as free functions that take `&str` and
//! return [`SseParseResult`]. No `LlmClient` state is touched — all
//! provider-specific parsing (OpenAI `data: {json}`, Anthropic
//! `event: … data: …`, Gemini OpenAI-style `data: {json}`) lives here,
//! and `streaming.rs` + the `complete_stream` loop in `mod.rs` use
//! [`dispatch_sse_event`] to route a buffered event block to the right
//! parser based on the active [`Provider`].
//!
//! Keeping these parsers pure (Option 3 in #793) means this module has a
//! zero-surface API — no `pub(crate)` helpers need widening, no closures
//! are passed, and every function is trivially unit-testable. The state
//! (network read buffer, per-chunk timeout, provider tag) stays in
//! `streaming.rs::stream_response`.

use super::{Provider, SseParseResult};
use crate::llm_types::StreamChunk;
use tracing::warn;

/// Route an SSE event block to the appropriate provider-specific parser.
pub(crate) fn dispatch_sse_event(provider: Provider, event: &str) -> SseParseResult {
    match provider {
        Provider::OpenAi => parse_openai_sse(event),
        Provider::Anthropic => parse_anthropic_sse_block(event),
        Provider::Gemini => parse_gemini_sse_block(event),
    }
}

/// Parse a single Gemini SSE event block. Gemini's
/// `streamGenerateContent?alt=sse` uses OpenAI-style `data: {json}`
/// events (no typed `event:` header), so we walk the block for the
/// `data:` line and hand it to the provider parser.
pub(crate) fn parse_gemini_sse_block(event: &str) -> SseParseResult {
    for line in event.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            return crate::gemini::parse_gemini_sse(data.trim());
        }
    }
    SseParseResult::Skip
}

/// Parse an Anthropic SSE event block which has `event:` and `data:` fields.
pub(crate) fn parse_anthropic_sse_block(event: &str) -> SseParseResult {
    let mut event_type: Option<&str> = None;
    let mut data: Option<&str> = None;

    for line in event.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(et) = line
            .strip_prefix("event: ")
            .or_else(|| line.strip_prefix("event:"))
        {
            event_type = Some(et.trim());
        }
        if let Some(d) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            data = Some(d.trim());
        }
    }

    match (event_type, data) {
        (Some(et), Some(d)) => crate::anthropic::parse_anthropic_sse(et, d),
        (Some("message_stop"), _) => SseParseResult::Done,
        _ => SseParseResult::Skip,
    }
}

/// Parse a single OpenAI SSE event block (one or more `data:` lines) into a StreamChunk.
pub(crate) fn parse_openai_sse(event: &str) -> SseParseResult {
    for line in event.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            let data = data.trim();
            if data == "[DONE]" {
                return SseParseResult::Done;
            }
            match serde_json::from_str::<StreamChunk>(data) {
                Ok(chunk) => return SseParseResult::Chunk(chunk),
                Err(e) => {
                    warn!("Failed to parse SSE chunk: {} - data: {}", e, data);
                    continue;
                }
            }
        }
    }
    SseParseResult::Skip
}
