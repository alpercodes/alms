// SPDX-License-Identifier: Apache-2.0

//! Helpers for the hand-rolled loopback HTTP servers the scripted-LLM
//! tests stand up with a bare `TcpListener`.

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

/// Read one complete HTTP request (headers + `Content-Length` body) off the
/// socket, so a scripted LLM server consumes the agent's request before
/// responding.
///
/// Reads until the header terminator has been seen and at least
/// `Content-Length` further bytes have arrived (or the peer closes). A
/// request with no `Content-Length` is complete at the header terminator.
pub async fn read_full_http_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text[..header_end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
