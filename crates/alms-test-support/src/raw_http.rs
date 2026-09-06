// SPDX-License-Identifier: Apache-2.0

//! A raw-socket HTTP server for tests whose upstream misbehaves **on the
//! wire**: sends headers and then goes quiet mid-body, advertises a
//! `Content-Length` it never delivers, or holds a half-written response
//! open until the client gives up.
//!
//! `wiremock` — [`crate::llm_server`] — is the fake for everything else.
//! It runs a real HTTP server, and a real HTTP server will not lie about
//! its content length or stall inside a body for you; this one will,
//! because a [`WireScript`] is a list of the exact things to do to the
//! socket. Reach for it only when the test is *about* the wire.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::http::read_full_http_request;

/// One step of a [`WireScript`].
#[derive(Debug, Clone)]
pub enum Wire {
    /// Write the status line, `Content-Type`, `Content-Length` (when
    /// given), `Connection: close`, and the blank line. Nothing stops a
    /// script from advertising a length the body never reaches — that is
    /// the point.
    Headers {
        status: u16,
        content_type: &'static str,
        content_length: Option<usize>,
    },
    /// Write these bytes and flush.
    Body(String),
    /// Go quiet for this long with the connection open.
    Sleep(Duration),
    /// Hold the connection open until the peer goes away. Never returns;
    /// nothing after it in the script runs.
    Hold,
}

/// What to do to one connection, in order. When the script ends the
/// socket is shut down and dropped.
pub type WireScript = Vec<Wire>;

/// A running raw server. Point an `LlmClient` at
/// [`base_url`](Self::base_url). The accept loop stops when this is
/// dropped; connections already being served run their script out.
///
/// So it must outlive every connection the test opens. Binding it to a
/// bare `_` drops it on that line and the client meets connection-refused
/// instead of the script — and a test about a stall or a truncated body,
/// which expects an error, can pass on that wrong one. Bind `_llm`.
pub struct RawServer {
    base_url: String,
    calls: Arc<AtomicUsize>,
    accept_loop: JoinHandle<()>,
}

impl RawServer {
    /// Serve `scripts` one per accepted connection, in accept order;
    /// connections past the end of the list replay the last script. Every
    /// connection first reads the whole request, so the client is never
    /// blocked on an unconsumed body. Panics on an empty list.
    pub async fn serve(scripts: Vec<WireScript>) -> Self {
        assert!(
            !scripts.is_empty(),
            "a raw server needs at least one script"
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let scripts = Arc::new(scripts);

        let counter = Arc::clone(&calls);
        let accept_loop = tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let scripts = Arc::clone(&scripts);
                tokio::spawn(async move {
                    let _ = read_full_http_request(&mut sock).await;
                    let script = &scripts[n.min(scripts.len() - 1)];
                    run_script(&mut sock, script).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        Self {
            base_url,
            calls,
            accept_loop,
        }
    }

    /// `http://127.0.0.1:<port>` — no path.
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// How many connections have been accepted so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

async fn run_script(sock: &mut TcpStream, script: &[Wire]) {
    for step in script {
        match step {
            Wire::Headers {
                status,
                content_type,
                content_length,
            } => {
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    404 => "Not Found",
                    500 => "Internal Server Error",
                    _ => "Status",
                };
                let mut headers =
                    format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n");
                if let Some(len) = content_length {
                    headers.push_str(&format!("Content-Length: {len}\r\n"));
                }
                headers.push_str("Connection: close\r\n\r\n");
                let _ = sock.write_all(headers.as_bytes()).await;
                let _ = sock.flush().await;
            }
            Wire::Body(bytes) => {
                let _ = sock.write_all(bytes.as_bytes()).await;
                let _ = sock.flush().await;
            }
            Wire::Sleep(duration) => tokio::time::sleep(*duration).await,
            Wire::Hold => std::future::pending::<()>().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script that advertises more body than it sends and then closes
    /// faults the client mid-body — the wire pathology this exists for.
    #[tokio::test]
    async fn short_body_against_advertised_length_faults_the_client() {
        let server = RawServer::serve(vec![vec![
            Wire::Headers {
                status: 200,
                content_type: "application/json",
                content_length: Some(4096),
            },
            Wire::Body("{\"partial\":".into()),
        ]])
        .await;
        let err = reqwest::get(server.base_url())
            .await
            .unwrap()
            .text()
            .await
            .expect_err("a body shorter than Content-Length must fault the read");
        assert!(err.is_body() || err.is_decode(), "{err}");
        assert_eq!(server.calls(), 1);
    }

    /// Scripts are consumed in accept order and the last one repeats.
    #[tokio::test]
    async fn scripts_run_in_accept_order_and_the_last_repeats() {
        let ok = |body: &str| {
            vec![
                Wire::Headers {
                    status: 200,
                    content_type: "text/plain",
                    content_length: Some(body.len()),
                },
                Wire::Body(body.into()),
            ]
        };
        let server = RawServer::serve(vec![ok("first"), ok("second")]).await;
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(
                reqwest::get(server.base_url())
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(seen, ["first", "second", "second"]);
        assert_eq!(server.calls(), 3);
    }
}
