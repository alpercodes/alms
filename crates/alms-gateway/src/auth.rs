//! Bearer token authentication middleware.

use axum::{
    Extension,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::warn;

/// Wrapper for the optional auth token, injected via `Extension`.
#[derive(Clone, Debug)]
pub struct AuthToken(pub Option<String>);

/// Returns `true` if `path` is an SSE streaming endpoint where the browser's
/// `EventSource` API cannot send custom headers, so query-string auth is the
/// only viable option.
///
/// Recognised SSE paths:
///   - `/runs/{run_id}/events`
///   - `/sessions/{session_id}/events`
fn is_sse_endpoint(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // Pattern: ["runs", <id>, "events"] or ["sessions", <id>, "events"]
    matches!(
        segments.as_slice(),
        ["runs", _, "events"] | ["sessions", _, "events"]
    )
}

/// Axum middleware that enforces Bearer token authentication.
///
/// If the token is `None`, all requests pass through (dev mode).
/// Otherwise, the `Authorization: Bearer <token>` header must match.
///
/// The `?token=<token>` query parameter is accepted **only** on SSE
/// endpoints (`/runs/{id}/events`, `/sessions/{id}/events`) where the
/// browser `EventSource` API cannot set custom headers.  On all other
/// routes, query-string auth is rejected to prevent credential leakage
/// into server logs, browser history, and HTTP `Referer` headers.
pub async fn require_auth(
    Extension(auth): Extension<AuthToken>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(ref expected) = auth.0 else {
        // No token configured — auth disabled (dev mode)
        return next.run(req).await;
    };

    // Check Authorization header (all routes)
    if let Some(auth_header) = req.headers().get("authorization")
        && let Ok(value) = auth_header.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
        && token == expected.as_str()
    {
        return next.run(req).await;
    }

    // Fallback: check ?token= query parameter, but ONLY on SSE endpoints.
    // Bearer tokens in URLs leak into server logs, browser history, and
    // HTTP Referer headers, so we restrict this to SSE paths where
    // EventSource cannot set Authorization headers.
    if is_sse_endpoint(req.uri().path())
        && let Some(query) = req.uri().query()
    {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=")
                && token == expected.as_str()
            {
                return next.run(req).await;
            }
        }
    }

    warn!("Rejected unauthenticated request to {}", req.uri().path());
    crate::api_error(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "Missing or invalid Bearer token",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::{Router, middleware, routing::get};
    use tower::ServiceExt;

    async fn dummy() -> &'static str {
        "ok"
    }

    /// Build a test router with both a regular route and SSE-like routes.
    fn app(token: Option<&str>) -> Router {
        Router::new()
            .route("/test", get(dummy))
            .route("/runs/{run_id}/events", get(dummy))
            .route("/sessions/{session_id}/events", get(dummy))
            .layer(middleware::from_fn(require_auth))
            .layer(Extension(AuthToken(token.map(String::from))))
    }

    // --- Bearer header tests (unchanged) ---

    #[tokio::test]
    async fn test_auth_disabled_passes_all() {
        let resp = app(None)
            .oneshot(HttpRequest::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_valid_bearer_passes() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_missing_header_returns_401() {
        let resp = app(Some("secret"))
            .oneshot(HttpRequest::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_wrong_token_returns_401() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test")
                    .header("Authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_malformed_header_returns_401() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test")
                    .header("Authorization", "Basic secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // --- Query-string token: rejected on non-SSE routes ---

    #[tokio::test]
    async fn test_query_param_rejected_on_non_sse_route() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // --- Query-string token: accepted on SSE endpoints ---

    #[tokio::test]
    async fn test_query_param_accepted_on_run_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/runs/some-run-id/events?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_accepted_on_session_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/sessions/some-session-id/events?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_wrong_token_on_sse_returns_401() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/runs/some-run-id/events?token=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // --- is_sse_endpoint unit tests ---

    #[test]
    fn test_is_sse_endpoint_positive() {
        assert!(is_sse_endpoint("/runs/abc-123/events"));
        assert!(is_sse_endpoint("/sessions/def-456/events"));
    }

    #[test]
    fn test_is_sse_endpoint_negative() {
        assert!(!is_sse_endpoint("/test"));
        assert!(!is_sse_endpoint("/runs"));
        assert!(!is_sse_endpoint("/runs/abc/cancel"));
        assert!(!is_sse_endpoint("/sessions/abc/messages"));
        assert!(!is_sse_endpoint("/agents"));
        assert!(!is_sse_endpoint("/health"));
    }
}
