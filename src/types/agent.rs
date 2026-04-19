//! Agent configuration and state types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Configuration format version
    #[serde(default = "default_config_version")]
    pub version: String,
    /// Unique identifier (DID will be generated from this)
    pub name: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Team this agent belongs to (replaces tenant for team-based organization)
    #[serde(default)]
    pub team: Option<String>,
    /// Tenant/organization this agent belongs to (legacy, use team)
    pub tenant: Option<String>,
    /// Agent capabilities
    pub capabilities: Vec<AgentCapability>,
    /// LLM provider configuration
    pub provider: super::provider::ProviderConfig,
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

fn default_config_version() -> String {
    "1.0".to_string()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            name: "unnamed-agent".to_string(),
            description: None,
            team: None,
            tenant: None,
            capabilities: vec![],
            provider: super::provider::ProviderConfig::default(),
            tools: Some(ToolConfig::default()), // Default tool whitelist
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptConfig {
    /// System file configuration (formerly bootstrap)
    #[serde(alias = "bootstrap")] // Backward compatibility
    pub system: Option<SystemFileConfig>,
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

/// System file configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemFileConfig {
    /// Maximum characters per file
    #[serde(default = "default_max_chars")]
    pub max_chars_per_file: usize,
    /// Custom system files to load
    pub files: Option<Vec<String>>,
}

/// Deprecated: Use `SystemFileConfig` instead
pub type BootstrapFileConfig = SystemFileConfig;

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

/// Per-tool settings for granular control
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSettings {
    /// Whether this tool is enabled
    pub enabled: bool,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    /// Enabled built-in tools (whitelist)
    /// If empty, NO tools are allowed (secure by default)
    pub enabled: Vec<String>,
    /// Enabled skills for this agent (whitelist)
    pub skills: Vec<String>,
    /// HTTP tool settings
    pub http: Option<HttpToolConfig>,
    /// Custom tool definitions
    pub custom: Option<HashMap<String, serde_json::Value>>,
    /// Per-tool settings (`snake_case` keys like "`str_replace_file`", "`write_file`", etc.)
    #[serde(default)]
    pub read_file: Option<ToolSettings>,
    #[serde(default)]
    pub write_file: Option<ToolSettings>,
    #[serde(default)]
    pub glob: Option<ToolSettings>,
    #[serde(default)]
    pub grep: Option<ToolSettings>,
    #[serde(default)]
    pub str_replace_file: Option<ToolSettings>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            // Default: enable all common built-in tools so agents work out of the box
            enabled: vec![
                "shell".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
                "glob".to_string(),
                "grep".to_string(),
                "str_replace_file".to_string(),
            ],
            skills: vec![],
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            str_replace_file: None,
        }
    }
}

impl ToolConfig {
    /// Check if a tool is enabled according to the whitelist
    ///
    /// - If whitelist has entries: only listed tools are allowed (unless per-tool enabled=true)
    /// - If whitelist is empty: NO tools allowed (secure by default)
    /// - Supports wildcard patterns like "mcp:identity:*" which matches full tool names
    ///
    /// Tools must be referenced by their full names (e.g., "`mcp:identity:echo_identity`")
    /// for consistent identification across extension types.
    #[must_use] 
    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        // Check if tool is in the whitelist (case-insensitive, supports wildcards)
        let in_whitelist = self.enabled.iter().any(|pattern| {
            // Direct match (case-insensitive)
            if pattern.eq_ignore_ascii_case(tool_name) {
                return true;
            }

            // Support wildcard patterns like "mcp:identity:*"
            // This matches full names like "mcp:identity:echo_identity"
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                return tool_name.to_lowercase().starts_with(&prefix.to_lowercase());
            }
            false
        });

        // Get per-tool settings if they exist
        let per_tool_enabled = self
            .get_tool_settings(tool_name)
            .is_none_or(|s| s.enabled);

        // Tool is enabled if it's in the whitelist AND not explicitly disabled via per-tool config
        // OR if it's explicitly enabled via per-tool config (even if not in whitelist)
        in_whitelist && per_tool_enabled
    }

    /// Get per-tool settings for a specific tool (by `snake_case` name)
    #[must_use] 
    pub fn get_tool_settings(&self, tool_name: &str) -> Option<&ToolSettings> {
        match tool_name {
            "read_file" => self.read_file.as_ref(),
            "write_file" => self.write_file.as_ref(),
            "glob" => self.glob.as_ref(),
            "grep" => self.grep.as_ref(),
            "str_replace_file" => self.str_replace_file.as_ref(),
            _ => None,
        }
    }
}


/// HTTP tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

/// Channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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

    #[test]
    fn test_tool_config_default_whitelist() {
        let config = ToolConfig::default();

        // Default whitelist is empty (secure-by-default)
        assert!(config.enabled.is_empty());
    }

    #[test]
    fn test_tool_config_is_tool_enabled() {
        let config = ToolConfig {
            enabled: vec![
                "shell".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
                "glob".to_string(),
                "grep".to_string(),
                "str_replace_file".to_string(),
                "sessions_list".to_string(),
                "sessions_history".to_string(),
                "session_status".to_string(),
                "cron".to_string(),
            ],
            ..Default::default()
        };

        // All whitelisted tools should be enabled
        assert!(config.is_tool_enabled("shell"));
        assert!(config.is_tool_enabled("read_file"));
        assert!(config.is_tool_enabled("write_file"));
        assert!(config.is_tool_enabled("glob"));
        assert!(config.is_tool_enabled("grep"));
        assert!(config.is_tool_enabled("str_replace_file"));
        assert!(config.is_tool_enabled("sessions_list"));
        assert!(config.is_tool_enabled("sessions_history"));
        assert!(config.is_tool_enabled("session_status"));
        assert!(config.is_tool_enabled("cron"));

        // Case-insensitive matching
        assert!(config.is_tool_enabled("SHELL"));
        assert!(config.is_tool_enabled("Session_Status"));

        // Unknown tools should not be enabled
        assert!(!config.is_tool_enabled("unknown_tool"));
    }

    #[test]
    fn test_tool_config_empty_whitelist_disallows_all() {
        // Empty whitelist should disallow ALL tools (secure by default)
        let config = ToolConfig {
            enabled: vec![],
            skills: Vec::new(),
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            str_replace_file: None,
        };

        assert!(!config.is_tool_enabled("shell"));
        assert!(!config.is_tool_enabled("read_file"));
        assert!(!config.is_tool_enabled("any_tool"));
    }

    #[test]
    fn test_tool_config_custom_whitelist() {
        // Custom whitelist
        let config = ToolConfig {
            enabled: vec!["read_file".to_string(), "write_file".to_string()],
            skills: Vec::new(),
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            str_replace_file: None,
        };

        assert!(config.is_tool_enabled("read_file"));
        assert!(config.is_tool_enabled("write_file"));
        assert!(!config.is_tool_enabled("shell"));
    }

    #[test]
    fn test_agent_config_has_default_tools() {
        let config = AgentConfig::default();

        // Default tool config exists but whitelist is empty (secure-by-default)
        assert!(config.tools.is_some());
        let tools = config.tools.unwrap();
        assert!(tools.enabled.is_empty());
    }

    #[test]
    fn test_tool_config_toml_serialization() {
        let config = ToolConfig {
            enabled: vec!["shell".to_string(), "read_file".to_string()],
            ..Default::default()
        };
        let toml = toml::to_string(&config).unwrap();

        // Should contain the enabled list
        assert!(toml.contains("enabled"));
        assert!(toml.contains("shell"));
        assert!(toml.contains("read_file"));
    }
}
