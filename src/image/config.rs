//! Agent Configuration (config.toml)
//!
//! Defines the structure of config.toml as per `DATA_MODEL` §2.
//! This is the only required file in an agent image.

use serde::{Deserialize, Serialize};

/// Agent configuration from config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent identity
    pub agent: AgentIdentity,
    /// LLM provider configuration
    pub provider: ProviderConfig,
    /// Base image inheritance (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseImage>,
    /// Capability dependencies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilityConfig>,
    /// Triggers for outbound activation
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// Resource limits
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
}

impl AgentConfig {
    /// Load config from a TOML file
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config.toml: {e}"))?;
        Self::from_toml(&contents)
    }

    /// Parse from TOML string
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        // First, check for credentials in raw TOML (before parsing, to catch unknown fields)
        Self::check_raw_toml_for_credentials(toml_str)?;

        let config: Self = toml::from_str(toml_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse config.toml: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    /// Check raw TOML string for credential patterns (security hardening)
    /// This catches credentials even in unknown/invalid fields
    fn check_raw_toml_for_credentials(toml_str: &str) -> anyhow::Result<()> {
        // Parse as generic TOML value to catch all fields including unknown ones
        let toml_value: toml::Value = toml::from_str(toml_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse config for credential check: {e}"))?;

        // Patterns that indicate credentials (case-insensitive, checked against field names)
        let credential_patterns: &[&str] = &[
            "_api_key",
            "api_key",
            "apikey",
            "_secret_key",
            "secret_key",
            "_secret",
            "secret",
            "_token",
            "token",
            "_password",
            "password",
            "_access_key",
            "access_key",
            "_private_key",
            "private_key",
            "_key", // generic key pattern (check last)
        ];

        fn scan_value(value: &toml::Value, path: &str, patterns: &[&str]) -> anyhow::Result<()> {
            match value {
                toml::Value::Table(table) => {
                    for (key, val) in table {
                        let new_path = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path}.{key}")
                        };
                        scan_value(val, &new_path, patterns)?;
                    }
                }
                toml::Value::Array(arr) => {
                    for (i, val) in arr.iter().enumerate() {
                        scan_value(val, &format!("{path}[{i}]"), patterns)?;
                    }
                }
                toml::Value::String(s) => {
                    // Skip environment variable references (these are safe)
                    if s.starts_with('$') || s.starts_with("env.") {
                        return Ok(());
                    }

                    // Check if the field name looks like a credential field
                    let key_lower = path.to_lowercase();
                    for pattern in patterns {
                        if key_lower.ends_with(pattern) {
                            // Check if value looks like an actual credential (not a placeholder)
                            if looks_like_credential_value(s) {
                                return Err(anyhow::anyhow!(
                                    "Security violation: Field '{}' appears to contain a credential value. \
                                     Do not put credentials in config.toml. Use environment variables instead (e.g., '${}').",
                                    path,
                                    path.to_uppercase().replace('.', "_")
                                ));
                            }
                        }
                    }
                }
                _ => {} // Other types (int, float, bool, datetime) can't contain credentials
            }
            Ok(())
        }

        fn looks_like_credential_value(value: &str) -> bool {
            // Skip short values (likely placeholders)
            if value.len() < 8 {
                return false;
            }

            // Skip common placeholder values
            let lower = value.to_lowercase();
            let placeholders = [
                "placeholder",
                "example",
                "sample",
                "test",
                "dummy",
                "your_",
                "change_me",
                "changeme",
                "TODO",
                "FIXME",
                "<insert",
                "insert_",
                "...",
                "xxx",
                "***",
            ];
            for ph in &placeholders {
                if lower.contains(ph) {
                    return false;
                }
            }

            // Check for common credential prefixes
            let credential_prefixes = [
                "sk-",
                "sk_live_",
                "sk_test_",
                "bearer ",
                "basic ",
                "ghp_",
                "github_pat_",
            ];
            for prefix in &credential_prefixes {
                if lower.starts_with(prefix) {
                    return true;
                }
            }

            // Long values (20+ chars) with mixed alphanumeric are likely credentials
            if value.len() >= 20 {
                let has_digit = value.chars().any(|c| c.is_ascii_digit());
                let has_letter = value.chars().any(|c| c.is_ascii_alphabetic());
                if has_digit && has_letter {
                    return true;
                }
            }

            false
        }

        scan_value(&toml_value, "", credential_patterns)?;
        Ok(())
    }

    /// Serialize to TOML string
    pub fn to_toml(&self) -> anyhow::Result<String> {
        toml::to_string_pretty(self).map_err(|e| anyhow::anyhow!("Failed to serialize config: {e}"))
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        // Validate agent name
        if !is_valid_agent_name(&self.agent.name) {
            return Err(anyhow::anyhow!(
                "Invalid agent name '{}'. Must match regex: ^[a-z0-9][a-z0-9-]*[a-z0-9]$, 3-64 chars",
                self.agent.name
            ));
        }

        // Validate version is semver
        if !is_valid_semver(&self.agent.version) {
            return Err(anyhow::anyhow!(
                "Invalid version '{}'. Must be a valid semver string",
                self.agent.version
            ));
        }

        // Validate triggers
        for (i, trigger) in self.triggers.iter().enumerate() {
            if let Err(e) = trigger.validate() {
                return Err(anyhow::anyhow!("Trigger [{i}] validation failed: {e}"));
            }
        }

        // Check for duplicate webhook paths
        let mut seen_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for trigger in &self.triggers {
            if let TriggerType::Webhook { path, .. } = &trigger.trigger_type {
                if !seen_paths.insert(path.as_str()) {
                    return Err(anyhow::anyhow!("Duplicate webhook path: {path}"));
                }
            }
        }

        // Note: Credential check is done in from_toml() before parsing to catch unknown fields

        Ok(())
    }

    /// Merge with a base config (base first, then self overrides)
    #[must_use]
    pub fn merge_with_base(mut self, base: Self) -> Self {
        // Merge capabilities (append and deduplicate)
        let base_caps = base.capabilities;
        let self_caps = self.capabilities.take();
        self.capabilities = match (self_caps, base_caps) {
            (Some(mut s), Some(b)) => {
                s.merge_from(b);
                Some(s)
            }
            (Some(s), None) => Some(s),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // Merge triggers (base triggers first, then self triggers appended)
        let mut merged_triggers = base.triggers;
        merged_triggers.extend(self.triggers);
        self.triggers = merged_triggers;

        // Provider: self overrides entirely if present, else inherit base
        // (already default behavior via struct replacement)

        self
    }

    /// Get tools list
    #[must_use]
    pub fn tools(&self) -> Vec<String> {
        self.capabilities
            .as_ref()
            .and_then(|c| c.tools.clone())
            .unwrap_or_default()
    }

    /// Get skills list
    #[must_use]
    pub fn skills(&self) -> Vec<String> {
        self.capabilities
            .as_ref()
            .and_then(|c| c.skills.clone())
            .unwrap_or_default()
    }

    /// Get mcps list
    #[must_use]
    pub fn mcps(&self) -> Vec<String> {
        self.capabilities
            .as_ref()
            .and_then(|c| c.mcps.clone())
            .unwrap_or_default()
    }
}

/// Agent identity section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Agent name (lowercase alphanumeric + hyphens, 3-64 chars)
    pub name: String,
    /// Version (semver)
    pub version: String,
    /// Description (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Author (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// License (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

/// Provider configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider type
    #[serde(rename = "provider_type")]
    pub provider_type: ProviderType,
    /// Model identifier
    pub model: String,
    /// Max tokens (default: 8096)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Temperature (default: 1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p sampling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Inline system prompt (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Base URL (for `openai_compatible`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: ProviderType::Anthropic,
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: Some(8096),
            temperature: Some(1.0),
            top_p: None,
            system_prompt: None,
            base_url: None,
        }
    }
}

/// LLM provider types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// Anthropic Claude
    Anthropic,
    /// `OpenAI`
    Openai,
    /// Local Ollama
    Ollama,
    /// OpenAI-compatible endpoint
    #[serde(rename = "openai_compatible")]
    OpenaiCompatible,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::Anthropic => write!(f, "anthropic"),
            ProviderType::Openai => write!(f, "openai"),
            ProviderType::Ollama => write!(f, "ollama"),
            ProviderType::OpenaiCompatible => write!(f, "openai_compatible"),
        }
    }
}

/// Base image configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseImage {
    /// Base image reference or digest
    pub image: String,
}

/// Capability dependencies
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityConfig {
    /// Required tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Required skills
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Required MCPs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcps: Option<Vec<String>>,
    /// Session plugin
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionCapability>,
    /// Options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<CapabilityOptions>,
}

impl CapabilityConfig {
    /// Merge with base capabilities
    pub fn merge_from(&mut self, base: Self) {
        // Merge arrays (append and deduplicate)
        self.tools = merge_option_arrays(self.tools.take(), base.tools);
        self.skills = merge_option_arrays(self.skills.take(), base.skills);
        self.mcps = merge_option_arrays(self.mcps.take(), base.mcps);
        // Session and options: self overrides
    }
}

/// Session capability config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCapability {
    /// Plugin name
    pub plugin: String,
}

/// Capability options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityOptions {
    /// Auto-install missing capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_install: Option<bool>,
}

/// Trigger configuration for outbound activation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    /// Trigger type
    #[serde(flatten)]
    pub trigger_type: TriggerType,
    /// Action: "run" (start session with trigger as message)
    pub action: String,
    /// Session target: "new" or "active"
    #[serde(default = "default_session_target")]
    pub session: String,
    /// Whether trigger is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Trigger types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerType {
    /// Cron-based schedule
    Cron {
        /// 5-field crontab expression
        schedule: String,
    },
    /// Webhook endpoint
    Webhook {
        /// Path suffix under /`webhooks/{instance_id}/{token`}
        path: String,
        /// Optional shared secret token
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// Event bus trigger
    Event {
        /// Event topic pattern
        topic: String,
    },
    /// File watch trigger
    FileWatch {
        /// Path to watch (relative to workspace)
        path: String,
        /// Optional glob filter
        #[serde(skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },
}

impl Trigger {
    /// Validate this trigger
    pub fn validate(&self) -> anyhow::Result<()> {
        match &self.trigger_type {
            TriggerType::Cron { schedule } => {
                // Validate crontab expression (5 fields)
                let parts: Vec<&str> = schedule.split_whitespace().collect();
                if parts.len() != 5 {
                    return Err(anyhow::anyhow!(
                        "Invalid cron schedule '{schedule}'. Expected 5 fields (minute hour day month weekday)"
                    ));
                }
            }
            TriggerType::Webhook { path, token } => {
                if path.is_empty() {
                    return Err(anyhow::anyhow!("Webhook path cannot be empty"));
                }
                // Validate token if present (32-128 printable ASCII)
                if let Some(t) = token {
                    if t.len() < 32 || t.len() > 128 {
                        return Err(anyhow::anyhow!(
                            "Webhook token must be 32-128 characters, got {}",
                            t.len()
                        ));
                    }
                    if !t.chars().all(|c| c.is_ascii_graphic()) {
                        return Err(anyhow::anyhow!(
                            "Webhook token must contain only printable ASCII characters"
                        ));
                    }
                }
            }
            TriggerType::Event { topic } => {
                if topic.is_empty() {
                    return Err(anyhow::anyhow!("Event topic cannot be empty"));
                }
            }
            TriggerType::FileWatch { path, .. } => {
                if path.is_empty() {
                    return Err(anyhow::anyhow!("File watch path cannot be empty"));
                }
            }
        }
        Ok(())
    }
}

/// Resource limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resources {
    /// Max concurrent tool calls
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tools: Option<u32>,
    /// Tool timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_timeout_seconds: Option<u32>,
    /// Session idle timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_timeout_seconds: Option<u32>,
    /// Max session tokens (for context truncation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_session_tokens: Option<u32>,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            max_concurrent_tools: Some(5),
            tool_timeout_seconds: Some(30),
            session_timeout_seconds: None,
            max_session_tokens: Some(200000),
        }
    }
}

// Helper functions

fn is_valid_agent_name(name: &str) -> bool {
    if name.len() < 3 || name.len() > 64 {
        return false;
    }
    // Must start and end with alphanumeric, contain only alphanumeric and hyphens
    name.chars().enumerate().all(|(i, c)| {
        if i == 0 || i == name.len() - 1 {
            c.is_ascii_alphanumeric()
        } else {
            c.is_ascii_alphanumeric() || c == '-'
        }
    })
}

fn is_valid_semver(version: &str) -> bool {
    // Basic semver check: major.minor.patch with optional pre-release/build
    let re = regex::Regex::new(r"^(\d+)\.(\d+)\.(\d+)(-[a-zA-Z0-9.-]+)?(\+[a-zA-Z0-9.-]+)?$");
    re.map(|r| r.is_match(version)).unwrap_or(false)
}

fn default_session_target() -> String {
    "new".to_string()
}

fn default_true() -> bool {
    true
}

fn merge_option_arrays<T: Clone + PartialEq>(
    local: Option<Vec<T>>,
    base: Option<Vec<T>>,
) -> Option<Vec<T>> {
    match (local, base) {
        (Some(mut l), Some(b)) => {
            // Append base items not already in local
            for item in b {
                if !l.contains(&item) {
                    l.push(item);
                }
            }
            Some(l)
        }
        (Some(l), None) => Some(l),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_agent_names() {
        assert!(is_valid_agent_name("test-agent"));
        assert!(is_valid_agent_name("myagent123"));
        assert!(is_valid_agent_name("a-b-c"));
        assert!(!is_valid_agent_name("ab")); // too short
        assert!(!is_valid_agent_name("-test-")); // starts/ends with hyphen
        assert!(!is_valid_agent_name("test_agent")); // underscore
    }

    #[test]
    fn test_valid_semver() {
        assert!(is_valid_semver("1.0.0"));
        assert!(is_valid_semver("2.5.1-beta"));
        assert!(is_valid_semver("0.1.0+build123"));
        assert!(!is_valid_semver("1.0")); // missing patch
        assert!(!is_valid_semver("v1.0.0")); // v prefix
    }

    #[test]
    fn test_config_parse() {
        let toml = r#"
[agent]
name = "test-agent"
version = "1.0.0"
description = "A test agent"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
max_tokens = 4096

[capabilities]
tools = ["read_file", "web_search"]

[[triggers]]
type = "cron"
schedule = "0 8 * * *"
action = "run"
session = "new"
"#;

        let config = AgentConfig::from_toml(toml).unwrap();
        assert_eq!(config.agent.name, "test-agent");
        assert_eq!(config.agent.version, "1.0.0");
        assert_eq!(config.tools(), vec!["read_file", "web_search"]);
    }

    #[test]
    fn test_trigger_validation() {
        let trigger = Trigger {
            trigger_type: TriggerType::Cron {
                schedule: "0 8 * * *".to_string(),
            },
            action: "run".to_string(),
            session: "new".to_string(),
            enabled: true,
        };
        assert!(trigger.validate().is_ok());

        let bad_trigger = Trigger {
            trigger_type: TriggerType::Cron {
                schedule: "invalid".to_string(),
            },
            action: "run".to_string(),
            session: "new".to_string(),
            enabled: true,
        };
        assert!(bad_trigger.validate().is_err());
    }

    #[test]
    fn test_capability_merge() {
        let base = CapabilityConfig {
            tools: Some(vec!["tool1".to_string(), "tool2".to_string()]),
            skills: Some(vec!["skill1".to_string()]),
            mcps: None,
            session: None,
            options: None,
        };

        let mut local = CapabilityConfig {
            tools: Some(vec!["tool2".to_string(), "tool3".to_string()]),
            skills: None,
            mcps: Some(vec!["mcp1".to_string()]),
            session: None,
            options: None,
        };

        local.merge_from(base);

        // Should have tool1, tool2, tool3 (deduplicated, local first)
        assert_eq!(local.tools.unwrap().len(), 3);
        // Should inherit skill1 from base
        assert_eq!(local.skills.unwrap().len(), 1);
        // Should keep local mcp
        assert_eq!(local.mcps.unwrap().len(), 1);
    }

    #[test]
    fn test_credential_detection_blocks_secrets() {
        // Test that actual-looking credentials are rejected
        let toml_with_secret = r#"
[agent]
name = "test-agent"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
secret_key = "sk-ant-api03-AbCdEfGh123456789XYZabcdefghijklmnopqrstuvwxyz"
"#;

        let result = AgentConfig::from_toml(toml_with_secret);
        if result.is_ok() {
            // Debug: print what was parsed
            let config = result.unwrap();
            let toml_out = config.to_toml().unwrap();
            panic!("Expected error but config parsed successfully. TOML output:\n{toml_out}");
        }
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Security violation"),
            "Expected security violation, got: {err}"
        );
        assert!(
            err.contains("secret_key"),
            "Error should mention the field: {err}"
        );
    }

    #[test]
    fn test_credential_detection_allows_placeholders() {
        // Test that placeholder values are allowed
        let toml_with_placeholder = r#"
[agent]
name = "test-agent"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
api_key = "your_api_key_here"
"#;

        let result = AgentConfig::from_toml(toml_with_placeholder);
        assert!(result.is_ok(), "Placeholders should be allowed: {result:?}");
    }

    #[test]
    fn test_credential_detection_allows_env_vars() {
        // Test that env var references are allowed
        let toml_with_env = r#"
[agent]
name = "test-agent"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
api_key = "$ANTHROPIC_API_KEY"
"#;

        let result = AgentConfig::from_toml(toml_with_env);
        assert!(
            result.is_ok(),
            "Environment variable references should be allowed: {result:?}"
        );
    }

    #[test]
    fn test_credential_detection_blocks_api_key() {
        // Test that api_key fields with real-looking values are rejected
        let toml_with_api_key = r#"
[agent]
name = "test-agent"
version = "1.0.0"

[provider]
provider_type = "openai"
model = "gpt-4"
api_key = "sk-abcdefghijklmnop12345678901234567890ABCDEFGH"
"#;

        let result = AgentConfig::from_toml(toml_with_api_key);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Security violation"),
            "Expected security violation, got: {err}"
        );
    }

    #[test]
    fn test_credential_detection_allows_short_values() {
        // Test that short values are not flagged as credentials
        let toml_short = r#"
[agent]
name = "test-agent"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
token = "abc123"
"#;

        let result = AgentConfig::from_toml(toml_short);
        assert!(result.is_ok(), "Short values should be allowed: {result:?}");
    }
}
