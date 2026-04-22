//! SSE streaming response pipeline for the LLM client.
//!
//! Wraps a successful `reqwest::Response` in a `BoxStream<StreamChunk>` that
//! buffers raw bytes into complete SSE events, normalises CRLF to LF, and
//! enforces a per-chunk read timeout. Shared by the primary streaming path
//! in [`super::LlmClient::complete_stream`] and the Gemini cache-expired
//! retry path so the two code paths cannot diverge.

use super::{LlmClient, Provider, SseParseResult};
use crate::llm_types::StreamChunk;
use alms_core::{AlmsError, AlmsResult};
use tracing::warn;

/// Convert a successful HTTP response into a boxed stream of `StreamChunk`s.
///
/// Extracted so the cache-expired retry path on the Gemini streaming
/// endpoint (#769) can reuse the same SSE-buffering + per-chunk timeout
/// pipeline as the primary success path. Buffers raw bytes into complete
/// SSE events (events span TCP chunks), normalises CRLF to LF, and
/// enforces a per-chunk read timeout to prevent indefinite hangs when the
/// upstream stops sending data without closing the connection.
pub(crate) fn stream_response(
    response: reqwest::Response,
    provider: Provider,
    stream_chunk_timeout_secs: u64,
) -> futures::stream::BoxStream<'static, AlmsResult<StreamChunk>> {
    use futures::StreamExt;
    let byte_stream = response.bytes_stream();
    let chunk_timeout = std::time::Duration::from_secs(stream_chunk_timeout_secs);
    let stream = futures::stream::unfold(
        (byte_stream, String::new()),
        move |(mut bytes, mut buf)| async move {
            use futures::StreamExt as _;
            loop {
                // Try to extract a complete SSE event from the buffer
                if let Some(pos) = buf.find("\n\n") {
                    let event_text = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    match LlmClient::dispatch_sse_event(provider, &event_text) {
                        SseParseResult::Chunk(chunk) => {
                            return Some((Ok(chunk), (bytes, buf)));
                        }
                        SseParseResult::Done => {
                            return None; // [DONE] — stream complete
                        }
                        SseParseResult::Skip => {
                            continue; // comment or empty event
                        }
                    }
                }
                // Need more data from the network (with timeout)
                match tokio::time::timeout(chunk_timeout, bytes.next()).await {
                    Ok(Some(Ok(b))) => {
                        // Normalize \r\n → \n so the \n\n event separator works
                        // regardless of whether the upstream sends CRLF or LF.
                        let text = String::from_utf8_lossy(&b).replace("\r\n", "\n");
                        buf.push_str(&text);
                    }
                    Ok(Some(Err(e))) => {
                        return Some((
                            Err(AlmsError::Runtime(format!("Stream error: {}", e))),
                            (bytes, buf),
                        ));
                    }
                    Ok(None) => {
                        // Stream ended — try to parse any remaining buffered data
                        if !buf.trim().is_empty() {
                            let remaining = std::mem::take(&mut buf);
                            if let SseParseResult::Chunk(chunk) =
                                LlmClient::dispatch_sse_event(provider, remaining.trim())
                            {
                                return Some((Ok(chunk), (bytes, buf)));
                            }
                        }
                        return None; // stream complete
                    }
                    Err(_) => {
                        warn!(
                            "LLM stream stalled (no data for {}s), terminating",
                            chunk_timeout.as_secs()
                        );
                        return Some((
                            Err(AlmsError::Runtime(format!(
                                "LLM stream stalled (no data for {}s) — partial response discarded",
                                chunk_timeout.as_secs()
                            ))),
                            (bytes, buf),
                        ));
                    }
                }
            }
        },
    );
    stream.boxed()
}
