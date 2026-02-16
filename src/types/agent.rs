//! Agent configuration and state types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier (DID will be generated from this)
    pub name: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Tenant/organization this agent belongs to
    pub tenant: Option<String>,
    /// Agent capabilities
    pub capabilities: Vec<AgentCapability>,
    /// LLM provider configuration
    pub provider: super::provider::ProviderConfig,
    /// Memory configuration
    pub memory: Option<super::memory::MemoryConfig>,
    /// Tool configurations
    pub tools: Option<ToolConfig>,
    /// Channel configurations
    pub channels: Option<ChannelConfig>,
    /// Auto-accept quotes (for trusted agents)
    pub auto_accept_trusted: bool,
    /// Require human approval for contracts above this amount
    pub approval_threshold: Option<f64>,
    /// Default timeout for tasks (seconds)
    pub default_timeout_seconds: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "unnamed-agent".to_string(),
            description: None,
            tenant: None,
            capabilities: vec![],
            provider: super::provider::ProviderConfig::default(),
            memory: None,
            tools: None,
            channels: None,
            auto_accept_trusted: false,
            approval_threshold: Some(100.0),
            default_timeout_seconds: 300,
        }
    }
}

/// Agent capability definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Unique capability name
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Input parameters
    pub parameters: Vec<CapabilityParameter>,
    /// Required authentication scopes
    pub required_auth: Vec<String>,
    /// Estimated cost per invocation (if applicable)
    pub estimated_cost: Option<f64>,
    /// Estimated duration
    pub estimated_duration: Option<String>,
}

/// Capability parameter definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityParameter {
    /// Parameter name
    pub name: String,
    /// Parameter type (string, number, boolean, object, array)
    pub param_type: String,
    /// Human-readable description
    pub description: String,
    /// Is this parameter required?
    pub required: bool,
    /// Default value (JSON)
    pub default: Option<serde_json::Value>,
    /// JSON Schema for validation
    pub schema: Option<serde_json::Value>,
}

/// Agent runtime state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentState {
    /// Agent is initializing
    #[serde(rename = "initializing")]
    Initializing,
    /// Agent is idle and ready
    #[serde(rename = "idle")]
    Idle,
    /// Agent is processing a task
    #[serde(rename = "busy")]
    Busy,
    /// Agent is paused
    #[serde(rename = "paused")]
    Paused,
    /// Agent encountered an error
    #[serde(rename = "error")]
    Error,
    /// Agent is shutting down
    #[serde(rename = "shutting_down")]
    ShuttingDown,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Initializing => write!(f, "initializing"),
            AgentState::Idle => write!(f, "idle"),
            AgentState::Busy => write!(f, "busy"),
            AgentState::Paused => write!(f, "paused"),
            AgentState::Error => write!(f, "error"),
            AgentState::ShuttingDown => write!(f, "shutting_down"),
        }
    }
}

/// Tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Enabled tools
    pub enabled: Vec<String>,
    /// HTTP tool settings
    pub http: Option<HttpToolConfig>,
    /// Memory tool settings
    pub memory: Option<MemoryToolConfig>,
    /// Custom tool definitions
    pub custom: Option<HashMap<String, serde_json::Value>>,
}

/// HTTP tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpToolConfig {
    /// Allowed domains (empty = all allowed)
    pub allowed_domains: Vec<String>,
    /// Blocked domains
    pub blocked_domains: Vec<String>,
    /// Maximum request size (bytes)
    pub max_request_size: usize,
    /// Timeout seconds
    pub timeout_seconds: u64,
    /// Require HTTPS
    pub require_https: bool,
}

impl Default for HttpToolConfig {
    fn default() -> Self {
        Self {
            allowed_domains: vec![],
            blocked_domains: vec![],
            max_request_size: 10 * 1024 * 1024, // 10MB
            timeout_seconds: 30,
            require_https: true,
        }
    }
}

/// Memory tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryToolConfig {
    /// Maximum entries to return per query
    pub max_results: usize,
    /// Default search scope
    pub default_scope: String,
}

impl Default for MemoryToolConfig {
    fn default() -> Self {
        Self {
            max_results: 10,
            default_scope: "session".to_string(),
        }
    }
}

/// Channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// CLI channel enabled
    pub cli: bool,
    /// HTTP webhook configuration
    pub http: Option<HttpChannelConfig>,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            cli: true,
            http: None,
        }
    }
}

/// HTTP channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpChannelConfig {
    /// Bind address
    pub bind: String,
    /// Port
    pub port: u16,
    /// TLS configuration
    pub tls: Option<TlsConfig>,
    /// Webhook secret for verification
    pub webhook_secret: Option<String>,
}

/// TLS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Certificate file path
    pub cert_path: String,
    /// Key file path
    pub key_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.name, "unnamed-agent");
        assert_eq!(config.default_timeout_seconds, 300);
        assert!(!config.auto_accept_trusted);
    }

    #[test]
    fn test_agent_state_display() {
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Busy.to_string(), "busy");
        assert_eq!(AgentState::Error.to_string(), "error");
    }

    #[test]
    fn test_capability_serialization() {
        let cap = AgentCapability {
            name: "test-cap".to_string(),
            description: "Test capability".to_string(),
            parameters: vec![CapabilityParameter {
                name: "input".to_string(),
                param_type: "string".to_string(),
                description: "Input parameter".to_string(),
                required: true,
                default: None,
                schema: None,
            }],
            required_auth: vec!["read".to_string()],
            estimated_cost: None,
            estimated_duration: Some("1s".to_string()),
        };

        let json = serde_json::to_string(&cap).unwrap();
        assert!(json.contains("test-cap"));
        assert!(json.contains("Test capability"));
    }
}
