use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Per-extension settings for granular control
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionSettings {
    /// Whether this extension is enabled
    pub enabled: bool,
}

impl Default for ExtensionSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Extension configuration — unified config for all extension types
/// (tools, skills, MCP servers, universal tools, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtensionConfig {
    /// Enabled extensions (whitelist)
    /// If empty, NO extensions are allowed (secure by default)
    pub enabled: Vec<String>,
    /// HTTP tool settings
    pub http: Option<HttpToolConfig>,
    /// Custom tool definitions
    pub custom: Option<HashMap<String, serde_json::Value>>,
    /// Per-extension settings (snake_case keys like "str_replace_file", "write_file", etc.)
    #[serde(default)]
    pub read_file: Option<ExtensionSettings>,
    #[serde(default)]
    pub write_file: Option<ExtensionSettings>,
    #[serde(default)]
    pub glob: Option<ExtensionSettings>,
    #[serde(default)]
    pub grep: Option<ExtensionSettings>,
    #[serde(default)]
    pub str_replace_file: Option<ExtensionSettings>,
}

impl Default for ExtensionConfig {
    fn default() -> Self {
        Self {
            // Default: enable all common built-in tools so agents work out of the box.
            // Whitelist stores canonical extension IDs, not bare tool names.
            enabled: vec![
                "builtin:tool:shell".to_string(),
                "builtin:tool:Read".to_string(),
                "builtin:tool:write_file".to_string(),
                "builtin:tool:glob".to_string(),
                "builtin:tool:grep".to_string(),
                "builtin:tool:str_replace_file".to_string(),
                "builtin:tool:session".to_string(),
                "builtin:tool:cron".to_string(),
                "builtin:tool:agent_spawn".to_string(),
                "builtin:tool:task".to_string(),
            ],
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

impl ExtensionConfig {
    /// Check if an extension is enabled according to the whitelist.
    ///
    /// The whitelist stores **canonical extension IDs** (e.g. `builtin:tool:shell`,
    /// `mcp:standard-echo`, `universal:calculator`).  The caller is expected to
    /// pass the canonical `extension_id` of the extension that owns the tool.
    ///
    /// - If whitelist has entries: only listed extensions are allowed.
    /// - If whitelist is empty: NO extensions allowed (secure by default).
    /// - Supports wildcard patterns like `mcp:identity:*`.
    #[must_use]
    pub fn is_extension_enabled(&self, name: &str) -> bool {
        let in_whitelist = self.enabled.iter().any(|pattern| {
            // Direct match (case-insensitive)
            if pattern.eq_ignore_ascii_case(name) {
                return true;
            }

            // Wildcard match (e.g. "mcp:identity:*" matches "mcp:identity:echo")
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                return name.to_lowercase().starts_with(&prefix.to_lowercase());
            }

            false
        });

        // Get per-extension settings if they exist
        let per_extension_enabled = self.get_extension_settings(name).is_none_or(|s| s.enabled);

        in_whitelist && per_extension_enabled
    }

    /// Get per-extension settings for a specific extension.
    ///
    /// `name` is the canonical extension ID.  For built-in tools the last
    /// segment is the snake_case tool name.
    #[must_use]
    pub fn get_extension_settings(&self, name: &str) -> Option<&ExtensionSettings> {
        let bare = name.rsplit(':').next().unwrap_or(name);
        match bare {
            "Read" => self.read_file.as_ref(),
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

/// Tool configuration (deprecated — use `ExtensionConfig` instead)
/// Kept for backward compatibility during deserialization of old configs.
pub type ToolConfig = ExtensionConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.name, "unnamed-agent");
        assert_eq!(config.default_timeout_seconds, 300);
        assert!(!config.auto_accept_trusted);
        // Issue #28: `agent_did` is `None` by default — back-filled on
        // first `Agent::new()` and persisted into config.toml.
        assert!(config.agent_did.is_none());
    }

    /// Issue #28: `wire_agent_id` must return the DID when present
    /// (cross-runtime wire) and the local name as a fallback
    /// (single-runtime back-compat). The empty-DID guard is
    /// inherited from `Principal::agent_wire_id` (review of #34
    /// concern #3) and is pinned here so the shim doesn't drift.
    #[test]
    fn test_wire_agent_id_prefers_did_over_name() {
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some("did:peko:local:abc123".to_string());
        assert_eq!(config.wire_agent_id(), "did:peko:local:abc123");
    }

    #[test]
    fn test_wire_agent_id_falls_back_to_name_when_did_missing() {
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = None;
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_wire_agent_id_treats_empty_did_as_missing() {
        // Pin the empty-DID defense: a hand-edited config that left
        // `agent_did = ""` must NOT surface an empty string as the
        // wire id (would serialize as `agentDid: ""` over the
        // tunnel, breaking PekoHub's lookup).
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some(String::new());
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_agent_did_toml_round_trip() {
        // An empty `agent_did` round-trips as `None` (legacy config).
        let legacy = AgentConfig {
            name: "legacy-agent".to_string(),
            ..Default::default()
        };
        let toml = toml::to_string_pretty(&legacy).expect("serialize legacy");
        let parsed: AgentConfig = toml::from_str(&toml).expect("parse legacy");
        assert!(parsed.agent_did.is_none());
        assert_eq!(parsed.name, "legacy-agent");

        // A populated `agent_did` round-trips verbatim.
        let mut modern = AgentConfig::default();
        modern.name = "modern-agent".to_string();
        modern.agent_did = Some("did:peko:local:deadbeef".to_string());
        let toml = toml::to_string_pretty(&modern).expect("serialize modern");
        let parsed: AgentConfig = toml::from_str(&toml).expect("parse modern");
        assert_eq!(parsed.agent_did.as_deref(), Some("did:peko:local:deadbeef"));
        assert_eq!(parsed.name, "modern-agent");
    }

    #[test]
    fn test_agent_state_display() {
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Busy.to_string(), "busy");
    }

    #[test]
    fn test_extension_config_default() {
        let config = ExtensionConfig::default();
        assert!(!config.enabled.is_empty());
        assert!(config.enabled.contains(&"builtin:tool:shell".to_string()));
    }

    #[test]
    fn test_extension_config_default_whitelist() {
        let config = ExtensionConfig::default();

        // Default whitelist enables common built-in tools using canonical extension IDs
        assert!(!config.enabled.is_empty());
        assert!(config.enabled.contains(&"builtin:tool:shell".to_string()));
        assert!(config.enabled.contains(&"builtin:tool:Read".to_string()));
    }

    #[test]
    fn test_extension_config_is_extension_enabled() {
        let config = ExtensionConfig {
            enabled: vec![
                "builtin:tool:shell".to_string(),
                "builtin:tool:Read".to_string(),
                "builtin:tool:write_file".to_string(),
                "builtin:tool:glob".to_string(),
                "builtin:tool:grep".to_string(),
                "builtin:tool:str_replace_file".to_string(),
                "builtin:tool:session".to_string(),
                "builtin:tool:cron".to_string(),
            ],
            ..Default::default()
        };

        // All whitelisted extensions should be enabled (canonical IDs)
        assert!(config.is_extension_enabled("builtin:tool:shell"));
        assert!(config.is_extension_enabled("builtin:tool:Read"));
        assert!(config.is_extension_enabled("builtin:tool:write_file"));
        assert!(config.is_extension_enabled("builtin:tool:glob"));
        assert!(config.is_extension_enabled("builtin:tool:grep"));
        assert!(config.is_extension_enabled("builtin:tool:str_replace_file"));
        assert!(config.is_extension_enabled("builtin:tool:session"));
        assert!(config.is_extension_enabled("builtin:tool:cron"));

        // Case-insensitive matching
        assert!(config.is_extension_enabled("BUILTIN:TOOL:SHELL"));
        assert!(config.is_extension_enabled("Builtin:Tool:Session"));

        // Bare tool names should NOT match (no special-case parsing)
        assert!(!config.is_extension_enabled("shell"));
        assert!(!config.is_extension_enabled("Read"));

        // Unknown extensions should not be enabled
        assert!(!config.is_extension_enabled("unknown_tool"));
    }

    #[test]
    fn test_extension_config_empty_whitelist_disallows_all() {
        // Empty whitelist should disallow ALL extensions (secure by default)
        let config = ExtensionConfig {
            enabled: vec![],
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            str_replace_file: None,
        };

        assert!(!config.is_extension_enabled("builtin:tool:shell"));
        assert!(!config.is_extension_enabled("builtin:tool:Read"));
        assert!(!config.is_extension_enabled("mcp:any_server"));
    }

    #[test]
    fn test_extension_config_custom_whitelist() {
        // Custom whitelist using canonical extension IDs
        let config = ExtensionConfig {
            enabled: vec![
                "builtin:tool:Read".to_string(),
                "builtin:tool:write_file".to_string(),
            ],
            http: None,
            custom: None,
            read_file: None,
            write_file: None,
            glob: None,
            grep: None,
            str_replace_file: None,
        };

        assert!(config.is_extension_enabled("builtin:tool:Read"));
        assert!(config.is_extension_enabled("builtin:tool:write_file"));
        assert!(!config.is_extension_enabled("builtin:tool:shell"));
    }

    #[test]
    fn test_agent_config_has_default_extensions() {
        let config = AgentConfig::default();

        // Default extension config exists with common built-in tools enabled
        assert!(config.extensions.is_some());
        let extensions = config.extensions.unwrap();
        assert!(!extensions.enabled.is_empty());
        assert!(extensions
            .enabled
            .contains(&"builtin:tool:shell".to_string()));
    }

    #[test]
    fn test_extension_config_toml_serialization() {
        let config = ExtensionConfig {
            enabled: vec![
                "builtin:tool:shell".to_string(),
                "builtin:tool:Read".to_string(),
            ],
            ..Default::default()
        };
        let toml = toml::to_string(&config).unwrap();

        // Should contain the enabled list with canonical IDs
        assert!(toml.contains("enabled"));
        assert!(toml.contains("builtin:tool:shell"));
        assert!(toml.contains("builtin:tool:Read"));
    }

    /// v3-cleanup (commit 2.1): the legacy `[provider]` and
    /// `owner_id` fields are gone entirely. The agent TOML carries
    /// only soft hints and ownership metadata (`owner` as a `Principal`).
    #[test]
    fn test_v3_round_trip_has_no_legacy_fields() {
        let mut config = AgentConfig::default();
        config.name = "modern".to_string();
        config.preferred_provider_id = Some("openai".into());
        config.preferred_model_id = Some("gpt-4o-mini".into());

        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(
            !toml.contains("[provider]"),
            "[provider] table must NOT be serialized in v3 (PR #43 cleanup): {toml}"
        );
        // `owner` serializes as a `Principal` inline table (ADR-039).
        assert!(
            toml.contains("owner"),
            "owner field must be serialized as a Principal in v3 (PR #43 cleanup): {toml}"
        );
        // Soft hints round-trip.
        assert!(toml.contains("preferred_provider_id = \"openai\""));
        assert!(toml.contains("preferred_model_id = \"gpt-4o-mini\""));
        assert!(toml.contains("version = \"3.0\""));
    }
}
