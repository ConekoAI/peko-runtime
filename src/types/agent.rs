//! Agent configuration and state types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// Workspace directory for bootstrap files
    pub workspace: Option<PathBuf>,
    /// System prompt configuration
    pub prompt: Option<PromptConfig>,
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
            workspace: None,
            prompt: None,
        }
    }
}

/// Prompt configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    /// Prompt mode: full, minimal, none
    #[serde(default)]
    pub mode: PromptMode,
    /// Custom system prompt override (replaces default)
    pub custom_prompt: Option<String>,
    /// Additional prompt sections to inject
    #[serde(default)]
    pub extra_sections: Vec<String>,
    /// Bootstrap file configuration
    pub bootstrap: Option<BootstrapFileConfig>,
}

/// Prompt mode - controls which sections are included
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// All sections (default for main sessions)
    #[default]
    Full,
    /// Reduced sections (for sub-agents)
    Minimal,
    /// Base identity only
    None,
}

impl PromptMode {
    /// Parse from string
    #[must_use] 
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "minimal" => Self::Minimal,
            "none" => Self::None,
            _ => Self::Full,
        }
    }
}

/// Bootstrap file configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapFileConfig {
    /// Maximum characters per file
    #[serde(default = "default_max_chars")]
    pub max_chars_per_file: usize,
    /// Custom bootstrap files to load
    pub files: Option<Vec<String>>,
}

fn default_max_chars() -> usize {
    20_000
}

/// Agent capability definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Unique capability name
    pub name: String,
    /// Capability version (semver)
    #[serde(default = "default_version")]
    pub version: String,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Input parameters (legacy field)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<CapabilityParameter>>,
    /// Required authentication scopes (legacy field)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_auth: Option<Vec<String>>,
    /// Estimated cost per invocation (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    /// Estimated duration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_duration: Option<String>,
}

fn default_version() -> String {
    "1.0".to_string()
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

/// Agent runtime state - simplified to Idle/Busy
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentState {
    /// Agent is idle and ready for work
    #[serde(rename = "idle")]
    Idle,
    /// Agent is busy processing a task
    #[serde(rename = "busy")]
    Busy,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "idle"),
            AgentState::Busy => write!(f, "busy"),
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
    }

    #[test]
    fn test_capability_serialization() {
        let cap = AgentCapability {
            name: "test-cap".to_string(),
            version: "1.0".to_string(),
            description: Some("Test capability".to_string()),
            parameters: Some(vec![CapabilityParameter {
                name: "input".to_string(),
                param_type: "string".to_string(),
                description: "Input parameter".to_string(),
                required: true,
                default: None,
                schema: None,
            }]),
            required_auth: Some(vec!["read".to_string()]),
            estimated_cost: None,
            estimated_duration: Some("1s".to_string()),
        };

        let json = serde_json::to_string(&cap).unwrap();
        assert!(json.contains("test-cap"));
        assert!(json.contains("Test capability"));
    }
}
