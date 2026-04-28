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

/// Single-shot HTTP responder: binds a port, accepts one connection,
/// reads the request line (enough to dispatch in-order), and writes
/// `responses[i]` for request `i`. Returns the `base_url` for the
/// `LlmClient`. The listener task exits once all `responses` have
/// been served.
pub(super) async fn spawn_sequential_responder(responses: Vec<(u16, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}/v1beta", addr);

    tokio::spawn(async move {
        for (status, body) in responses {
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
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                404 => "Not Found",
                500 => "Internal Server Error",
                _ => "Status",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    base_url
}
