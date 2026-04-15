//! API Request and Response Types
//!
//! This module defines the data structures used in API requests and responses.
//! All types implement Serialize/Deserialize for JSON encoding.

use serde::{Deserialize, Serialize};

// =============================================================================
// Health Endpoint
// =============================================================================

/// Response from the health check endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Service status: "ok" or "degraded"
    pub status: String,
    /// Pekobot version string
    pub version: String,
    /// Daemon uptime in seconds
    pub uptime_seconds: u64,
    /// Number of running instances
    pub instance_count: u64,
    /// Number of deployed teams
    pub team_count: u64,
}

impl HealthResponse {
    /// Create a healthy response
    pub fn healthy(version: impl Into<String>, uptime_seconds: u64) -> Self {
        Self {
            status: "ok".to_string(),
            version: version.into(),
            uptime_seconds,
            instance_count: 0,
            team_count: 0,
        }
    }

    /// Create a degraded response
    pub fn degraded(version: impl Into<String>, uptime_seconds: u64) -> Self {
        Self {
            status: "degraded".to_string(),
            version: version.into(),
            uptime_seconds,
            instance_count: 0,
            team_count: 0,
        }
    }
}

// =============================================================================
// Info Endpoint
// =============================================================================

/// Response from the daemon info endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoResponse {
    /// Pekobot version
    pub version: String,
    /// API version
    pub api_version: String,
    /// Path to workspace directory
    pub workspace: String,
    /// Port the daemon is listening on
    pub port: u16,
    /// Process ID
    pub pid: u32,
    /// Platform string (e.g., "linux-x86_64")
    pub platform: String,
    /// Available capabilities
    pub capabilities: CapabilitiesInfo,
}

/// Capability flags returned by the info endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesInfo {
    /// Streaming responses supported
    pub streaming: bool,
    /// WebSocket connections supported
    pub websocket: bool,
    /// Teams/multi-agent support
    pub teams: bool,
}

impl Default for CapabilitiesInfo {
    fn default() -> Self {
        Self {
            streaming: true,
            websocket: true,
            teams: true,
        }
    }
}

// =============================================================================
// Error Responses
// =============================================================================

/// Standard error response envelope
///
/// All API errors follow this structure for consistency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error details
    pub error: ErrorDetail,
}

/// Detailed error information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Error code (machine-readable)
    pub code: String,
    /// Human-readable error message
    pub message: String,
    /// Request ID for tracing
    pub request_id: String,
    /// Additional structured details (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ErrorResponse {
    /// Create a new error response
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
                request_id: request_id.into(),
                details: None,
            },
        }
    }

    /// Add details to the error response
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.error.details = Some(details);
        self
    }
}

// =============================================================================
// Session Types
// =============================================================================

/// Session response object (API_CONTRACT §2.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub id: String,
    /// Agent name (replaces instance_id in stateless model)
    #[serde(rename = "agent_name")]
    pub agent_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub turn_count: u32,
    #[serde(default)]
    pub message_count: usize,
    /// Current context window size (total_tokens from last assistant message)
    #[serde(default)]
    pub context_window: usize,
    /// Cumulative input tokens across all assistant messages
    #[serde(default)]
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    #[serde(default)]
    pub total_output_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,
}

/// History event response (API_CONTRACT §5.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEventResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    pub created_at: String,
}

/// History response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub session_id: String,
    pub items: Vec<HistoryEventResponse>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// Branch response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchResponse {
    #[serde(flatten)]
    pub session: SessionResponse,
    pub parent_session_id: String,
}

// =============================================================================
// Common Request Types
// =============================================================================

/// Pagination parameters for list endpoints
#[derive(Debug, Clone, Deserialize)]
pub struct PaginationParams {
    /// Maximum number of items to return
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination (simple cursor)
    #[serde(default)]
    pub offset: usize,
    /// Pagination cursor (alternative to offset)
    pub cursor: Option<String>,
}

impl PaginationParams {
    /// Get the offset value
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Get the limit value (capped at 100)
    pub fn limit(&self) -> usize {
        self.limit.min(100)
    }
}

fn default_limit() -> usize {
    20
}

/// Standard pagination wrapper for list responses
#[derive(Debug, Clone, Serialize)]
pub struct PaginatedResponse<T> {
    /// Items in this page
    pub items: Vec<T>,
    /// Cursor for the next page (null if no more items)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Whether more items are available
    pub has_more: bool,
}

impl<T> PaginatedResponse<T> {
    /// Create a new paginated response
    pub fn new(items: Vec<T>, has_more: bool) -> Self {
        Self {
            items,
            cursor: None,
            has_more,
        }
    }

    /// Add a cursor for the next page
    pub fn with_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }
}
