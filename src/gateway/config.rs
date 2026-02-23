//! Gateway configuration types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::gateway::types::GatewayMetadata;

/// Configuration for a gateway instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Unique name for this gateway instance
    /// (e.g., "my-discord", "work-slack")
    pub name: String,

    /// Which plugin to use
    /// (e.g., "discord", "whatsapp")
    pub plugin: String,

    /// Plugin-specific configuration
    /// (see plugin documentation for required fields)
    #[serde(flatten)]
    pub config: HashMap<String, serde_json::Value>,

    /// Whether this gateway is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Rate limiting configuration
    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    /// Retry configuration
    #[serde(default)]
    pub retry: RetryConfig,

    /// Message filtering rules
    #[serde(default)]
    pub filters: Vec<FilterRule>,
}

fn default_true() -> bool {
    true
}

/// Rate limiting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum messages per second
    #[serde(default = "default_rate_limit")]
    pub messages_per_second: f64,

    /// Burst capacity
    #[serde(default = "default_burst")]
    pub burst: u32,

    /// Cooldown period in milliseconds after hitting limit
    #[serde(default = "default_cooldown")]
    pub cooldown_ms: u64,
}

fn default_rate_limit() -> f64 {
    5.0 // 5 messages per second default
}

fn default_burst() -> u32 {
    10
}

fn default_cooldown() -> u64 {
    1000 // 1 second
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            messages_per_second: default_rate_limit(),
            burst: default_burst(),
            cooldown_ms: default_cooldown(),
        }
    }
}

/// Retry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retries
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Base delay in milliseconds
    #[serde(default = "default_base_delay")]
    pub base_delay_ms: u64,

    /// Maximum delay in milliseconds
    #[serde(default = "default_max_delay")]
    pub max_delay_ms: u64,

    /// Exponential backoff multiplier
    #[serde(default = "default_backoff")]
    pub backoff_multiplier: f64,
}

fn default_max_retries() -> u32 {
    3
}

fn default_base_delay() -> u64 {
    1000 // 1 second
}

fn default_max_delay() -> u64 {
    30000 // 30 seconds
}

fn default_backoff() -> f64 {
    2.0 // Double each retry
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay(),
            max_delay_ms: default_max_delay(),
            backoff_multiplier: default_backoff(),
        }
    }
}

/// Message filter rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterRule {
    /// Rule name (for logging)
    pub name: String,

    /// Condition to match
    pub condition: FilterCondition,

    /// Action to take
    pub action: FilterAction,
}

/// Filter condition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FilterCondition {
    /// Match specific user
    FromUser { user_id: String },
    /// Match specific channel
    InChannel { channel_id: String },
    /// Match message content pattern
    ContentContains { pattern: String },
    /// Match message prefix
    StartsWith { prefix: String },
    /// Match message suffix
    EndsWith { suffix: String },
    /// Match message type
    MessageType { msg_type: String },
    /// Negate another condition
    Not { inner: Box<FilterCondition> },
    /// Match all of multiple conditions
    All { conditions: Vec<FilterCondition> },
    /// Match any of multiple conditions
    Any { conditions: Vec<FilterCondition> },
}

/// Filter action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum FilterAction {
    /// Allow the message through
    Allow,
    /// Drop the message
    Drop,
    /// Drop and log
    DropWithLog,
    /// Forward to specific handler
    ForwardTo { handler: String },
    /// Add metadata tag
    Tag { key: String, value: String },
}

/// Global gateway configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaysConfig {
    /// List of configured gateways
    #[serde(default)]
    pub gateways: Vec<GatewayConfig>,

    /// Default rate limiting
    #[serde(default)]
    pub default_rate_limit: RateLimitConfig,

    /// Default retry configuration
    #[serde(default)]
    pub default_retry: RetryConfig,

    /// Plugin cache directory
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,

    /// Pekohub URL for downloading plugins
    #[serde(default = "default_pekohub_url")]
    pub pekohub_url: String,

    /// Auto-update plugins on startup
    #[serde(default)]
    pub auto_update: bool,

    /// Plugin verification (signatures)
    #[serde(default = "default_true")]
    pub verify_signatures: bool,
}

fn default_cache_dir() -> String {
    "~/.pekobot/gateways".to_string()
}

fn default_pekohub_url() -> String {
    "https://tools.coneko.ai".to_string()
}

impl Default for GatewaysConfig {
    fn default() -> Self {
        Self {
            gateways: Vec::new(),
            default_rate_limit: RateLimitConfig::default(),
            default_retry: RetryConfig::default(),
            cache_dir: default_cache_dir(),
            pekohub_url: default_pekohub_url(),
            auto_update: false,
            verify_signatures: true,
        }
    }
}

/// Plugin manifest (gateway.toml)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin metadata
    pub plugin: PluginInfo,

    /// Capabilities provided
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Binary downloads by platform
    pub binary: BinaryDownloads,

    /// Configuration schema
    #[serde(default)]
    pub config: ConfigSchema,
}

/// Plugin info section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Unique plugin name
    pub name: String,
    /// Plugin type (always "gateway")
    #[serde(rename = "type")]
    pub plugin_type: String,
    /// Semantic version
    pub version: String,
    /// Human-readable description
    pub description: String,
    /// Author/maintainer
    pub author: String,
    /// Minimum Pekobot version required
    pub min_pekobot_version: Option<String>,
    /// API version
    pub api_version: String,
}

/// Binary download URLs by platform
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDownloads {
    /// Linux x86_64
    #[serde(rename = "linux_x64")]
    pub linux_x64: String,
    /// Linux ARM64
    #[serde(rename = "linux_arm64")]
    pub linux_arm64: Option<String>,
    /// macOS x86_64
    #[serde(rename = "macos_x64")]
    pub macos_x64: Option<String>,
    /// macOS ARM64 (Apple Silicon)
    #[serde(rename = "macos_arm")]
    pub macos_arm: Option<String>,
    /// Windows x86_64
    #[serde(rename = "windows_x64")]
    pub windows_x64: Option<String>,
}

/// Configuration schema
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigSchema {
    /// Required configuration fields
    #[serde(default)]
    pub required: Vec<ConfigField>,
    /// Optional configuration fields
    #[serde(default)]
    pub optional: Vec<ConfigField>,
}

/// Configuration field definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    /// Field name
    pub name: String,
    /// Field type (string, number, boolean, etc.)
    pub field_type: String,
    /// Human-readable description
    pub description: String,
    /// Default value (for optional fields)
    pub default: Option<serde_json::Value>,
    /// Whether this is a secret (mask in logs)
    #[serde(default)]
    pub secret: bool,
}

/// Gateway info (for listing available)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayInfo {
    /// Plugin name
    pub name: String,
    /// Display name
    pub display_name: String,
    /// Version
    pub version: String,
    /// Description
    pub description: String,
    /// Author
    pub author: String,
    /// Whether it's installed locally
    pub installed: bool,
    /// Latest version available (if different from installed)
    pub latest_version: Option<String>,
}
