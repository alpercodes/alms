// SPDX-License-Identifier: Apache-2.0

//! A scripted upstream LLM for tests, on `wiremock`.
//!
//! This is the fake for every test whose upstream sends **complete HTTP
//! responses** — possibly after a delay, possibly chosen by call order or
//! by request content, but always a status line, headers and a whole body.
//! That is what `wiremock` does, and it does it on a real HTTP server, so
//! keep-alive, chunking and content-length are hyper's problem rather than
//! a hand-rolled `TcpListener`'s.
//!
//! What it cannot do is misbehave *on the wire*: send headers and then go
//! quiet mid-body, lie about `Content-Length`, or hold a half-written
//! response open. Those cases are [`crate::raw_http::RawServer`].
//!
//! Keep the returned [`ScriptedLlm`] alive for the test — dropping it
//! shuts the server down.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// One complete canned HTTP response.
#[derive(Debug, Clone)]
pub struct Canned {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    /// Wait this long before sending anything — headers included. This is
    /// "the upstream is slow to answer", not "the body stalls".
    pub delay: Option<Duration>,
}

impl Canned {
    pub fn new(status: u16, content_type: &str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: content_type.to_string(),
            body: body.into(),
            delay: None,
        }
    }

    /// `application/json` with the given status.
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self::new(status, "application/json", body)
    }

    /// A `200 text/event-stream` body — one whole SSE stream, delivered at
    /// once.
    pub fn sse(body: impl Into<String>) -> Self {
        Self::new(200, "text/event-stream", body)
    }

    /// Send nothing for `delay`, then the response.
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    fn template(&self) -> ResponseTemplate {
        let mut template = ResponseTemplate::new(self.status)
            .set_body_raw(self.body.clone().into_bytes(), &self.content_type);
        if let Some(delay) = self.delay {
            template = template.set_delay(delay);
        }
        template
    }
}

/// Answers requests from a list, in order; once the list is exhausted the
/// last entry answers every further request, so "the same reply forever"
/// is a one-element list.
struct InOrder {
    responses: Vec<Canned>,
    next: AtomicUsize,
    calls: Arc<AtomicUsize>,
}

impl Respond for InOrder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses[n.min(self.responses.len() - 1)].template()
    }
}

/// Answers each request by inspecting it.
struct Routed {
    route: Box<dyn Fn(&Request) -> Canned + Send + Sync>,
    calls: Arc<AtomicUsize>,
}

impl Respond for Routed {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.route)(request).template()
    }
}

/// A running scripted upstream. Point an `LlmClient` at
/// [`base_url`](Self::base_url).
pub struct ScriptedLlm {
    server: MockServer,
    calls: Arc<AtomicUsize>,
}

impl ScriptedLlm {
    /// Answer in order, repeating the last response once the list is used
    /// up. Panics on an empty list.
    pub async fn in_order(responses: Vec<Canned>) -> Self {
        assert!(
            !responses.is_empty(),
            "a scripted LLM needs at least one response"
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let responder = InOrder {
            responses,
            next: AtomicUsize::new(0),
            calls: Arc::clone(&calls),
        };
        Self::mount(responder, calls).await
    }

    /// Answer every request with `response`.
    pub async fn always(response: Canned) -> Self {
        Self::in_order(vec![response]).await
    }

    /// Answer each request by inspecting it — for scripts where turns run
    /// concurrently and call order is not the discriminator.
    pub async fn routed(route: impl Fn(&Request) -> Canned + Send + Sync + 'static) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let responder = Routed {
            route: Box::new(route),
            calls: Arc::clone(&calls),
        };
        Self::mount(responder, calls).await
    }

    async fn mount(responder: impl Respond + 'static, calls: Arc<AtomicUsize>) -> Self {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(responder)
            .mount(&server)
            .await;
        Self { server, calls }
    }

    /// `http://127.0.0.1:<port>` — no path. Append whatever prefix the
    /// adapter under test expects (`/v1beta` for Gemini).
    pub fn base_url(&self) -> String {
        self.server.uri()
    }

    /// How many requests have been answered so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The bodies of every request received, in order.
    pub async fn request_bodies(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_order_answers_in_sequence_then_repeats_the_last() {
        let llm = ScriptedLlm::in_order(vec![Canned::json(200, "1"), Canned::json(500, "2")]).await;
        let client = reqwest::Client::new();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let resp = client
                .post(format!("{}/anything", llm.base_url()))
                .body("ping")
                .send()
                .await
                .unwrap();
            seen.push((resp.status().as_u16(), resp.text().await.unwrap()));
        }
        assert_eq!(
            seen,
            [
                (200, "1".to_string()),
                (500, "2".to_string()),
                (500, "2".to_string())
            ]
        );
        assert_eq!(llm.calls(), 3);
        assert_eq!(llm.request_bodies().await, vec!["ping"; 3]);
    }

    #[tokio::test]
    async fn routed_answers_by_request_content() {
        let llm = ScriptedLlm::routed(|req| {
            if req.body.starts_with(b"a") {
                Canned::json(200, "A")
            } else {
                Canned::json(200, "other")
            }
        })
        .await;
        let client = reqwest::Client::new();
        let a = client
            .post(llm.base_url())
            .body("a-request")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let b = client
            .post(llm.base_url())
            .body("b-request")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("A", "other"));
    }

    #[tokio::test]
    async fn after_delays_the_whole_response() {
        let llm =
            ScriptedLlm::always(Canned::sse("data: x\n\n").after(Duration::from_millis(300))).await;
        let started = std::time::Instant::now();
        let resp = reqwest::get(llm.base_url()).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(resp.text().await.unwrap(), "data: x\n\n");
    }
}
