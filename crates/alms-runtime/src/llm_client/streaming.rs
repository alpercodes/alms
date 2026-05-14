//! SSE streaming response pipeline for the LLM client.
//!
//! Wraps a successful `reqwest::Response` in a `BoxStream<StreamChunk>` that
//! buffers raw bytes into complete SSE events, normalises CRLF to LF, and
//! enforces a per-chunk read timeout. Shared by the primary streaming path
//! in [`super::LlmClient::complete_stream`] and the Gemini cache-expired
//! retry path so the two code paths cannot diverge.

use super::diagnostic::{DecodeDiagnostic, flatten_error_chain, format_decode_error};
use super::sse_parsers::dispatch_sse_event;
use super::{Provider, SseParseResult};
use crate::llm_types::StreamChunk;
use alms_core::{AlmsError, AlmsResult};
use tracing::{error, warn};

/// Convert a successful HTTP response into a boxed stream of `StreamChunk`s.
///
/// Extracted so the cache-expired retry path on the Gemini streaming
/// endpoint (#769) can reuse the same SSE-buffering + per-chunk timeout
/// pipeline as the primary success path. Buffers raw bytes into complete
/// SSE events (events span TCP chunks), normalises CRLF to LF, and
/// enforces a per-chunk read timeout to prevent indefinite hangs when the
/// upstream stops sending data without closing the connection.
///
/// `provider_name` and `model` are baked into the error path so that a
/// mid-stream body-read failure (connection reset, malformed chunked
/// transfer, gzip decode failure, etc.) surfaces a structured diagnostic
/// with the same shape as the non-streaming decode/parse failures —
/// `provider`, `model`, `bytes_read`, and the full underlying error
/// chain. See [`super::diagnostic`] and #1044.
pub(crate) fn stream_response(
    response: reqwest::Response,
    provider: Provider,
    stream_chunk_timeout_secs: u64,
    provider_name: String,
    model: String,
) -> futures::stream::BoxStream<'static, AlmsResult<StreamChunk>> {
    use futures::StreamExt;
    let byte_stream = response.bytes_stream();
    let chunk_timeout = std::time::Duration::from_secs(stream_chunk_timeout_secs);
    // Move tracking state into the unfold closure: `total_bytes_read`
    // accumulates raw bytes pulled from the upstream so the mid-stream
    // decode-failure path can report how far we got before the connection
    // gave up. Carried through the closure's accumulator alongside the
    // existing buffer.
    let stream = futures::stream::unfold(
        (byte_stream, String::new(), 0usize, provider_name, model),
        move |(mut bytes, mut buf, mut total_bytes_read, provider_name, model)| async move {
            use futures::StreamExt as _;
            loop {
                // Try to extract a complete SSE event from the buffer
                if let Some(pos) = buf.find("\n\n") {
                    let event_text = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    match dispatch_sse_event(provider, &event_text) {
                        SseParseResult::Chunk(chunk) => {
                            return Some((
                                Ok(chunk),
                                (bytes, buf, total_bytes_read, provider_name, model),
                            ));
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
                        total_bytes_read = total_bytes_read.saturating_add(b.len());
                        // Normalize \r\n → \n so the \n\n event separator works
                        // regardless of whether the upstream sends CRLF or LF.
                        let text = String::from_utf8_lossy(&b).replace("\r\n", "\n");
                        buf.push_str(&text);
                    }
                    Ok(Some(Err(e))) => {
                        // Mid-stream body-read failure. reqwest's bare
                        // `Display` collapses this to "error decoding
                        // response body" with no provider/model/byte-count
                        // context — walk the source chain and bake the
                        // structured shape into the bubbled error so the
                        // operator (and the parent agent's tool-call
                        // result, via the coordinator forwarding the
                        // error string verbatim) can tell connection-reset
                        // from gzip-decode-failure from H2-stream-reset.
                        // See #1044.
                        //
                        // Tim's review on #1064 (P2): include the
                        // partially-accumulated SSE buffer in the
                        // diagnostic. If the upstream got some chunks in
                        // before connection-resetting, the prefix tells
                        // an operator exactly where on the SSE event
                        // boundary the failure landed. The formatter's
                        // 512-byte cap + control-byte escape pipeline
                        // bounds the size and keeps log framing safe.
                        let chain = flatten_error_chain(&e);
                        let body_prefix = if buf.is_empty() {
                            None
                        } else {
                            Some(buf.as_str())
                        };
                        let diag = DecodeDiagnostic {
                            provider: provider_name.as_str(),
                            model: model.as_str(),
                            bytes_read: Some(total_bytes_read),
                            body_prefix,
                            ..Default::default()
                        };
                        let msg = format_decode_error("LLM stream decode failed", &diag, &chain);
                        error!("{msg}");
                        return Some((
                            Err(AlmsError::Runtime(msg)),
                            (bytes, buf, total_bytes_read, provider_name, model),
                        ));
                    }
                    Ok(None) => {
                        // Stream ended — try to parse any remaining buffered data
                        if !buf.trim().is_empty() {
                            let remaining = std::mem::take(&mut buf);
                            if let SseParseResult::Chunk(chunk) =
                                dispatch_sse_event(provider, remaining.trim())
                            {
                                return Some((
                                    Ok(chunk),
                                    (bytes, buf, total_bytes_read, provider_name, model),
                                ));
                            }
                        }
                        return None; // stream complete
                    }
                    Err(_) => {
                        warn!(
                            provider = provider_name.as_str(),
                            model = model.as_str(),
                            bytes_read = total_bytes_read,
                            "LLM stream stalled (no data for {}s), terminating",
                            chunk_timeout.as_secs()
                        );
                        return Some((
                            Err(AlmsError::Runtime(format!(
                                "LLM stream stalled [provider={} model={} bytes_read={}] \
                                 (no data for {}s) — partial response discarded",
                                provider_name,
                                model,
                                total_bytes_read,
                                chunk_timeout.as_secs()
                            ))),
                            (bytes, buf, total_bytes_read, provider_name, model),
                        ));
                    }
                }
            }
        },
    );
    stream.boxed()
}
