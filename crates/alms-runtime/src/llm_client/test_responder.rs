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
