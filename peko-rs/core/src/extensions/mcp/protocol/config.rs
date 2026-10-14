//! MCP authentication configuration.
//!
//! `McpAuthConfig` is the bearer/OAuth shape consumed by `SseTransport` and
//! `OAuthFlow`; the principal-owned service (ADR-075) builds it from its own
//! `Definition`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Authentication configuration for an MCP server connection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpAuthConfig {
    /// Static bearer token sent as `Authorization: Bearer <token>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    /// OAuth client identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    /// OAuth authorization endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_endpoint: Option<String>,
    /// OAuth token endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    /// OAuth scopes to request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Additional static headers to send with every request.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

impl McpAuthConfig {
    /// Returns true when no auth fields are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bearer_token.is_none()
            && self.oauth_client_id.is_none()
            && self.authorization_endpoint.is_none()
            && self.token_endpoint.is_none()
            && self.scopes.is_empty()
            && self.headers.is_empty()
    }
}
