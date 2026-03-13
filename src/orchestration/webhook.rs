//! Webhook Server for orchestration layer
//!
//! HTTP server that receives external webhooks and emits SystemEvent::Webhook events.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::orchestration::events::SystemEvent;

/// Webhook route configuration
#[derive(Debug, Clone)]
pub struct WebhookRoute {
    /// Route path (e.g., "/github", "/slack")
    pub path: String,
    /// Agent to invoke
    pub agent_id: String,
    /// Optional secret for HMAC verification
    pub secret: Option<String>,
    /// Optional source identifier
    pub source: String,
}

impl WebhookRoute {
    /// Create a new webhook route
    pub fn new(path: impl Into<String>, agent_id: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            agent_id: agent_id.into(),
            secret: None,
            source: "webhook".to_string(),
        }
    }

    /// Set the source identifier
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Set the secret for verification
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
        self
    }
}

/// Webhook server state shared across handlers
#[derive(Clone)]
struct WebhookState {
    routes: Arc<RwLock<HashMap<String, WebhookRoute>>>,
    event_tx: mpsc::Sender<SystemEvent>,
}

/// Webhook server
pub struct WebhookServer {
    port: u16,
    routes: HashMap<String, WebhookRoute>,
    event_tx: mpsc::Sender<SystemEvent>,
}

impl WebhookServer {
    /// Create a new webhook server
    pub fn new(port: u16, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            port,
            routes: HashMap::new(),
            event_tx,
        }
    }

    /// Register a webhook route
    pub fn register_route(&mut self, route: WebhookRoute) {
        info!(
            "Registering webhook route: {} -> agent:{}",
            route.path, route.agent_id
        );
        self.routes.insert(route.path.clone(), route);
    }

    /// Register a simple route
    pub fn register(&mut self, path: impl Into<String>, agent_id: impl Into<String>) {
        self.register_route(WebhookRoute::new(path, agent_id));
    }

    /// Build the axum router
    fn build_router(&self) -> Router {
        let state = WebhookState {
            routes: Arc::new(RwLock::new(self.routes.clone())),
            event_tx: self.event_tx.clone(),
        };

        Router::new()
            .route("/webhook/:route", post(handle_webhook))
            .route("/health", get(health_check))
            .route("/", get(index_handler))
            .with_state(state)
    }

    /// Start the webhook server
    pub async fn start(&self) -> anyhow::Result<()> {
        let app = self.build_router();
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));

        info!("Starting webhook server on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    /// Start the server in a background task
    pub fn spawn(self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::spawn(async move { self.start().await })
    }
}

/// Index handler showing registered routes
async fn index_handler(State(state): State<WebhookState>) -> impl IntoResponse {
    let routes = state.routes.read().await;
    let route_list: Vec<_> = routes
        .values()
        .map(|r| {
            serde_json::json!({
                "path": r.path,
                "agent_id": r.agent_id,
                "source": r.source,
            })
        })
        .collect();

    Json(serde_json::json!({
        "service": "pekobot-webhook",
        "version": env!("CARGO_PKG_VERSION"),
        "routes": route_list,
        "endpoints": {
            "health": "/health",
            "webhook": "/webhook/:route",
        }
    }))
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "pekobot-webhook"
    }))
}

/// Handle incoming webhook
async fn handle_webhook(
    State(state): State<WebhookState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    debug!("Received webhook on route: {}", route);

    // Look up route configuration
    let routes = state.routes.read().await;
    let route_config = match routes.get(&format!("/{}", route)) {
        Some(config) => config.clone(),
        None => {
            warn!("Unknown webhook route: {}", route);
            return (StatusCode::NOT_FOUND, "Unknown route").into_response();
        }
    };
    drop(routes);

    // TODO: Verify secret if configured
    if route_config.secret.is_some() {
        // Implement HMAC verification here
        debug!("Secret verification not yet implemented");
    }

    // Parse payload as JSON
    let payload = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(_) => serde_json::json!({ "raw": body }),
    };

    // Extract headers
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();

    // Create system event
    let event = SystemEvent::Webhook {
        source: route_config.source.clone(),
        route: route.clone(),
        payload,
        headers: header_map,
        timestamp: chrono::Utc::now(),
    };

    // Send the event
    if let Err(e) = state.event_tx.send(event).await {
        error!("Failed to send webhook event: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to process webhook",
        )
            .into_response();
    }

    (StatusCode::OK, "Webhook received").into_response()
}

/// Builder for creating a webhook server with multiple routes
pub struct WebhookServerBuilder {
    port: u16,
    event_tx: mpsc::Sender<SystemEvent>,
    routes: Vec<WebhookRoute>,
}

impl WebhookServerBuilder {
    /// Create a new builder
    pub fn new(port: u16, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            port,
            event_tx,
            routes: Vec::new(),
        }
    }

    /// Add a webhook route
    pub fn add_route(mut self, route: WebhookRoute) -> Self {
        self.routes.push(route);
        self
    }

    /// Add a simple route
    pub fn route(mut self, path: impl Into<String>, agent_id: impl Into<String>) -> Self {
        self.routes.push(WebhookRoute::new(path, agent_id));
        self
    }

    /// Build the webhook server
    pub fn build(self) -> WebhookServer {
        let mut server = WebhookServer::new(self.port, self.event_tx);
        for route in self.routes {
            server.register_route(route);
        }
        server
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_route_builder() {
        let route = WebhookRoute::new("/github", "github-agent")
            .with_source("github")
            .with_secret("my-secret");

        assert_eq!(route.path, "/github");
        assert_eq!(route.agent_id, "github-agent");
        assert_eq!(route.source, "github");
        assert_eq!(route.secret, Some("my-secret".to_string()));
    }

    #[test]
    fn test_webhook_route_new() {
        let route = WebhookRoute::new("/slack", "slack-agent");
        assert_eq!(route.path, "/slack");
        assert_eq!(route.agent_id, "slack-agent");
        assert_eq!(route.source, "webhook");
        assert_eq!(route.secret, None);
    }

    #[tokio::test]
    async fn test_webhook_server_builder() {
        let (tx, mut rx) = mpsc::channel(10);

        let server = WebhookServerBuilder::new(3000, tx)
            .route("/github", "github-agent")
            .route("/slack", "slack-agent")
            .build();

        assert_eq!(server.port, 3000);
        assert_eq!(server.routes.len(), 2);
        assert!(server.routes.contains_key("/github"));
        assert!(server.routes.contains_key("/slack"));

        // Verify we can receive events
        let event = SystemEvent::Webhook {
            source: "test".to_string(),
            route: "test-route".to_string(),
            payload: serde_json::json!({}),
            headers: HashMap::new(),
            timestamp: chrono::Utc::now(),
        };

        server.event_tx.send(event).await.unwrap();

        let received = rx.recv().await;
        assert!(received.is_some());
    }

    #[test]
    fn test_webhook_route_lookup() {
        let (tx, _rx) = mpsc::channel::<SystemEvent>(10);
        let mut server = WebhookServer::new(8080, tx);

        server.register("/github", "github-agent");
        server.register("/slack", "slack-agent");

        assert!(server.routes.contains_key("/github"));
        assert!(server.routes.contains_key("/slack"));

        let github_route = server.routes.get("/github").unwrap();
        assert_eq!(github_route.agent_id, "github-agent");
        assert_eq!(github_route.source, "webhook");
    }
}
