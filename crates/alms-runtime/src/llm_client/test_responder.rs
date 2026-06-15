//! Hand-rolled single-shot TCP responder used by `mod.rs` tests to pin
//! the cache-expired retry code paths in `complete()` / `complete_stream()`
//! without taking on `wiremock` as a dev-dependency.
//!
//! Extracted from `mod.rs` (#792) — previously inline next to the two
//! tests that use it. Keeping it in its own `#[cfg(test)]` submodule keeps
//! `mod.rs` focused on production logic and opens a cleaner home for any
//! future HTTP-fixture helpers. #795 will layer `wiremock`-backed tests on
//! top of this same surface; this responder remains the lightweight path
//! for tests that only need a couple of sequential canned responses.

#![cfg(test)]

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One canned HTTP response served by [`spawn_sequential_responder`].
///
/// Grew a `content_type` slot in response to Tim's review on #1064 — the
/// original `(u16, &'static str)` tuple shape always emitted
/// `Content-Type: application/json` regardless of body, which made the
/// `decode_diagnostic_openai_html_body_*` test mildly dishonest about
/// what an upstream HTML error page actually looks like on the wire.
///
/// The `From<(u16, &'static str)>` impl preserves the older two-tuple
/// shape with a JSON default so existing call sites compile unchanged;
/// tests that care about the response content-type use the explicit
/// `(status, content_type, body)` three-tuple form.
#[derive(Debug, Clone)]
pub(super) struct CannedResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: &'static str,
}

impl From<(u16, &'static str)> for CannedResponse {
    fn from((status, body): (u16, &'static str)) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }
}

impl From<(u16, &'static str, &'static str)> for CannedResponse {
    fn from((status, content_type, body): (u16, &'static str, &'static str)) -> Self {
        Self {
            status,
            content_type,
            body,
        }
    }
}

/// Single-shot HTTP responder: binds a port, accepts one connection,
/// reads the request line (enough to dispatch in-order), and writes
/// `responses[i]` for request `i`. Returns the `base_url` for the
/// `LlmClient`. The listener task exits once all `responses` have
/// been served.
///
/// Accepts anything convertible into [`CannedResponse`] so the older
/// `(status, body)` two-tuple call sites keep working unchanged while
/// new tests that need a non-JSON `Content-Type` can pass the explicit
/// three-tuple form. See [`CannedResponse`].
pub(super) async fn spawn_sequential_responder<I>(responses: I) -> String
where
    I: IntoIterator,
    I::Item: Into<CannedResponse>,
{
    let responses: Vec<CannedResponse> = responses.into_iter().map(Into::into).collect();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}/v1beta", addr);

    tokio::spawn(async move {
        for canned in responses {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            // Drain the request headers so the client doesn't block
            // waiting for us to consume the body. We read until the
            // buffer stops growing for one chunk — the client sends
            // everything in a single write for these tiny bodies.
            let mut buf = vec![0u8; 16 * 1024];
            let _ = sock.read(&mut buf).await;
            let reason = match canned.status {
                200 => "OK",
                400 => "Bad Request",
                404 => "Not Found",
                500 => "Internal Server Error",
                _ => "Status",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                status = canned.status,
                reason = reason,
                ctype = canned.content_type,
                len = canned.body.len(),
                body = canned.body,
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    base_url
}

/// Variant of [`spawn_sequential_responder`] that simulates a mid-stream
/// connection failure: writes HTTP headers advertising a `Content-Length`
/// strictly larger than the body length, writes the partial body, then
/// drops the socket without sending the remaining bytes. reqwest's
/// `bytes_stream()` detects the premature close and surfaces a
/// `BodyDecodeError` on the next poll — the exact failure mode the
/// streaming branch of #1044's enriched diagnostic is instrumenting for.
///
/// Used by `complete_stream_mid_stream_decode_failure_*` (#1064 review
/// follow-up) to drive `stream_response` through a real mid-stream
/// `Err(_)` on the `bytes_stream()` poll, end-to-end, so the
/// `provider`, `model`, and `bytes_read=N` bracket on the bubbled error
/// is pinned by an integration test rather than only by the formatter
/// unit test.
pub(super) async fn spawn_truncated_body_responder(
    status: u16,
    content_type: &'static str,
    actual_body: &'static str,
    claimed_len: usize,
) -> String {
    assert!(
        claimed_len > actual_body.len(),
        "claimed_len must exceed actual body length for reqwest to fault \
         (claimed={claimed_len} actual={})",
        actual_body.len()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}/v1beta", addr);

    tokio::spawn(async move {
        let (mut sock, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut buf = vec![0u8; 16 * 1024];
        let _ = sock.read(&mut buf).await;
        let reason = match status {
            200 => "OK",
            _ => "Status",
        };
        // Advertise more bytes than we plan to send, then write the
        // partial body and drop the connection. reqwest's body-stream
        // detection notices the premature EOF and surfaces a decode
        // error on the next `bytes_stream()` poll.
        //
        // The `\` at end-of-line below is a Rust string-literal line
        // continuation: it absorbs the newline _and_ the indented
        // whitespace on the next source line, so the rendered bytes
        // are `...Content-Type: <ct>\r\nContent-Length: <n>\r\n...`,
        // i.e. `Content-Length` is its own properly-terminated header
        // line (not a folded continuation of `Content-Type`). The
        // `debug_assert!` block below pins this shape so a future
        // refactor that breaks the byte layout fails loudly in tests
        // instead of silently degrading the fixture into a clean-EOF
        // case (which would let unrelated stream errors trip the
        // diagnostic and pass the caller test for the wrong reason).
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
             Content-Length: {claimed_len}\r\nConnection: close\r\n\r\n",
        );
        debug_assert!(
            headers.contains(&format!(
                "\r\nContent-Length: {claimed_len}\r\nConnection: close\r\n"
            )),
            "truncated-body responder header layout drifted — \
             Content-Length must be its own \\r\\n-terminated line, got: {headers:?}"
        );
        let _ = sock.write_all(headers.as_bytes()).await;
        let _ = sock.write_all(actual_body.as_bytes()).await;
        let _ = sock.flush().await;
        // Drop the socket without writing the rest of the advertised
        // body — the explicit `drop` is load-bearing here.
        drop(sock);
    });

    base_url
}

/// Variant of [`spawn_sequential_responder`] that simulates a **slow /
/// stalled** response body: writes the HTTP headers (advertising a
/// `Content-Length` larger than what it sends) plus an optional partial
/// body, then *holds the connection open without sending the rest* for
/// `stall_secs`. The body never finishes arriving within the per-chunk
/// inactivity window, so the body-read idle guard
/// (`read_body_with_idle_timeout` on the buffered path, `stream_response`
/// on the streaming path) faults the read promptly — rather than hanging
/// until the total request deadline.
///
/// This is the #1163 failure mode: `minimax/minimax-m3` on openrouter
/// returned a buffered `application/json` body that stalled mid-transfer,
/// and because the buffered `complete()` path had no inactivity guard at
/// all it hung for the full total `.timeout()` window before surfacing
/// "LLM response decode failed … operation timed out". Used to pin that a
/// stalled body now faults within `stream_chunk_timeout_secs`.
///
/// Note this stalls *mid-body* — headers are sent immediately. The
/// distinct slow-to-first-byte / header-wait path is covered separately by
/// [`spawn_slow_headers_responder`], which must NOT be clipped by the
/// per-chunk guard.
///
/// The task keeps the socket alive for `stall_secs` (don't make this
/// longer than the test's own timeout budget) so the client side is the
/// party that gives up first via its body-read idle timeout.
pub(super) async fn spawn_slow_body_responder(
    status: u16,
    content_type: &'static str,
    partial_body: &'static str,
    claimed_len: usize,
    stall_secs: u64,
) -> String {
    assert!(
        claimed_len > partial_body.len(),
        "claimed_len must exceed the partial body length so the body never \
         completes (claimed={claimed_len} partial={})",
        partial_body.len()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}/v1beta", addr);

    tokio::spawn(async move {
        let (mut sock, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut buf = vec![0u8; 16 * 1024];
        let _ = sock.read(&mut buf).await;
        let reason = match status {
            200 => "OK",
            _ => "Status",
        };
        // Advertise more bytes than we send, flush the headers + partial
        // body, then stall: keep the connection open without sending the
        // remaining advertised bytes. The client's read timeout (not the
        // total deadline) is what we want to fire first.
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
             Content-Length: {claimed_len}\r\nConnection: close\r\n\r\n",
        );
        let _ = sock.write_all(headers.as_bytes()).await;
        if !partial_body.is_empty() {
            let _ = sock.write_all(partial_body.as_bytes()).await;
        }
        let _ = sock.flush().await;
        // Hold the socket open, sending nothing further. The client side
        // gives up first via its per-read inactivity timeout.
        tokio::time::sleep(std::time::Duration::from_secs(stall_secs)).await;
        drop(sock);
    });

    base_url
}

/// Variant of [`spawn_sequential_responder`] that simulates a **slow but
/// healthy** upstream: accepts the connection, drains the request, then
/// sends *nothing at all* — no status line, no headers, no body — for
/// `delay_secs`, and only then writes a single complete, valid response.
///
/// This exercises the **time-to-first-byte / header-wait** path, which is a
/// distinct phase from the mid-body stall covered by
/// [`spawn_slow_body_responder`]: here the delay is entirely *before* any
/// response byte is emitted, so the client is blocked waiting for headers,
/// not for the rest of an already-started body.
///
/// It is the regression guard for #1163's Option-1 rework (Tim's review):
/// the inactivity guard must live on the body read only, so a healthy
/// response that is merely slow to *start* (delay under the total
/// `timeout_secs` but over the per-chunk `stream_chunk_timeout_secs`) must
/// **succeed**, not get clipped at the per-chunk window. The earlier
/// `.read_timeout(stream_chunk_timeout_secs)` mechanism also capped this
/// header wait and would fail such a call at the per-chunk window — this
/// responder pins that the body-only guard does not.
///
/// `delay_secs` must be shorter than the client's total `timeout_secs` (so
/// the healthy response still arrives in time) and longer than its
/// `stream_chunk_timeout_secs` (so the test actually distinguishes the two).
pub(super) async fn spawn_slow_headers_responder(
    status: u16,
    content_type: &'static str,
    body: &'static str,
    delay_secs: u64,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}/v1beta", addr);

    tokio::spawn(async move {
        let (mut sock, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut buf = vec![0u8; 16 * 1024];
        let _ = sock.read(&mut buf).await;
        // The whole point: send NOTHING for `delay_secs` — the client sits in
        // the connect+headers wait the entire time. This is time-to-first-
        // byte latency, not a mid-body stall.
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        let reason = match status {
            200 => "OK",
            _ => "Status",
        };
        // Now send one complete, well-formed response in a single write.
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
             Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            len = body.len(),
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
    });

    base_url
}
