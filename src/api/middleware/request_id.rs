//! Request ID Middleware
//!
//! Manages the `X-Request-ID` header:
//! - Reads from incoming requests
//! - Generates a new UUID if not present
//! - Echoes back in response headers
//! - Makes available to route handlers via extensions

use axum::{
    body::Body,
    http::{HeaderValue, Request, Response},
    middleware::Next,
};
use uuid::Uuid;

use crate::api::REQUEST_ID_HEADER;

/// Extension key for request ID
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Middleware that handles request ID propagation
pub async fn request_id_middleware(mut request: Request<Body>, next: Next) -> Response<Body> {
    // Get or generate request ID
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Make request ID available to handlers
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    // Continue processing
    let mut response = next.run(request).await;

    // Echo request ID in response
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    response
}

/// Extract request ID from extensions
pub fn get_request_id<B>(request: &Request<B>) -> Option<&RequestId> {
    request.extensions().get::<RequestId>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::Extension, http::StatusCode, routing::get, Router};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_request_id_echoed() {
        let app = Router::new()
            .route("/test", get(|| async { "hello" }))
            .layer(axum::middleware::from_fn(request_id_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(REQUEST_ID_HEADER, "test-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let request_id_header = response.headers().get(REQUEST_ID_HEADER);
        assert!(request_id_header.is_some());
        assert_eq!(request_id_header.unwrap(), "test-123");
    }

    #[tokio::test]
    async fn test_request_id_generated() {
        let app = Router::new()
            .route("/test", get(|| async { "hello" }))
            .layer(axum::middleware::from_fn(request_id_middleware));

        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Should have a generated UUID
        let request_id_header = response.headers().get(REQUEST_ID_HEADER);
        assert!(request_id_header.is_some());
        let id = request_id_header.unwrap().to_str().unwrap();
        // Verify it's a valid UUID format
        assert!(Uuid::parse_str(id).is_ok());
    }

    #[tokio::test]
    async fn test_request_id_available_in_handler() {
        let app = Router::new()
            .route(
                "/test",
                get(|Extension(req_id): Extension<RequestId>| async move { req_id.0 }),
            )
            .layer(axum::middleware::from_fn(request_id_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(REQUEST_ID_HEADER, "handler-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // The handler returns the request ID it received
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, "handler-test");
    }
}
