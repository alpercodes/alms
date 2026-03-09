//! Bearer token authentication middleware.

use axum::{
    Extension,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use tracing::warn;

/// Wrapper for the optional auth token, injected via `Extension`.
#[derive(Clone, Debug)]
pub struct AuthToken(pub Option<String>);

/// Axum middleware that enforces Bearer token authentication.
///
/// If the token is `None`, all requests pass through (dev mode).
/// Otherwise, the `Authorization: Bearer <token>` header must match.
/// Also accepts `?token=<token>` query parameter for SSE EventSource
/// compatibility (browser EventSource cannot set custom headers).
pub async fn require_auth(
    Extension(auth): Extension<AuthToken>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(ref expected) = auth.0 else {
        // No token configured — auth disabled (dev mode)
        return next.run(req).await;
    };

    // Check Authorization header
    if let Some(auth_header) = req.headers().get("authorization")
        && let Ok(value) = auth_header.to_str()
        && let Some(token) = value.strip_prefix("Bearer ")
        && token == expected.as_str()
    {
        return next.run(req).await;
    }

    // Fallback: check ?token= query parameter (for EventSource SSE)
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=")
                && token == expected.as_str()
            {
                return next.run(req).await;
            }
        }
    }

    warn!("Rejected unauthenticated request to {}", req.uri().path());
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": { "code": "UNAUTHORIZED", "message": "Missing or invalid Bearer token" }
        })),
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

    fn app(token: Option<&str>) -> Router {
        Router::new()
            .route("/test", get(dummy))
            .layer(middleware::from_fn(require_auth))
            .layer(Extension(AuthToken(token.map(String::from))))
    }

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

    #[tokio::test]
    async fn test_query_param_token_passes() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test?token=secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_query_param_wrong_token_returns_401() {
        let resp = app(Some("secret"))
            .oneshot(
                HttpRequest::get("/test?token=wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
