//! External Ingress - Unified webhook endpoint for all external services
//!
//! Provides a single `/webhook/ingress` endpoint that receives events from
//! external services (Discord, GitHub, Slack, etc.) and routes them based on
//! configurable source detection rules.
//!
//! This complements the native event sources (file watcher, cron, internal)
//! by providing a unified HTTP ingress for external SaaS integrations.

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

/// Source detection method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SourceDetection {
    /// Detect by HTTP header presence/value
    Header {
        name: String,
        /// Optional expected value prefix/pattern
        value_prefix: Option<String>,
    },
    /// Detect by JSON payload field
    PayloadField {
        path: String,
        /// Optional expected value
        value: Option<String>,
    },
    /// Detect by User-Agent header
    UserAgent { contains: String },
}

/// External source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalSource {
    /// Source identifier (e.g., "github", "discord")
    pub name: String,
    /// How to detect this source
    pub detection: SourceDetection,
    /// Agent to invoke
    pub agent_id: String,
    /// Verification method (HMAC, etc.)
    pub verification: Option<VerificationConfig>,
    /// Transform payload before creating event
    pub transform: Option<String>, // JSONata or JSON path expression
}

/// Verification configuration for webhook security
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationConfig {
    /// HMAC-SHA256 signature verification
    HmacSha256 { header: String, secret: String },
    /// Ed25519 signature verification (Discord style)
    Ed25519 { public_key: String },
    /// Static token in header
    BearerToken { header: String, token: String },
}

/// External ingress configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalIngressConfig {
    /// Enable unified ingress
    pub enabled: bool,
    /// Port to listen on
    pub port: u16,
    /// Bind address
    pub bind_address: String,
    /// Endpoint path (default: "/webhook/ingress")
    pub endpoint: String,
    /// Registered external sources
    pub sources: Vec<ExternalSource>,
}

impl Default for ExternalIngressConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
            bind_address: "0.0.0.0".to_string(),
            endpoint: "/webhook/ingress".to_string(),
            sources: Vec::new(),
        }
    }
}

impl ExternalIngressConfig {
    /// Add a new external source
    pub fn add_source(&mut self, source: ExternalSource) {
        self.sources.push(source);
    }

    /// Find source by detection
    pub fn detect_source(&self, headers: &HeaderMap, body: &str) -> Option<&ExternalSource> {
        for source in &self.sources {
            if Self::matches_detection(&source.detection, headers, body) {
                return Some(source);
            }
        }
        None
    }

    /// Check if detection matches
    fn matches_detection(detection: &SourceDetection, headers: &HeaderMap, body: &str) -> bool {
        match detection {
            SourceDetection::Header { name, value_prefix } => {
                if let Some(header_value) = headers.get(name) {
                    if let Ok(value_str) = header_value.to_str() {
                        if let Some(prefix) = value_prefix {
                            return value_str.starts_with(prefix);
                        }
                        return true; // Header exists, no specific value required
                    }
                }
                false
            }
            SourceDetection::PayloadField { path, value } => {
                // Simple JSON path extraction (supports dot notation like "event.type")
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                    let field_value = path
                        .split('.')
                        .fold(Some(&json), |acc, key| acc.and_then(|v| v.get(key)));

                    if let Some(expected) = value {
                        field_value
                            .and_then(|v| v.as_str())
                            .map(|v| v == expected)
                            .unwrap_or(false)
                    } else {
                        field_value.is_some()
                    }
                } else {
                    false
                }
            }
            SourceDetection::UserAgent { contains } => headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .map(|ua| ua.to_lowercase().contains(&contains.to_lowercase()))
                .unwrap_or(false),
        }
    }
}

/// External ingress server state
#[derive(Clone)]
struct IngressState {
    config: Arc<RwLock<ExternalIngressConfig>>,
    event_tx: mpsc::Sender<SystemEvent>,
    /// Event counter for metrics
    counter: Arc<RwLock<u64>>,
}

/// Unified external ingress server
pub struct ExternalIngress {
    config: ExternalIngressConfig,
    event_tx: mpsc::Sender<SystemEvent>,
}

impl ExternalIngress {
    /// Create a new external ingress server
    pub fn new(config: ExternalIngressConfig, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self { config, event_tx }
    }

    /// Build the axum router
    fn build_router(&self) -> Router {
        let state = IngressState {
            config: Arc::new(RwLock::new(self.config.clone())),
            event_tx: self.event_tx.clone(),
            counter: Arc::new(RwLock::new(0)),
        };

        Router::new()
            .route(&self.config.endpoint, post(handle_ingress))
            .route("/health", get(health_check))
            .route("/ingress/sources", get(list_sources))
            .route("/ingress/sources/:name", get(get_source))
            .with_state(state)
    }

    /// Start the ingress server
    pub async fn start(&self) -> anyhow::Result<()> {
        let app = self.build_router();
        let addr = SocketAddr::from((
            self.config
                .bind_address
                .parse::<std::net::IpAddr>()
                .unwrap_or_else(|_| "0.0.0.0".parse().unwrap()),
            self.config.port,
        ));

        info!(
            "Starting external ingress on {} (endpoint: {})",
            addr, self.config.endpoint
        );
        info!("Registered {} external sources", self.config.sources.len());

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    /// Spawn in background
    pub fn spawn(self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::spawn(async move { self.start().await })
    }
}

/// Handle unified ingress request
async fn handle_ingress(
    State(state): State<IngressState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Increment counter
    {
        let mut counter = state.counter.write().await;
        *counter += 1;
    }

    debug!("Received ingress request, body size: {} bytes", body.len());

    // Get config
    let config = state.config.read().await;

    // Detect source
    let source = match config.detect_source(&headers, &body) {
        Some(s) => s.clone(),
        None => {
            warn!("Could not detect source from request");
            debug!("Headers: {:?}", headers);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Unknown source",
                    "message": "Could not detect event source from request"
                })),
            )
                .into_response();
        }
    };
    drop(config);

    info!("Detected source: {}", source.name);

    // TODO: Verify signature if configured
    if let Some(_verification) = &source.verification {
        debug!("Verification required for source: {}", source.name);
        // TODO: Implement HMAC/Ed25519 verification
    }

    // Parse payload
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
        source: source.name.clone(),
        route: format!("/ingress/{}", source.name),
        payload,
        headers: header_map,
        timestamp: chrono::Utc::now(),
    };

    // Send event
    if let Err(e) = state.event_tx.send(event).await {
        error!("Failed to send ingress event: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Event processing failed"
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "source": source.name,
            "agent": source.agent_id
        })),
    )
        .into_response()
}

/// Health check endpoint
async fn health_check(State(state): State<IngressState>) -> impl IntoResponse {
    let config = state.config.read().await;
    let counter = state.counter.read().await;

    Json(serde_json::json!({
        "status": "ok",
        "service": "external-ingress",
        "sources": config.sources.len(),
        "events_received": *counter,
        "endpoint": config.endpoint
    }))
}

/// List all configured sources
async fn list_sources(State(state): State<IngressState>) -> impl IntoResponse {
    let config = state.config.read().await;

    let sources: Vec<_> = config
        .sources
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "agent_id": s.agent_id,
                "detection": format!("{:?}", s.detection),
            })
        })
        .collect();

    Json(serde_json::json!({ "sources": sources }))
}

/// Get specific source info
async fn get_source(
    State(state): State<IngressState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let config = state.config.read().await;

    match config.sources.iter().find(|s| s.name == name) {
        Some(source) => Json(serde_json::json!({
            "name": source.name,
            "agent_id": source.agent_id,
            "detection": format!("{:?}", source.detection),
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Source not found"
            })),
        )
            .into_response(),
    }
}

/// Builder for ExternalIngress
pub struct ExternalIngressBuilder {
    config: ExternalIngressConfig,
    event_tx: Option<mpsc::Sender<SystemEvent>>,
}

impl ExternalIngressBuilder {
    /// Create new builder
    pub fn new() -> Self {
        Self {
            config: ExternalIngressConfig::default(),
            event_tx: None,
        }
    }

    /// Set port
    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    /// Set endpoint path
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.endpoint = endpoint.into();
        self
    }

    /// Set event sender
    pub fn with_event_channel(mut self, tx: mpsc::Sender<SystemEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Add external source
    pub fn add_source(mut self, source: ExternalSource) -> Self {
        self.config.sources.push(source);
        self
    }

    /// Build the ingress server
    pub fn build(self) -> anyhow::Result<ExternalIngress> {
        let event_tx = self
            .event_tx
            .ok_or_else(|| anyhow::anyhow!("Event channel required"))?;

        Ok(ExternalIngress::new(self.config, event_tx))
    }
}

impl Default for ExternalIngressBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ExternalIngressConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.port, 8080);
        assert_eq!(config.endpoint, "/webhook/ingress");
    }

    #[test]
    fn test_source_detection_header() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "github".to_string(),
            detection: SourceDetection::Header {
                name: "X-GitHub-Event".to_string(),
                value_prefix: None,
            },
            agent_id: "github-agent".to_string(),
            verification: None,
            transform: None,
        });

        let mut headers = HeaderMap::new();
        headers.insert("X-GitHub-Event", "push".parse().unwrap());

        let source = config.detect_source(&headers, "{}");
        assert!(source.is_some());
        assert_eq!(source.unwrap().name, "github");
    }

    #[test]
    fn test_source_detection_header_with_prefix() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "stripe".to_string(),
            detection: SourceDetection::Header {
                name: "Authorization".to_string(),
                value_prefix: Some("Bearer stripe_".to_string()),
            },
            agent_id: "stripe-agent".to_string(),
            verification: None,
            transform: None,
        });

        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer stripe_test_123".parse().unwrap());

        let source = config.detect_source(&headers, "{}");
        assert!(source.is_some());
        assert_eq!(source.unwrap().name, "stripe");
    }

    #[test]
    fn test_source_detection_payload_field() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "discord".to_string(),
            detection: SourceDetection::PayloadField {
                path: "type".to_string(),
                value: Some("interaction".to_string()),
            },
            agent_id: "discord-agent".to_string(),
            verification: None,
            transform: None,
        });

        let headers = HeaderMap::new();
        let body = r#"{"type":"interaction","data":{"name":"test"}}"#;

        let source = config.detect_source(&headers, body);
        assert!(source.is_some());
        assert_eq!(source.unwrap().name, "discord");
    }

    #[test]
    fn test_source_detection_nested_payload() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "slack".to_string(),
            detection: SourceDetection::PayloadField {
                path: "event.type".to_string(),
                value: Some("message".to_string()),
            },
            agent_id: "slack-agent".to_string(),
            verification: None,
            transform: None,
        });

        let headers = HeaderMap::new();
        let body = r#"{"event":{"type":"message","text":"hello"}}"#;

        let source = config.detect_source(&headers, body);
        assert!(source.is_some());
        assert_eq!(source.unwrap().name, "slack");
    }

    #[test]
    fn test_source_detection_user_agent() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "custom-bot".to_string(),
            detection: SourceDetection::UserAgent {
                contains: "MyBot".to_string(),
            },
            agent_id: "custom-agent".to_string(),
            verification: None,
            transform: None,
        });

        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "MyBot/1.0".parse().unwrap());

        let source = config.detect_source(&headers, "{}");
        assert!(source.is_some());
        assert_eq!(source.unwrap().name, "custom-bot");
    }

    #[test]
    fn test_source_not_detected() {
        let mut config = ExternalIngressConfig::default();
        config.add_source(ExternalSource {
            name: "github".to_string(),
            detection: SourceDetection::Header {
                name: "X-GitHub-Event".to_string(),
                value_prefix: None,
            },
            agent_id: "github-agent".to_string(),
            verification: None,
            transform: None,
        });

        let headers = HeaderMap::new(); // Missing required header
        let source = config.detect_source(&headers, "{}");
        assert!(source.is_none());
    }

    #[test]
    fn test_builder_pattern() {
        let (tx, _rx) = mpsc::channel(10);

        let ingress = ExternalIngressBuilder::new()
            .port(3000)
            .endpoint("/ingress")
            .with_event_channel(tx)
            .add_source(ExternalSource {
                name: "test".to_string(),
                detection: SourceDetection::Header {
                    name: "X-Test".to_string(),
                    value_prefix: None,
                },
                agent_id: "test-agent".to_string(),
                verification: None,
                transform: None,
            })
            .build();

        assert!(ingress.is_ok());
    }

    #[test]
    fn test_builder_requires_channel() {
        let result = ExternalIngressBuilder::new().port(3000).build();

        assert!(result.is_err());
    }

    #[test]
    fn test_verification_config_serialization() {
        let hmac = VerificationConfig::HmacSha256 {
            header: "X-Hub-Signature-256".to_string(),
            secret: "my-secret".to_string(),
        };

        let json = serde_json::to_string(&hmac).unwrap();
        assert!(json.contains("HmacSha256"));

        let ed25519 = VerificationConfig::Ed25519 {
            public_key: "abc123".to_string(),
        };
        let json = serde_json::to_string(&ed25519).unwrap();
        assert!(json.contains("Ed25519"));
    }
}
