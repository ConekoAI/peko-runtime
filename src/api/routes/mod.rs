//! API Route Handlers
//!
//! This module contains all HTTP endpoint handlers organized by resource:
//! - health: Service health checks
//! - info: Daemon information
//! - agents: Instance management (Milestone 2)
//! - sessions: Session management (Milestone 3)
//! - teams: Team management (Milestone 7)
//! - images: Image registry (Milestone 2)
//! - webhooks: Webhook endpoint (Milestone 8)
//! - events: System event stream (Milestone 8)

pub mod agents;
pub mod chat;
pub mod events;
pub mod health;
pub mod images;
pub mod info;
pub mod metrics;
pub mod sessions;
pub mod teams;
pub mod webhooks;
pub mod websocket;

use axum::{routing::get, Router};

use crate::api::state::AppState;

/// Create the API router with all routes
pub fn create_router() -> Router<AppState> {
    Router::new()
        // Health and info endpoints (Milestone 1)
        .route("/health", get(health::health_check))
        .route("/info", get(info::daemon_info))
        // Merge nested routers (Milestone 2, 3, 4 & 7)
        .merge(images::router())
        .merge(agents::router())
        .merge(sessions::router())
        .merge(chat::router())
        .merge(teams::routes())
        .merge(websocket::router())
        // Milestone 8: Webhooks and system events
        .merge(webhooks::router())
        .merge(events::router())
        // Milestone 12: Performance metrics
        .merge(metrics::router())
        // Milestone 10: Web UI
        .merge(crate::web_ui::router())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_state() -> AppState {
        AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
        )
    }

    #[tokio::test]
    async fn test_router_has_health_endpoint() {
        let app = create_router().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_router_has_info_endpoint() {
        let app = create_router().with_state(test_state());

        let response = app
            .oneshot(Request::builder().uri("/info").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_router_returns_404_for_unknown_path() {
        let app = create_router().with_state(test_state());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 404);
    }
}
