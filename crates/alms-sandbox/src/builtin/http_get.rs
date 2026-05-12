use crate::error::SandboxResult;
use crate::{SandboxError, Tool};
use alms_core::truncate_to_char_boundary;
use serde_json::Value;
use std::collections::BTreeMap;

/// Absolute ceiling on the response body `http_get` will read into memory
/// before bailing out (issue #851).
///
/// This is *not* the agent-visible cap — that comes from the in-loop
/// truncation service in `alms-runtime` which trims to ~32 KB. The 5 MB
/// ceiling is purely a defense against `http_get` pulling a 1 GB download
/// and OOM'ing the daemon before the truncation service has a chance to
/// act. Pulled bytes never leave this function — the truncation service
/// strips them down to a bounded preview before they touch any agent
/// context.
pub const MAX_RESPONSE_BYTES: u64 = 5 * 1024 * 1024;

/// Per-header-value byte ceiling used when surfacing response headers
/// (issue #956). Values over this length are truncated with a trailing
/// `…` marker so a hostile server can't blow the tool result size by
/// stuffing an 8 MB `Set-Cookie`. 8 KB is comfortably above any legitimate
/// header (browsers cap individual cookies around 4 KB) yet small enough
/// that dozens of these still fit inside the per-tool truncation budget.
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

/// Collect a reqwest `HeaderMap` into a deterministic, JSON-safe shape
/// for the tool result (issue #956).
///
/// Decisions:
/// - Returns a `BTreeMap<String, Vec<String>>` so cross-run output diffs
///   are stable (keys sorted alphabetically) and so multi-value headers
///   (`Set-Cookie`, `Vary`) keep their structure rather than getting
///   joined into a single comma-separated string.
/// - Keys are lowercased via `HeaderName::as_str()` so the UI does not
///   need to do case-insensitive matching to find `content-type` vs
///   `Content-Type`.
/// - Non-UTF-8 header values are rendered with `from_utf8_lossy` so a
///   garbage byte cannot crash the tool — the surrounding ASCII (which
///   is almost all of HTTP) still reads cleanly.
/// - Values exceeding `MAX_HEADER_VALUE_BYTES` are truncated with a
///   trailing `…` marker so a hostile server cannot blow up tool
///   output size.
fn collect_response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_string();
        let mut text = String::from_utf8_lossy(value.as_bytes()).into_owned();
        if text.len() > MAX_HEADER_VALUE_BYTES {
            // `String::truncate` panics if the byte offset isn't on a UTF-8
            // character boundary, and `text` comes from `from_utf8_lossy`
            // so a hostile server stuffing multibyte characters (or U+FFFD
            // replacement characters from invalid bytes) would have landed
            // byte 8192 mid-codepoint and crashed the tool. Floor to the
            // nearest char boundary at or below the cap via the shared
            // `alms_core::truncate_to_char_boundary` helper.
            let end = truncate_to_char_boundary(&text, MAX_HEADER_VALUE_BYTES).len();
            text.truncate(end);
            text.push('…');
        }
        map.entry(key).or_default().push(text);
    }
    map
}

/// HTTP GET tool - performs HTTP GET requests
#[derive(Debug, Clone)]
pub struct HttpGetTool {
    client: reqwest::Client,
}

impl HttpGetTool {
    /// Create a new HTTP GET tool
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("ALMS/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }
}

impl Default for HttpGetTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }

    fn description(&self) -> &str {
        "Performs an HTTP GET request to a URL and returns the response body"
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to send a GET request to"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("Missing 'url' field".to_string()))?;

        // Parse headers if provided
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(hdrs) = params.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in hdrs {
                if let Some(val_str) = value.as_str()
                    && let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    && let Ok(header_value) = reqwest::header::HeaderValue::from_str(val_str)
                {
                    headers.insert(header_name, header_value);
                }
            }
        }

        // Build request
        let mut request = self.client.get(url);

        // Add headers
        if !headers.is_empty() {
            request = request.headers(headers);
        }

        // Execute request
        let response = request.send().await.map_err(SandboxError::from)?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Capture the full response header map for the tool result
        // (issue #956). `content_type` above is kept as a convenience
        // extraction the UI already uses for the body label — the new
        // `headers` field is purely additive.
        let response_headers = collect_response_headers(response.headers());

        // Bail early when the server's `Content-Length` already exceeds
        // the pre-fetch ceiling. This avoids streaming gigabytes off the
        // wire just to reject them (issue #851).
        if let Some(len) = response.content_length()
            && len > MAX_RESPONSE_BYTES
        {
            return Err(SandboxError::InvalidParameters(format!(
                "Response body declared {len} bytes, exceeds {MAX_RESPONSE_BYTES} byte cap. \
                 http_get is intended for small responses; use the agent's tools to fetch \
                 large payloads to disk via the shell tool (e.g. `curl -o file.bin <url>`)."
            )));
        }

        // Stream the body chunk-by-chunk so we can stop reading once we
        // pass the pre-fetch ceiling. `response.bytes_stream()` decodes
        // transfer-encoding (e.g. chunked) for us; the cap is checked
        // against accumulated decoded bytes.
        use futures::StreamExt;
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(SandboxError::from)?;
            if buf.len().saturating_add(chunk.len()) as u64 > MAX_RESPONSE_BYTES {
                return Err(SandboxError::InvalidParameters(format!(
                    "Response body exceeded {MAX_RESPONSE_BYTES} byte cap mid-stream. \
                     http_get is intended for small responses; use the shell tool to \
                     fetch large payloads to disk."
                )));
            }
            buf.extend_from_slice(&chunk);
        }

        // Decode as UTF-8 (lossy) so binary-ish bodies don't crash the tool.
        // Truncation in the LLM-visible context is handled by the in-loop
        // truncation service in `alms-runtime`; this tool is responsible
        // only for the *pre-fetch* ceiling.
        let body_text = String::from_utf8_lossy(&buf).into_owned();

        // Try to parse as JSON, fallback to string
        let body = if let Ok(json) = serde_json::from_str::<Value>(&body_text) {
            json
        } else {
            Value::String(body_text)
        };

        // Build response object. `headers` is additive — pre-#956
        // callers only consumed `status` / `content_type` / `body` and
        // they're all still here in the same shape.
        let result = serde_json::json!({
            "status": status,
            "content_type": content_type,
            "headers": response_headers,
            "body": body,
        });

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_http_get_invalid_url() {
        let tool = HttpGetTool::new();

        // Missing URL
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn pre_fetch_ceiling_is_5mb() {
        // Sanity check: this is a load-bearing constant referenced from
        // the #851 design. If a future change shrinks it without updating
        // documentation, this test will catch the drift.
        assert_eq!(MAX_RESPONSE_BYTES, 5 * 1024 * 1024);
    }

    /// #956 — `http_get` must surface the full response header map in the
    /// tool result, with stable ordering, lowercased keys, and multi-value
    /// headers preserved as a `Vec<String>` rather than collapsed.
    #[tokio::test]
    async fn surfaces_response_headers_multi_value_lowercased_sorted() {
        let server = MockServer::start().await;
        // Note: `set_body_json` is what pins the Content-Type header to
        // `application/json` — using `set_body_string` together with
        // `.insert_header("Content-Type", ...)` doesn't work because
        // wiremock's body-string helper sets `text/plain` *after* the
        // insert and wins. We pin content-type via the body helper so the
        // backward-compat assertion below is meaningful.
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"abc123\"")
                    .insert_header("Cache-Control", "max-age=60")
                    .append_header("Set-Cookie", "a=1; Path=/")
                    .append_header("Set-Cookie", "b=2; Path=/")
                    .set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        let tool = HttpGetTool::new();
        let result = tool
            .execute(serde_json::json!({ "url": server.uri() }))
            .await
            .expect("http_get should succeed against mock server");

        // Backward-compatible fields.
        assert_eq!(result["status"], 200);
        assert_eq!(result["content_type"], "application/json");
        assert_eq!(result["body"]["ok"], true);

        // New `headers` field — must exist as an object.
        let headers = result["headers"]
            .as_object()
            .expect("headers field must be a JSON object");

        // Lowercased keys.
        assert!(
            headers.contains_key("content-type"),
            "content-type must be present (lowercased): {headers:?}"
        );
        assert!(
            headers.contains_key("etag"),
            "etag must be present (lowercased): {headers:?}"
        );
        assert!(
            headers.contains_key("cache-control"),
            "cache-control must be present: {headers:?}"
        );

        // Multi-value `Set-Cookie` preserved as a 2-element array.
        let cookies = headers["set-cookie"]
            .as_array()
            .expect("set-cookie must be an array");
        assert_eq!(cookies.len(), 2, "two Set-Cookie values expected");
        let cookie_strings: Vec<&str> = cookies.iter().filter_map(|v| v.as_str()).collect();
        assert!(cookie_strings.contains(&"a=1; Path=/"));
        assert!(cookie_strings.contains(&"b=2; Path=/"));

        // Single-value headers are still `[string]`, not bare strings —
        // the renderer is allowed to assume `Vec<String>` for every key.
        let etag = headers["etag"]
            .as_array()
            .expect("etag must be an array even when single-valued");
        assert_eq!(etag.len(), 1);
        assert_eq!(etag[0], "\"abc123\"");

        // BTreeMap → keys sorted alphabetically in serde_json::Value
        // (Map preserves insertion order, BTreeMap iterates sorted).
        let keys: Vec<&String> = headers.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            keys, sorted,
            "headers map must be alphabetically sorted: {keys:?}"
        );
    }

    /// #956 — values longer than `MAX_HEADER_VALUE_BYTES` are truncated
    /// with a trailing `…` so a hostile server can't blow the result size.
    #[tokio::test]
    async fn truncates_oversize_header_values() {
        let server = MockServer::start().await;
        // 16 KB string — well above the 8 KB per-value cap.
        let big = "x".repeat(16 * 1024);
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-Big-Header", big.as_str())
                    .set_body_string("ok"),
            )
            .mount(&server)
            .await;

        let tool = HttpGetTool::new();
        let result = tool
            .execute(serde_json::json!({ "url": server.uri() }))
            .await
            .expect("http_get should succeed");

        let headers = result["headers"].as_object().expect("headers object");
        let big_arr = headers["x-big-header"].as_array().expect("array");
        let big_val = big_arr[0].as_str().expect("string");
        // Truncated to the cap + the `…` marker (3 bytes in UTF-8).
        assert!(
            big_val.ends_with('…'),
            "oversize header must end with the truncation marker"
        );
        assert!(
            big_val.len() <= MAX_HEADER_VALUE_BYTES + 4,
            "truncated value must be at most {} + marker bytes, got {}",
            MAX_HEADER_VALUE_BYTES,
            big_val.len()
        );
    }

    /// #956 / Tim re-review on #1035 — wiremock sibling of
    /// `truncates_oversize_header_values` that pins the end-to-end multibyte
    /// path through `HttpGetTool::execute`. The pure-function test below
    /// covers the panic-free walk-back at the `collect_response_headers`
    /// boundary; this one rounds it out by confirming the full reqwest →
    /// `HeaderMap` → `collect_response_headers` → tool-result pipeline does
    /// not panic on a server that responds with a 16 KB multibyte header
    /// value.
    #[tokio::test]
    async fn truncates_oversize_multibyte_header_values() {
        let server = MockServer::start().await;
        // 4097 × `日` = 12,291 bytes — well above the 8 KB per-value cap
        // and crucially crosses the 8192-byte threshold mid-codepoint
        // (8192 / 3 ≈ 2730.67), so the walk-back is exercised in earnest.
        let big_jp = "日".repeat(4097);
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-Multibyte-Header", big_jp.as_str())
                    .set_body_string("ok"),
            )
            .mount(&server)
            .await;

        let tool = HttpGetTool::new();
        let result = tool
            .execute(serde_json::json!({ "url": server.uri() }))
            .await
            .expect("http_get must not panic on multibyte response headers");

        let headers = result["headers"].as_object().expect("headers object");
        let arr = headers["x-multibyte-header"].as_array().expect("array");
        let value = arr[0].as_str().expect("string");
        assert!(
            value.ends_with('…'),
            "oversize multibyte header must end with the truncation marker"
        );
        assert!(
            value.len() <= MAX_HEADER_VALUE_BYTES + '…'.len_utf8(),
            "truncated value must be at most {} + marker bytes, got {}",
            MAX_HEADER_VALUE_BYTES,
            value.len()
        );
        // Walk-back must not have left a dangling mojibake byte —
        // the prefix before the marker must be valid `日` runs only.
        let prefix = value.trim_end_matches('…');
        assert!(
            prefix.chars().all(|c| c == '日'),
            "truncated prefix must contain only whole `日` codepoints"
        );
    }

    /// #956 / Codex P1 on #1035 — values from `String::from_utf8_lossy`
    /// can carry multibyte characters, so the per-value truncate cannot
    /// be a bare `String::truncate(MAX_HEADER_VALUE_BYTES)` — that panics
    /// when byte 8192 lands mid-codepoint. With 4097 × `日` (3 bytes each
    /// = 12,291 bytes), byte 8192 falls at codepoint index 2730.66, which
    /// is mid-character; a hostile server can trigger this remotely.
    ///
    /// This test pins the panic-free path: the truncation walks back to
    /// the nearest UTF-8 char boundary at or below the cap, appends the
    /// `…` marker, and returns a well-formed string.
    #[test]
    fn collect_response_headers_truncates_multibyte_without_panic() {
        let mut hm = reqwest::header::HeaderMap::new();
        let big_jp = "日".repeat(4097); // 12_291 bytes, well past the 8 KB cap
        hm.insert(
            reqwest::header::HeaderName::from_static("x-test"),
            reqwest::header::HeaderValue::from_str(&big_jp)
                .expect("multibyte UTF-8 is valid HTTP header bytes"),
        );

        // The load-bearing assertion: this call must not panic.
        let map = collect_response_headers(&hm);

        let value = &map["x-test"][0];
        // Ends with the marker.
        assert!(
            value.ends_with('…'),
            "truncated multibyte value must end with the marker"
        );
        // Stayed under the cap (plus the marker, which is 3 bytes in UTF-8;
        // give a tiny bit of slack since the walk-back may shave up to 2
        // bytes off to find a char boundary).
        assert!(
            value.len() <= MAX_HEADER_VALUE_BYTES + '…'.len_utf8(),
            "truncated value must be at most {} + marker bytes, got {}",
            MAX_HEADER_VALUE_BYTES,
            value.len(),
        );
        // The truncated prefix must still be valid UTF-8 and contain only
        // whole `日` characters before the marker — i.e. the walk-back
        // didn't leave a mojibake byte behind.
        let prefix = value.trim_end_matches('…');
        assert!(
            prefix.chars().all(|c| c == '日'),
            "truncated prefix must contain only whole `日` codepoints, got prefix len {}",
            prefix.len()
        );
    }

    /// #956 — happy-path ASCII truncation: pure single-byte input must
    /// truncate at the cap exactly so the common case isn't penalized by
    /// the char-boundary walk-back.
    #[test]
    fn collect_response_headers_ascii_truncation_hits_exact_cap() {
        let mut hm = reqwest::header::HeaderMap::new();
        let big = "x".repeat(16 * 1024); // 16 KB, well above the 8 KB cap
        hm.insert(
            reqwest::header::HeaderName::from_static("x-test"),
            reqwest::header::HeaderValue::from_str(&big).expect("ASCII is valid"),
        );

        let map = collect_response_headers(&hm);
        let value = &map["x-test"][0];
        // Exactly MAX_HEADER_VALUE_BYTES of ASCII plus the 3-byte `…`
        // marker — no slack, no walk-back, since every byte boundary is
        // a char boundary in pure ASCII.
        assert_eq!(value.len(), MAX_HEADER_VALUE_BYTES + '…'.len_utf8());
        assert!(value.ends_with('…'));
    }

    /// #956 — `collect_response_headers` is pure and can be unit-tested
    /// without a live server.
    #[test]
    fn collect_response_headers_lowercases_and_groups() {
        let mut hm = reqwest::header::HeaderMap::new();
        hm.append(
            reqwest::header::HeaderName::from_static("set-cookie"),
            reqwest::header::HeaderValue::from_static("a=1"),
        );
        hm.append(
            reqwest::header::HeaderName::from_static("set-cookie"),
            reqwest::header::HeaderValue::from_static("b=2"),
        );
        hm.insert(
            reqwest::header::HeaderName::from_static("content-type"),
            reqwest::header::HeaderValue::from_static("text/plain"),
        );

        let map = collect_response_headers(&hm);
        assert_eq!(
            map.get("content-type").map(|v| v.as_slice()),
            Some(["text/plain".to_string()].as_slice())
        );
        let cookies = map.get("set-cookie").expect("set-cookie key");
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0], "a=1");
        assert_eq!(cookies[1], "b=2");
    }
}
