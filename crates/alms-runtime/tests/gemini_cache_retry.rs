// SPDX-License-Identifier: Apache-2.0

//! End-to-end wiremock integration tests for the Gemini cache-expired
//! retry cycle (#795).
//!
//! Context: #769 introduced Gemini's `cachedContents` passthrough. The
//! adapter creates a cache on the first turn of a session, references it
//! on subsequent turns via `cachedContent: "cachedContents/<id>"`, and —
//! when Gemini returns a cache-not-found error (TTL expired, GC'd, or
//! deleted out of band) — transparently invalidates the stored handle
//! and retries the same `generateContent` / `streamGenerateContent`
//! request once without `cachedContent`.
//!
//! The retry decision lives in a pure function (`decide_cache_retry`)
//! with its own unit tests. The TCP-level retry assertion for
//! `complete()` / `complete_stream()` already lives in the llm_client
//! crate's mod tests, backed by a hand-rolled `spawn_sequential_responder`
//! loopback listener that returns bytes verbatim without parsing the
//! incoming request.
//!
//! What's still missing — and what this file adds — is a full end-to-end
//! check that actually hits a fake Gemini server through reqwest, lets
//! wiremock parse the request URIs + bodies, and verifies the entire
//! sequence of HTTP calls:
//!
//!   1. `POST /v1beta/cachedContents` (cache creation).
//!   2. `POST /v1beta/models/<model>:generateContent` with
//!      `cachedContent: "cachedContents/<id>"` in the body → Gemini
//!      returns a cache-not-found error.
//!   3. `POST /v1beta/models/<model>:generateContent` WITHOUT
//!      `cachedContent` → 200 with a normal completion body.
//!
//! That sequence exercises every seam in the retry contract:
//!
//!   - `GeminiCacheStore::ensure_cache` correctly POSTs to
//!     `cachedContents` when no handle is stored for the session.
//!   - `build_request` attaches the cache name to the first
//!     `generateContent` body.
//!   - `decide_cache_retry` correctly classifies the error body as a
//!     cache-not-found case.
//!   - `LlmClient::complete` / `complete_stream` re-dispatches the same
//!     request without `cachedContent` and parses the retry response.
//!
//! The streaming test also proves that the SSE state machine can
//! re-establish a fresh connection on retry — the same `body_matcher`
//! shape Gemini uses for `generateContent` is used verbatim for
//! `streamGenerateContent?alt=sse`.
//!
//! Determinism: wiremock pins request matching on URL path + method +
//! body-contains predicates. No real Gemini API is hit. The fake server
//! is bound to `127.0.0.1:0` (OS-assigned port) so tests can run in
//! parallel without port collisions.

use alms_runtime::{CompletionRequest, LlmClient, LlmConfig, LlmMessage};
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Canned successful `generateContent` body — one text candidate, one
/// STOP finish reason, and a plausible `usageMetadata`. Shared between
/// the non-streaming and streaming tests so any shape drift only has to
/// be updated in one place.
fn success_gemini_body() -> serde_json::Value {
    json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"text": "pong"}]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 1,
            "totalTokenCount": 11
        },
        "modelVersion": "gemini-2.5-pro-001"
    })
}

/// Canned cache-not-found error body. Shape matches what Gemini returns
/// when a referenced `cachedContents/<id>` has been GC'd or its TTL has
/// expired; `is_cache_not_found_error` classifies it as a retry-eligible
/// case because the body contains both `cachedContent` and `not_found`.
fn cache_not_found_body() -> serde_json::Value {
    json!({
        "error": {
            "code": 404,
            "status": "NOT_FOUND",
            "message": "CachedContent cachedContents/test-cache-123 was not found"
        }
    })
}

/// Build an `LlmClient` pointed at `mock_base_url` using the Gemini
/// native adapter, with a generous timeout so slow CI runners don't
/// race the wiremock handshake.
fn gemini_client_against(mock_base_url: &str) -> LlmClient {
    let mut core_cfg = alms_core::config::LlmConfig::default();
    core_cfg.ensure_builtin_providers();
    core_cfg.provider = "gemini".into();

    let mut runtime_cfg: LlmConfig = core_cfg.into();
    runtime_cfg.api_key = "test-api-key".into();
    // Wiremock returns its URI as `http://127.0.0.1:PORT` — append the
    // `/v1beta` prefix so our adapter's `{base_url}/models/...` and
    // `{base_url}/cachedContents` format strings land on the paths we
    // register on the mock below. Mirrors the real Gemini URL layout.
    runtime_cfg.base_url = format!("{mock_base_url}/v1beta");
    // Short timeout: a hang would waste CI minutes and mask wiring bugs
    // as generic "LLM API error" strings. 10 seconds is plenty for the
    // local loopback round-trips here.
    runtime_cfg.timeout_secs = 10;
    runtime_cfg.stream_chunk_timeout_secs = 10;

    LlmClient::new(runtime_cfg).expect("Gemini client must build")
}

/// Non-streaming end-to-end: cache creation → generateContent-with-cache
/// returns NOT_FOUND → generateContent-without-cache succeeds.
///
/// The test explicitly pins the request ordering by scoping each mock
/// to the exact body shape it should match: cache-reference calls match
/// on `body_string_contains("cachedContent")` while the retry matches on
/// the absence of that string (via a separate non-overlapping mock).
#[tokio::test]
async fn complete_cache_expired_retry_full_cycle() {
    let server = MockServer::start().await;

    // Stub 1: cache creation. Any POST to `/v1beta/cachedContents`
    // returns a stable cache name. The adapter doesn't re-create on
    // retry (retry goes without `cachedContent`), so a single
    // expectation with `expect(1)` is exactly right.
    Mock::given(method("POST"))
        .and(path("/v1beta/cachedContents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "cachedContents/test-cache-123"
        })))
        .expect(1)
        .named("cachedContents.create")
        .mount(&server)
        .await;

    // Stub 2: first `generateContent` carries `cachedContent` in the
    // body → respond with the cache-not-found error.
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-pro:generateContent"))
        .and(body_string_contains("cachedContent"))
        .respond_with(ResponseTemplate::new(404).set_body_json(cache_not_found_body()))
        .expect(1)
        .named("generateContent with cached reference -> 404")
        .mount(&server)
        .await;

    // Stub 3: retry `generateContent` WITHOUT `cachedContent` → 200 OK.
    //
    // We can't assert "body does NOT contain cachedContent" directly
    // from wiremock's builder surface, so we use a custom matcher that
    // fails if the body still carries the field. This makes the mock
    // exclusive with Stub 2 rather than relying on insertion-order
    // fallthrough.
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-pro:generateContent"))
        .and(body_does_not_contain_cached_content())
        .respond_with(ResponseTemplate::new(200).set_body_json(success_gemini_body()))
        .expect(1)
        .named("generateContent without cached reference -> 200")
        .mount(&server)
        .await;

    let client = gemini_client_against(&server.uri());

    let session_id = alms_core::SessionId::new();
    let request = CompletionRequest::new("gemini-2.5-pro")
        .with_messages(vec![LlmMessage::user("ping")])
        .with_session_id(session_id)
        .with_gemini_cache_enabled(true)
        .with_gemini_cache_ttl(300);

    let response = client
        .complete(request)
        .await
        .expect("cache-expired retry must succeed end-to-end");

    // Shape check: the retry response body flowed through the normal
    // success path and parsed into a CompletionResponse with the
    // expected content text. If the retry code accidentally fed the 404
    // body into the success-path parser, this would fail with a
    // JSON-parse error upstream.
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.content_str(), "pong");

    // Exhaustiveness: all three mocks got exactly one hit. wiremock
    // verifies this automatically on drop when `.expect(n)` is set, so
    // the assert above plus the implicit drop-verification covers the
    // full sequence of: cache create -> ref-carrying call -> retry.
    //
    // Belt-and-braces: also drain the server's received requests log to
    // fail loudly if the ordering turned out to be out of sequence
    // (e.g. retry fired before the error) — wiremock's default is
    // insertion-order matching, but being explicit here guards against
    // a future re-ordering bug.
    let received = server.received_requests().await.expect("request log");
    assert_eq!(
        received.len(),
        3,
        "expected 3 HTTP calls (cache create, ref-carrying generateContent, retry without ref), got {}",
        received.len()
    );
    assert!(
        received[0].url.path().ends_with("/cachedContents"),
        "first call must be cache creation, got path: {}",
        received[0].url.path()
    );
    assert!(
        received[1].url.path().ends_with(":generateContent"),
        "second call must be generateContent, got path: {}",
        received[1].url.path()
    );
    let first_gc_body = std::str::from_utf8(&received[1].body).unwrap_or("");
    assert!(
        first_gc_body.contains("cachedContent"),
        "first generateContent call must carry cachedContent reference, got body: {first_gc_body}"
    );
    let retry_body = std::str::from_utf8(&received[2].body).unwrap_or("");
    assert!(
        !retry_body.contains("cachedContent"),
        "retry must NOT carry cachedContent reference, got body: {retry_body}"
    );
}

/// Streaming end-to-end counterpart. Same three-call shape as the
/// non-streaming test, except the success response is an SSE stream
/// (content-type `text/event-stream`, `data: {json}\n\n` frames) and we
/// assert on the chunks the consumer iterates off the `BoxStream`.
///
/// This proves the streaming state machine correctly re-establishes the
/// SSE connection on retry — the common regression in #787 / #795 was
/// that the error-body code path reused the same HTTP response object
/// without re-issuing the request.
#[tokio::test]
async fn complete_stream_cache_expired_retry_full_cycle() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1beta/cachedContents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "cachedContents/test-cache-stream"
        })))
        .expect(1)
        .named("cachedContents.create (stream test)")
        .mount(&server)
        .await;

    // First streaming call: body carries `cachedContent` → error.
    //
    // We use `path_regex` with the `?alt=sse` query suffix swallowed
    // because reqwest surfaces query params separately from the URL path
    // that wiremock matches on. The important thing is that the adapter
    // hits `:streamGenerateContent` (not `:generateContent`).
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/v1beta/models/gemini-2\.5-pro:streamGenerateContent$",
        ))
        .and(body_string_contains("cachedContent"))
        .respond_with(ResponseTemplate::new(404).set_body_json(cache_not_found_body()))
        .expect(1)
        .named("streamGenerateContent with cached reference -> 404")
        .mount(&server)
        .await;

    // Retry streaming call WITHOUT `cachedContent` → SSE stream.
    // Shape: one `data: {json}\n\n` frame with a single text part and
    // STOP finish. Terminates when the TCP connection closes (no [DONE]
    // sentinel needed — the Gemini parser tolerates both paths).
    let sse_frame = format!(
        "data: {}\n\n",
        serde_json::to_string(&success_gemini_body()).unwrap()
    );
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/v1beta/models/gemini-2\.5-pro:streamGenerateContent$",
        ))
        .and(body_does_not_contain_cached_content())
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_frame),
        )
        .expect(1)
        .named("streamGenerateContent without cached reference -> 200 SSE")
        .mount(&server)
        .await;

    let client = gemini_client_against(&server.uri());

    let session_id = alms_core::SessionId::new();
    let request = CompletionRequest::new("gemini-2.5-pro")
        .with_messages(vec![LlmMessage::user("ping")])
        .with_session_id(session_id)
        .with_gemini_cache_enabled(true)
        .with_gemini_cache_ttl(300);

    let mut stream = client
        .complete_stream(request)
        .await
        .expect("cache-expired retry must succeed for streaming path");

    let mut content = String::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.expect("stream chunk must not error");
        if let Some(choice) = chunk.choices.into_iter().next()
            && let Some(delta) = choice.delta.content
        {
            content.push_str(&delta);
        }
    }

    // The retry stream carried a single visible text part. If the retry
    // mistakenly received the 404 error body instead of the SSE frame,
    // the stream would produce zero content chunks (and likely error).
    assert_eq!(
        content, "pong",
        "stream content must match the retry SSE frame"
    );

    let received = server.received_requests().await.expect("request log");
    assert_eq!(
        received.len(),
        3,
        "expected 3 HTTP calls for the streaming retry cycle, got {}",
        received.len()
    );
    assert!(
        received[0].url.path().ends_with("/cachedContents"),
        "first call must be cache creation"
    );
    assert!(
        received[1].url.path().ends_with(":streamGenerateContent"),
        "second call must be streamGenerateContent (with cache ref), got path: {}",
        received[1].url.path()
    );
    assert!(
        received[2].url.path().ends_with(":streamGenerateContent"),
        "third call must be streamGenerateContent (retry, without cache ref), got path: {}",
        received[2].url.path()
    );
    let retry_body = std::str::from_utf8(&received[2].body).unwrap_or("");
    assert!(
        !retry_body.contains("cachedContent"),
        "streaming retry must NOT carry cachedContent reference, got body: {retry_body}"
    );
}

/// Helper: wiremock match that asserts the request body does NOT
/// contain the substring `cachedContent`. Wiremock's standard matcher
/// set offers only positive `body_string_contains`; this wraps that to
/// invert the predicate so our retry-stub doesn't accidentally catch
/// the cache-carrying call too.
///
/// Implemented as a tiny `MatchExactly` impl via the low-level
/// `Match` trait rather than a closure so it reads the same as the
/// other matchers in the mock chain.
fn body_does_not_contain_cached_content() -> NoCachedContentMatcher {
    NoCachedContentMatcher
}

struct NoCachedContentMatcher;

impl wiremock::Match for NoCachedContentMatcher {
    fn matches(&self, req: &Request) -> bool {
        match std::str::from_utf8(&req.body) {
            Ok(s) => !s.contains("cachedContent"),
            // Non-UTF8 body is impossible in this test (we only POST
            // serde_json-rendered bodies), but fail closed rather than
            // match accidentally if the stream ever carries raw bytes.
            Err(_) => false,
        }
    }
}
