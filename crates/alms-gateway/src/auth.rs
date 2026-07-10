//! Bearer token authentication middleware and API response headers.

use axum::{
    Extension,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::warn;

/// Wrapper for the optional auth token, injected via `Extension`.
#[derive(Clone, Debug)]
pub struct AuthToken(pub Option<String>);

/// Canonical source of truth for SSE streaming endpoints.
///
/// Each entry is the first path segment of an SSE route whose full shape is
/// `/<segment>/{id}/events`.  Used by:
///
/// - [`is_sse_endpoint`] (the auth-middleware matcher that decides whether
///   `?token=<token>` query-string auth is accepted for a request),
/// - `crate::server::routes::sse_route_specs` (the production route-table
///   builder, where the same list is turned into axum path + handler
///   pairs and registered on the protected router), and
/// - `crate::server::routes::sse_route_paths` (the stateless path-only
///   helper, consumed by the auth tests' in-test `Router`).
///
/// Adding a new SSE endpoint = appending one entry here plus a matching
/// handler arm in `sse_route_specs()`.  No other matcher / test app
/// needs to be kept in sync, which is the bug #905 / PR #904 closed.
pub(crate) const SSE_ENDPOINT_SEGMENTS: &[&str] = &["runs", "sessions", "agents"];

/// Returns `true` if `path` is an SSE streaming endpoint where the browser's
/// `EventSource` API cannot send custom headers, so query-string auth is the
/// only viable option.
///
/// Derived from [`SSE_ENDPOINT_SEGMENTS`] — to register a new SSE endpoint,
/// add a segment there and the matcher picks it up automatically.
fn is_sse_endpoint(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // Pattern: ["<segment>", <id>, "events"] where <segment> is in the
    // canonical SSE list.
    match segments.as_slice() {
        [first, _id, "events"] => SSE_ENDPOINT_SEGMENTS.contains(first),
        // Global cross-agent session-activity feed (#1211). Parameterless
        // (no `{id}`), so it isn't derived from SSE_ENDPOINT_SEGMENTS —
        // whitelisted explicitly. Uses `?token=` like the other SSE feeds
        // because the browser `EventSource` API cannot set custom headers.
        ["events", "session-activity"] => true,
        _ => false,
    }
}

/// Axum middleware that enforces Bearer token authentication.
///
/// If the token is `None`, all requests pass through (dev mode).
/// Otherwise, the `Authorization: Bearer <token>` header must match.
///
/// The `?token=<token>` query parameter is accepted **only** on SSE
/// endpoints (`/runs/{id}/events`, `/sessions/{id}/events`,
/// `/agents/{id}/events`) where the browser `EventSource` API cannot set
/// custom headers.  On all other routes, query-string auth is rejected to
/// prevent credential leakage into server logs, browser history, and HTTP
/// `Referer` headers.
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

/// Axum middleware that adds `Cache-Control: no-store` to every response.
///
/// Applied to the authenticated API router so browsers never heuristically
/// cache JSON API responses. Static asset routes are on the public router
/// and already set their own `Cache-Control` header, so this middleware
/// does not interfere with them.
pub async fn no_cache(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
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
    ///
    /// The SSE routes are derived from
    /// [`crate::server::routes::sse_route_paths`] rather than hand-mirrored,
    /// so adding a new SSE endpoint in production picks up the auth-middleware
    /// test coverage automatically (#905).
    fn app(token: Option<&str>) -> Router {
        let mut router = Router::new().route("/test", get(dummy));
        for path in crate::server::routes::sse_route_paths() {
            router = router.route(&path, get(dummy));
        }
        // Global cross-agent session-activity feed (#1211). Parameterless,
        // so it isn't part of the segment-derived sse_route_paths(); register
        // it explicitly to mirror production (routes.rs) and exercise the
        // is_sse_endpoint whitelist arm for it.
        router = router.route("/events/session-activity", get(dummy));
        router
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

    // --- Bearer header on SSE endpoints (regression guard) ---

    #[tokio::test]
    async fn test_bearer_header_accepted_on_run_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/runs/some-run-id/events")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_bearer_header_accepted_on_session_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/sessions/some-session-id/events")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_bearer_header_accepted_on_agent_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/agents/some-agent-id/events")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
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
    async fn test_query_param_accepted_on_agent_events_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/agents/some-agent-id/events?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // --- Global cross-agent session-activity feed (#1211) ---

    #[tokio::test]
    async fn test_bearer_header_accepted_on_session_activity_sse() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/events/session-activity")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_accepted_on_session_activity_sse() {
        // The browser `EventSource` cannot set headers, so the global
        // activity feed must accept `?token=` like the other SSE feeds.
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/events/session-activity?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_wrong_token_on_session_activity_returns_401() {
        // The whitelist accepts the `?token=` form on this endpoint but must
        // still enforce the value — a wrong token is rejected.
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/events/session-activity?token=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
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
        assert!(is_sse_endpoint("/agents/ghi-789/events"));
    }

    #[test]
    fn test_is_sse_endpoint_negative() {
        assert!(!is_sse_endpoint("/test"));
        assert!(!is_sse_endpoint("/runs"));
        assert!(!is_sse_endpoint("/runs/abc/cancel"));
        assert!(!is_sse_endpoint("/sessions/abc/messages"));
        assert!(!is_sse_endpoint("/agents"));
        assert!(!is_sse_endpoint("/agents/abc"));
        assert!(!is_sse_endpoint("/agents/abc/sessions"));
        assert!(!is_sse_endpoint("/health"));
    }

    // --- Structural regression guard for #905 ---

    /// Every route returned by the production SSE route-table helper
    /// must be accepted by `is_sse_endpoint()`.  If anyone ever adds a
    /// new entry to `SSE_ENDPOINT_SEGMENTS` (or to the route table
    /// directly via some other path) without keeping the matcher in
    /// sync, this test trips before the silent-401 regression that
    /// caused #885 / #900 can ship.
    ///
    /// The axum `{id}` placeholder is substituted with a concrete UUID
    /// before calling `is_sse_endpoint()` because the matcher operates
    /// on real request paths, not route templates.
    #[test]
    fn test_sse_route_paths_match_is_sse_endpoint() {
        let paths = crate::server::routes::sse_route_paths();
        assert!(
            !paths.is_empty(),
            "sse_route_paths() returned an empty list — at minimum runs/sessions/agents are expected"
        );
        for template in paths {
            let concrete = template.replace("{id}", "00000000-0000-0000-0000-000000000000");
            assert!(
                is_sse_endpoint(&concrete),
                "is_sse_endpoint() rejects \"{concrete}\" (template \"{template}\") — \
                 SSE_ENDPOINT_SEGMENTS and is_sse_endpoint() have drifted; \
                 see crates/alms-gateway/src/auth.rs and issue #905"
            );
        }
    }

    // --- no_cache middleware tests ---

    /// Build a test router with the no_cache middleware applied.
    fn app_with_no_cache() -> Router {
        Router::new()
            .route("/api", get(dummy))
            .layer(middleware::from_fn(no_cache))
    }

    #[tokio::test]
    async fn test_no_cache_sets_cache_control_header() {
        let resp = app_with_no_cache()
            .oneshot(HttpRequest::get("/api").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
