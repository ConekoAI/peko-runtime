//! Version Header Middleware
//!
//! Injects the `X-Pekobot-Version` header into all responses.

use axum::{
    body::Body,
    http::{Request, Response},
    middleware::Next,
};

use crate::api::{VERSION, VERSION_HEADER};

/// Middleware that adds the Pekobot version header to all responses
pub async fn version_middleware(request: Request<Body>, next: Next) -> Response<Body> {
    let mut response = next.run(request).await;

    // Add version header
    response
        .headers_mut()
        .insert(VERSION_HEADER, VERSION.parse().unwrap());

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::StatusCode, routing::get, Router};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_version_header_injected() {
        let app = Router::new()
            .route("/test", get(|| async { "hello" }))
            .layer(axum::middleware::from_fn(version_middleware));

        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let version_header = response.headers().get(VERSION_HEADER);
        assert!(version_header.is_some());
        assert_eq!(version_header.unwrap(), VERSION);
    }
}
