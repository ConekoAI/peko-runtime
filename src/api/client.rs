//! HTTP API Client
//!
//! This module provides a client for the Pekobot HTTP API.
//! All CLI commands use this client to communicate with the daemon.

use crate::api::types::{ErrorResponse, HealthResponse, InfoResponse};
use reqwest::{Client, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use thiserror::Error;

/// Default daemon address
pub const DEFAULT_DAEMON_ADDR: &str = "http://127.0.0.1:11435";

/// Environment variable for daemon address
pub const DAEMON_ADDR_ENV: &str = "PEKOBOT_DAEMON_ADDR";

/// API client errors
#[derive(Debug, Error)]
pub enum ClientError {
    /// HTTP request failed
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// API returned an error
    #[error("API error ({code}): {message}")]
    Api {
        /// HTTP status code
        status: StatusCode,
        /// Error code from API
        code: String,
        /// Error message
        message: String,
        /// Request ID for tracing
        request_id: String,
    },

    /// Resource not found
    #[error("{resource_type} not found: {resource_id}")]
    NotFound {
        /// Type of resource
        resource_type: String,
        /// Resource identifier
        resource_id: String,
    },

    /// Daemon is not running or unreachable
    #[error("Daemon not running at {addr}. Start it with 'pekobot daemon start --foreground'")]
    DaemonNotRunning {
        /// Address that was attempted
        addr: String,
    },

    /// Invalid response from server
    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl ClientError {
    /// Get the exit code for this error
    pub fn exit_code(&self) -> i32 {
        match self {
            ClientError::DaemonNotRunning { .. } => 1,
            ClientError::NotFound { .. } => 5,
            ClientError::Api { status, .. } => match status.as_u16() {
                400 => 2,
                401 => 3,
                403 => 4,
                404 => 5,
                409 => 6,
                500 => 7,
                503 => 8,
                _ => 9,
            },
            ClientError::Http(_) => 10,
            ClientError::InvalidResponse(_) => 11,
            ClientError::Serialization(_) => 12,
        }
    }
}

/// HTTP API Client for Pekobot daemon
#[derive(Debug, Clone)]
pub struct ApiClient {
    /// HTTP client
    client: Client,
    /// Base URL for the daemon
    base_url: String,
}

impl ApiClient {
    /// Create a new API client with the default daemon address
    pub fn new() -> anyhow::Result<Self> {
        let addr =
            std::env::var(DAEMON_ADDR_ENV).unwrap_or_else(|_| DEFAULT_DAEMON_ADDR.to_string());
        Self::with_addr(&addr)
    }

    /// Create a new API client with a specific address
    pub fn with_addr(addr: &str) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()?;

        Ok(Self {
            client,
            base_url: addr.trim_end_matches('/').to_string(),
        })
    }

    /// Get the daemon address
    pub fn addr(&self) -> &str {
        &self.base_url
    }

    /// Check if the daemon is running
    pub async fn health_check(&self) -> Result<HealthResponse, ClientError> {
        self.get("/health").await
    }

    /// Get daemon info
    pub async fn daemon_info(&self) -> Result<InfoResponse, ClientError> {
        self.get("/info").await
    }

    // =================================================================================
    // Agent Endpoints (Stateless - ADR-013)
    // =================================================================================

    /// List all registered agents
    ///
    /// ADR-013: Returns agent configurations, not running instances
    pub async fn list_agents(
        &self,
        team_id: Option<&str>,
    ) -> Result<PaginatedAgentsResponse, ClientError> {
        let mut query = Vec::new();
        if let Some(t) = team_id {
            query.push(("team_id", t));
        }
        self.get_with_query("/agents", &query).await
    }

    /// Register a new agent from image
    ///
    /// ADR-013: Creates configuration, does not start instance
    pub async fn register_agent(
        &self,
        image: &str,
        name: Option<&str>,
        team_id: Option<&str>,
        env: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<AgentConfigResponse, ClientError> {
        let body = serde_json::json!({
            "image": image,
            "name": name,
            "team_id": team_id,
            "env": env,
        });
        self.post("/agents", &body).await
    }

    /// Get an agent configuration by name
    pub async fn get_agent(&self, name: &str) -> Result<AgentConfigResponse, ClientError> {
        let path = format!("/agents/{}", name);
        self.get(&path).await
    }

    /// Update an agent configuration
    pub async fn update_agent(
        &self,
        name: &str,
        image: Option<&str>,
        team_id: Option<&str>,
    ) -> Result<AgentConfigResponse, ClientError> {
        let path = format!("/agents/{}", name);
        let mut body = serde_json::Map::new();
        if let Some(img) = image {
            body.insert(
                "image".to_string(),
                serde_json::Value::String(img.to_string()),
            );
        }
        if let Some(team) = team_id {
            body.insert(
                "team_id".to_string(),
                serde_json::Value::String(team.to_string()),
            );
        }
        let body = serde_json::Value::Object(body);
        self.patch(&path, &body).await
    }

    /// Unregister an agent
    pub async fn unregister_agent(&self, name: &str) -> Result<(), ClientError> {
        let path = format!("/agents/{}", name);
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(map_http_error)?;

        self.handle_response(response).await?;
        Ok(())
    }

    /// Execute an agent (stateless cold-start)
    ///
    /// ADR-013: Agent cold-starts, executes, and exits
    pub async fn execute_agent(
        &self,
        name: &str,
        session_id: &str,
        message: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecuteAgentResponse, ClientError> {
        let path = format!("/agents/{}/execute", name);
        let body = serde_json::json!({
            "session_id": session_id,
            "message": message,
            "timeout_secs": timeout_secs,
        });
        self.post(&path, &body).await
    }

    // Legacy method aliases (deprecated)
    #[deprecated(since = "0.2.0", note = "Use list_agents instead")]
    pub async fn list_instances(
        &self,
        _status: Option<&str>,
        team_id: Option<&str>,
    ) -> Result<PaginatedAgentsResponse, ClientError> {
        self.list_agents(team_id).await
    }

    #[deprecated(since = "0.2.0", note = "Use register_agent instead")]
    pub async fn create_instance(
        &self,
        image: &str,
        name: Option<&str>,
        team_id: Option<&str>,
        env: Option<serde_json::Map<String, serde_json::Value>>,
        _auto_start: bool,
    ) -> Result<AgentConfigResponse, ClientError> {
        self.register_agent(image, name, team_id, env).await
    }

    #[deprecated(since = "0.2.0", note = "Use get_agent instead")]
    pub async fn get_instance(&self, id: &str) -> Result<AgentConfigResponse, ClientError> {
        self.get_agent(id).await
    }

    #[deprecated(since = "0.2.0", note = "Instances are stateless - no stop needed")]
    pub async fn stop_instance(
        &self,
        _id: &str,
        _force: bool,
        _timeout: u32,
    ) -> Result<AgentConfigResponse, ClientError> {
        Err(ClientError::InvalidResponse(
            "Stop instance is deprecated in stateless architecture".to_string(),
        ))
    }

    #[deprecated(since = "0.2.0", note = "Use unregister_agent instead")]
    pub async fn delete_instance(&self, id: &str, _purge: bool) -> Result<(), ClientError> {
        self.unregister_agent(id).await
    }

    // =================================================================================
    // Session Endpoints
    // =================================================================================

    /// List sessions for an instance
    pub async fn list_sessions(
        &self,
        instance_id: &str,
    ) -> Result<PaginatedSessionsResponse, ClientError> {
        let path = format!("/agents/{}/sessions", instance_id);
        self.get(&path).await
    }

    /// Get a session by ID
    pub async fn get_session(
        &self,
        instance_id: &str,
        session_id: &str,
    ) -> Result<SessionResponse, ClientError> {
        let path = format!("/agents/{}/sessions/{}", instance_id, session_id);
        self.get(&path).await
    }

    /// Get session history
    pub async fn get_session_history(
        &self,
        instance_id: &str,
        session_id: &str,
        include_tool_calls: bool,
        include_thinking: bool,
    ) -> Result<HistoryResponse, ClientError> {
        let path = format!("/agents/{}/sessions/{}/history", instance_id, session_id);
        let query = vec![
            ("include_tool_calls", include_tool_calls.to_string()),
            ("include_thinking", include_thinking.to_string()),
        ];
        self.get_with_query(&path, &query).await
    }

    /// Branch a session
    pub async fn branch_session(
        &self,
        instance_id: &str,
        session_id: &str,
        label: Option<&str>,
    ) -> Result<BranchResponse, ClientError> {
        let path = format!("/agents/{}/sessions/{}/branch", instance_id, session_id);
        let body = serde_json::json!({
            "label": label,
        });
        self.post(&path, &body).await
    }

    /// Delete a session
    pub async fn delete_session(
        &self,
        instance_id: &str,
        session_id: &str,
    ) -> Result<(), ClientError> {
        let path = format!("/agents/{}/sessions/{}", instance_id, session_id);
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(map_http_error)?;

        self.handle_response(response).await?;
        Ok(())
    }

    // =================================================================================
    // Team Endpoints
    // =================================================================================

    /// List all teams
    pub async fn list_teams(&self) -> Result<PaginatedTeamsResponse, ClientError> {
        self.get("/teams").await
    }

    /// Create a new team
    pub async fn create_team(&self, config: &str) -> Result<TeamResponse, ClientError> {
        let body = serde_json::json!({
            "config": config,
        });
        self.post("/teams", &body).await
    }

    /// Get a team by ID
    pub async fn get_team(&self, id: &str) -> Result<TeamResponse, ClientError> {
        let path = format!("/teams/{}", id);
        self.get(&path).await
    }

    /// Delete a team
    pub async fn delete_team(&self, id: &str) -> Result<(), ClientError> {
        let path = format!("/teams/{}", id);
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(map_http_error)?;

        self.handle_response(response).await?;
        Ok(())
    }

    /// Scale a team
    pub async fn scale_team(
        &self,
        team_id: &str,
        agent_name: &str,
        count: u32,
    ) -> Result<TeamResponse, ClientError> {
        let path = format!("/teams/{}/scale", team_id);
        let body = serde_json::json!({
            "agent": agent_name,
            "count": count,
        });
        self.post(&path, &body).await
    }

    // =================================================================================
    // Image Endpoints
    // =================================================================================

    /// List all images
    pub async fn list_images(&self) -> Result<PaginatedImagesResponse, ClientError> {
        self.get("/images").await
    }

    /// Build an image from a directory
    pub async fn build_image(
        &self,
        path: &str,
        tag: Option<&str>,
    ) -> Result<ImageResponse, ClientError> {
        let body = serde_json::json!({
            "path": path,
            "tag": tag,
        });
        self.post("/images/build", &body).await
    }

    /// Pull an image from a registry
    pub async fn pull_image(&self, image: &str) -> Result<ImageResponse, ClientError> {
        let body = serde_json::json!({
            "image": image,
        });
        self.post("/images/pull", &body).await
    }

    /// Push an image to a registry
    pub async fn push_image(
        &self,
        image: &str,
        registry: &str,
    ) -> Result<ImageResponse, ClientError> {
        let body = serde_json::json!({
            "image": image,
            "registry": registry,
        });
        self.post("/images/push", &body).await
    }

    // =================================================================================
    // Internal HTTP Methods
    // =================================================================================

    /// Perform a GET request
    async fn get<T>(&self, path: &str) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url).send().await.map_err(map_http_error)?;

        self.parse_response(response).await
    }

    /// Perform a GET request with query parameters
    async fn get_with_query<T, K, V>(&self, path: &str, query: &[(K, V)]) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .query(
                &query
                    .iter()
                    .map(|(k, v)| (k.as_ref(), v.as_ref()))
                    .collect::<Vec<_>>(),
            )
            .send()
            .await
            .map_err(map_http_error)?;

        self.parse_response(response).await
    }

    /// Perform a POST request
    async fn post<T, B>(&self, path: &str, body: &B) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
        B: Serialize,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(map_http_error)?;

        self.parse_response(response).await
    }

    /// Perform a PATCH request
    async fn patch<T, B>(&self, path: &str, body: &B) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
        B: Serialize,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .patch(&url)
            .json(body)
            .send()
            .await
            .map_err(map_http_error)?;

        self.parse_response(response).await
    }

    /// Handle a response (for empty bodies)
    async fn handle_response(&self, response: Response) -> Result<(), ClientError> {
        let status = response.status();

        if status.is_success() {
            Ok(())
        } else {
            Err(self.parse_error(response).await)
        }
    }

    /// Parse a successful response
    async fn parse_response<T>(&self, response: Response) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
    {
        let status = response.status();

        if status.is_success() {
            let body = response.json::<T>().await.map_err(|e| {
                ClientError::InvalidResponse(format!("Failed to parse JSON: {}", e))
            })?;
            Ok(body)
        } else {
            Err(self.parse_error(response).await)
        }
    }

    /// Parse an error response
    async fn parse_error(&self, response: Response) -> ClientError {
        let status = response.status();

        // Try to parse as API error
        match response.json::<ErrorResponse>().await {
            Ok(error) => ClientError::Api {
                status,
                code: error.error.code,
                message: error.error.message,
                request_id: error.error.request_id,
            },
            Err(_) => ClientError::Api {
                status,
                code: "unknown_error".to_string(),
                message: format!("HTTP {}", status),
                request_id: "unknown".to_string(),
            },
        }
    }
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new().expect("Failed to create API client")
    }
}

/// Map HTTP errors to client errors
fn map_http_error(e: reqwest::Error) -> ClientError {
    if e.is_connect() || e.is_timeout() {
        ClientError::DaemonNotRunning {
            addr: std::env::var(DAEMON_ADDR_ENV)
                .unwrap_or_else(|_| DEFAULT_DAEMON_ADDR.to_string()),
        }
    } else {
        ClientError::Http(e)
    }
}

// =================================================================================
// Response Types (mirrors API types for deserialization)
// =================================================================================

/// Paginated response wrapper
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaginatedResponse<T> {
    /// Items in this page
    pub items: Vec<T>,
    /// Cursor for the next page
    pub cursor: Option<String>,
    /// Whether more items are available
    pub has_more: bool,
}

/// Agent configuration response (stateless model)
///
/// ADR-013: Agents have no persistent runtime state.
/// They are configurations that cold-start on each request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentConfigResponse {
    /// Agent name (unique identifier)
    pub name: String,
    /// Source image reference
    pub image_ref: String,
    /// Pinned image digest
    pub image_digest: String,
    /// Team ID (if assigned)
    #[serde(default)]
    pub team_id: Option<String>,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Registration timestamp
    pub registered_at: String,
    /// Last updated timestamp
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Paginated agents response (replaces PaginatedInstancesResponse)
pub type PaginatedAgentsResponse = PaginatedResponse<AgentConfigResponse>;

/// Tool call in execute response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResponse {
    pub id: String,
    pub tool: String,
    #[serde(rename = "args")]
    pub args: serde_json::Value,
    pub output: String,
}

/// Token usage in execute response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TokenUsageResponse {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Execute agent response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecuteAgentResponse {
    pub execution_id: String,
    pub response: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallResponse>,
    pub usage: TokenUsageResponse,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

// Legacy type aliases for backward compatibility (deprecated)
#[deprecated(since = "0.2.0", note = "Use AgentConfigResponse instead")]
pub type InstanceResponse = AgentConfigResponse;
#[deprecated(since = "0.2.0", note = "Use PaginatedAgentsResponse instead")]
pub type PaginatedInstancesResponse = PaginatedAgentsResponse;

/// Session response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionResponse {
    pub id: String,
    /// Agent name (replaces instance_id in stateless model)
    #[serde(rename = "agent_name")]
    pub agent_name: String,
    /// Legacy field name (deprecated)
    #[serde(rename = "instance_id", default)]
    #[deprecated(since = "0.2.0", note = "Use agent_name instead")]
    pub _instance_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub turn_count: u32,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Paginated sessions response
pub type PaginatedSessionsResponse = PaginatedResponse<SessionResponse>;

/// History event response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEventResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: String,
}

/// History response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryResponse {
    pub session_id: String,
    pub items: Vec<HistoryEventResponse>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// Branch response
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BranchResponse {
    #[serde(flatten)]
    pub session: SessionResponse,
    pub parent_session_id: String,
}

/// Team response
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TeamResponse {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub config_path: Option<String>,
    pub created_at: String,
    pub agent_count: u32,
    #[serde(default)]
    pub instance_ids: Vec<String>,
}

/// Paginated teams response
pub type PaginatedTeamsResponse = PaginatedResponse<TeamResponse>;

/// Image response
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImageResponse {
    pub id: String,
    pub r#ref: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub size_bytes: u64,
    pub created_at: String,
    #[serde(default)]
    pub pulled_at: Option<String>,
    pub source: String,
}

/// Paginated images response
pub type PaginatedImagesResponse = PaginatedResponse<ImageResponse>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_error_exit_codes() {
        // Test daemon not running error
        let err = ClientError::DaemonNotRunning {
            addr: "http://localhost:11435".to_string(),
        };
        assert_eq!(err.exit_code(), 1);

        // Test not found error
        let err = ClientError::NotFound {
            resource_type: "instance".to_string(),
            resource_id: "inst_123".to_string(),
        };
        assert_eq!(err.exit_code(), 5);

        // Test API errors with different status codes
        let err = ClientError::Api {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request".to_string(),
            message: "Invalid input".to_string(),
            request_id: "req_123".to_string(),
        };
        assert_eq!(err.exit_code(), 2);

        let err = ClientError::Api {
            status: StatusCode::NOT_FOUND,
            code: "not_found".to_string(),
            message: "Not found".to_string(),
            request_id: "req_123".to_string(),
        };
        assert_eq!(err.exit_code(), 5);

        let err = ClientError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error".to_string(),
            message: "Server error".to_string(),
            request_id: "req_123".to_string(),
        };
        assert_eq!(err.exit_code(), 7);
    }

    #[test]
    fn test_client_error_display() {
        let err = ClientError::DaemonNotRunning {
            addr: "http://localhost:11435".to_string(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("Daemon not running"));
        assert!(msg.contains("localhost:11435"));
    }

    #[test]
    fn test_default_daemon_addr() {
        assert_eq!(DEFAULT_DAEMON_ADDR, "http://127.0.0.1:11435");
    }

    #[test]
    fn test_paginated_response_structure() {
        let response: PaginatedResponse<String> = PaginatedResponse {
            items: vec!["item1".to_string(), "item2".to_string()],
            cursor: Some("next_page".to_string()),
            has_more: true,
        };

        assert_eq!(response.items.len(), 2);
        assert_eq!(response.cursor, Some("next_page".to_string()));
        assert!(response.has_more);
    }

    #[test]
    fn test_agent_config_response_serialization() {
        // Test agent config response (stateless model)
        let agent = AgentConfigResponse {
            name: "test-agent".to_string(),
            image_ref: "test:v1.0".to_string(),
            image_digest: "sha256:abc123".to_string(),
            team_id: Some("team_456".to_string()),
            capabilities: vec!["chat".to_string(), "search".to_string()],
            registered_at: "2026-03-17T10:00:00Z".to_string(),
            updated_at: Some("2026-03-17T11:00:00Z".to_string()),
        };

        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("test-agent"));
        assert!(json.contains("sha256:abc123"));

        // Test deserialization
        let json = r#"{
            "name": "test-agent",
            "image_ref": "test:v1.0",
            "image_digest": "sha256:abc123",
            "capabilities": ["chat"],
            "registered_at": "2026-03-17T10:00:00Z"
        }"#;

        let agent: AgentConfigResponse = serde_json::from_str(json).unwrap();
        assert_eq!(agent.name, "test-agent");
        assert_eq!(agent.image_digest, "sha256:abc123");
        assert_eq!(agent.capabilities, vec!["chat"]);
    }

    #[test]
    fn test_session_response_serialization() {
        let session = SessionResponse {
            id: "sess_123".to_string(),
            agent_name: "test-agent".to_string(),
            #[allow(deprecated)]
            _instance_id: None,
            created_at: "2026-03-17T10:00:00Z".to_string(),
            updated_at: "2026-03-17T10:05:00Z".to_string(),
            turn_count: 5,
            parent_session_id: Some("sess_parent".to_string()),
            title: Some("Test Session".to_string()),
        };

        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("sess_123"));
        assert!(json.contains("test-agent"));
        assert!(json.contains("Test Session"));
        assert!(json.contains("sess_parent"));
    }

    #[test]
    fn test_history_event_response() {
        let event = HistoryEventResponse {
            id: "evt_123".to_string(),
            event_type: "user.message".to_string(),
            role: Some("user".to_string()),
            content: Some("Hello".to_string()),
            tool: None,
            args: None,
            tool_call_id: None,
            output: None,
            error: None,
            created_at: "2026-03-17T10:00:00Z".to_string(),
        };

        assert_eq!(event.event_type, "user.message");
        assert_eq!(event.role, Some("user".to_string()));
    }

    #[test]
    fn test_history_response() {
        let response = HistoryResponse {
            session_id: "sess_123".to_string(),
            items: vec![HistoryEventResponse {
                id: "evt_1".to_string(),
                event_type: "user.message".to_string(),
                role: Some("user".to_string()),
                content: Some("Hello".to_string()),
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: "2026-03-17T10:00:00Z".to_string(),
            }],
            cursor: None,
            has_more: false,
        };

        assert_eq!(response.session_id, "sess_123");
        assert_eq!(response.items.len(), 1);
        assert!(!response.has_more);
    }

    #[test]
    fn test_branch_response() {
        let session = SessionResponse {
            id: "sess_child".to_string(),
            agent_name: "test-agent".to_string(),
            #[allow(deprecated)]
            _instance_id: None,
            created_at: "2026-03-17T10:00:00Z".to_string(),
            updated_at: "2026-03-17T10:00:00Z".to_string(),
            turn_count: 0,
            parent_session_id: Some("sess_parent".to_string()),
            title: None,
        };

        let branch = BranchResponse {
            session,
            parent_session_id: "sess_parent".to_string(),
        };

        assert_eq!(branch.session.id, "sess_child");
        assert_eq!(branch.parent_session_id, "sess_parent");
    }
}
