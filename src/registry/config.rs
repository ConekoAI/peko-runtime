//! Registry Configuration
//!
//! Defines registry sources and authentication configuration
//! as per `DATA_MODEL` §3 (runtime.toml registry section).

use serde::{Deserialize, Serialize};

/// Registry configuration from runtime.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Default registry host
    #[serde(default = "default_registry")]
    pub default: String,
    /// List of registry sources with priorities
    #[serde(default)]
    pub sources: Vec<RegistrySource>,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            default: default_registry(),
            sources: vec![RegistrySource {
                url: default_registry(),
                priority: 1,
                auth: None,
            }],
        }
    }
}

impl RegistryConfig {
    /// Create a new empty registry config
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a registry source
    pub fn add_source(&mut self, source: RegistrySource) {
        self.sources.push(source);
        // Re-sort by priority
        self.sources.sort_by_key(|s| s.priority);
    }

    /// Get source by URL
    #[must_use]
    pub fn get_source(&self, url: &str) -> Option<&RegistrySource> {
        self.sources.iter().find(|s| s.url == url)
    }

    /// Resolve a registry reference to a source
    /// Returns the source with matching host or the default
    #[must_use]
    pub fn resolve_source(&self, host: &str) -> Option<&RegistrySource> {
        self.sources
            .iter()
            .find(|s| s.url == host || s.url.strip_prefix("https://") == Some(host))
            .or_else(|| self.sources.first())
    }
}

fn default_registry() -> String {
    "pekohub.com".to_string()
}

/// A registry source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySource {
    /// Registry URL (e.g., "pekohub.com" or "<https://registry.example.com>")
    pub url: String,
    /// Priority order (lower = checked first)
    #[serde(default)]
    pub priority: u32,
    /// Authentication configuration
    #[serde(flatten)]
    pub auth: Option<AuthConfig>,
}

/// Authentication configuration for registries
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Bearer token authentication
    Token {
        /// Environment variable name containing the token
        env: String,
    },
    /// HTTP Basic authentication
    Basic {
        /// Environment variable name containing the username
        user_env: String,
        /// Environment variable name containing the password
        password_env: String,
    },
    /// No authentication (public registry)
    None,
}

impl AuthConfig {
    /// Create token auth from env var
    pub fn token(env: impl Into<String>) -> Self {
        Self::Token { env: env.into() }
    }

    /// Create basic auth from env vars
    pub fn basic(user_env: impl Into<String>, password_env: impl Into<String>) -> Self {
        Self::Basic {
            user_env: user_env.into(),
            password_env: password_env.into(),
        }
    }

    /// Resolve credentials from environment
    pub fn resolve(&self) -> anyhow::Result<ResolvedAuth> {
        match self {
            Self::Token { env } => {
                let token =
                    std::env::var(env).map_err(|_| anyhow::anyhow!("Env var {env} not set"))?;
                Ok(ResolvedAuth::Bearer(token))
            }
            Self::Basic {
                user_env,
                password_env,
            } => {
                let username = std::env::var(user_env)
                    .map_err(|_| anyhow::anyhow!("Env var {user_env} not set"))?;
                let password = std::env::var(password_env)
                    .map_err(|_| anyhow::anyhow!("Env var {password_env} not set"))?;
                Ok(ResolvedAuth::Basic { username, password })
            }
            Self::None => Ok(ResolvedAuth::None),
        }
    }
}

/// Resolved authentication credentials (ready to use)
#[derive(Debug, Clone)]
pub enum ResolvedAuth {
    /// Bearer token
    Bearer(String),
    /// HTTP Basic credentials
    Basic { username: String, password: String },
    /// No authentication
    None,
}

impl ResolvedAuth {
    /// Apply authentication to a request builder
    pub fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::Bearer(token) => req.bearer_auth(token),
            Self::Basic { username, password } => req.basic_auth(username, Some(password)),
            Self::None => req,
        }
    }
}

/// Load registry configuration from workspace
///
/// Reads `.pekobot/config.toml` and extracts the `[registry]` section.
pub fn load_from_workspace(workspace_path: impl AsRef<std::path::Path>) -> RegistryConfig {
    let config_path = workspace_path.as_ref().join("config.toml");

    if !config_path.exists() {
        return RegistryConfig::default();
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => parse_runtime_toml(&content),
        Err(_) => RegistryConfig::default(),
    }
}

/// Parse registry configuration from runtime.toml content
fn parse_runtime_toml(content: &str) -> RegistryConfig {
    // Parse the full TOML to extract just the registry section
    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(_) => return RegistryConfig::default(),
    };

    // Extract registry section
    if let Some(registry) = parsed.get("registry") {
        match registry.clone().try_into::<RegistryConfig>() {
            Ok(config) => config,
            Err(_) => RegistryConfig::default(),
        }
    } else {
        RegistryConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_config_default() {
        let config = RegistryConfig::default();
        assert_eq!(config.default, "pekohub.com");
        assert_eq!(config.sources.len(), 1);
    }

    #[test]
    fn test_resolve_source() {
        let mut config = RegistryConfig::new();
        config.add_source(RegistrySource {
            url: "custom.registry.com".to_string(),
            priority: 1,
            auth: None,
        });

        let source = config.resolve_source("custom.registry.com");
        assert!(source.is_some());
        assert_eq!(source.unwrap().url, "custom.registry.com");
    }

    #[test]
    fn test_auth_config_token() {
        let auth = AuthConfig::token("TEST_TOKEN");
        match auth {
            AuthConfig::Token { env } => assert_eq!(env, "TEST_TOKEN"),
            _ => panic!("Expected Token variant"),
        }
    }

    #[test]
    fn test_auth_config_basic() {
        let auth = AuthConfig::basic("USER", "PASS");
        match auth {
            AuthConfig::Basic {
                user_env,
                password_env,
            } => {
                assert_eq!(user_env, "USER");
                assert_eq!(password_env, "PASS");
            }
            _ => panic!("Expected Basic variant"),
        }
    }

    #[test]
    fn test_parse_runtime_toml_with_registry() {
        let toml = r#"
[registry]
default = "custom.registry.com"

[[registry.sources]]
url = "custom.registry.com"
priority = 1

[[registry.sources]]
url = "backup.registry.com"
priority = 2
auth = { type = "token", env = "REGISTRY_TOKEN" }
"#;
        let config = parse_runtime_toml(toml);
        assert_eq!(config.default, "custom.registry.com");
        assert_eq!(config.sources.len(), 2);
    }

    #[test]
    fn test_parse_runtime_toml_without_registry() {
        let toml = r"
[daemon]
port = 11435
";
        let config = parse_runtime_toml(toml);
        assert_eq!(config.default, "pekohub.com"); // Default value
    }

    #[test]
    fn test_parse_runtime_toml_invalid() {
        let toml = "not valid toml {{{";
        let config = parse_runtime_toml(toml);
        assert_eq!(config.default, "pekohub.com"); // Falls back to default
    }

    #[test]
    fn test_load_from_workspace_nonexistent() {
        let config = load_from_workspace("/nonexistent/path/that/does/not/exist");
        assert_eq!(config.default, "pekohub.com"); // Falls back to default
    }

    #[test]
    fn test_registry_source_with_auth() {
        let source = RegistrySource {
            url: "private.registry.com".to_string(),
            priority: 1,
            auth: Some(AuthConfig::token("MY_TOKEN")),
        };

        assert_eq!(source.url, "private.registry.com");
        assert_eq!(source.priority, 1);
        assert!(source.auth.is_some());
    }

    #[test]
    fn test_registry_source_no_auth() {
        let source = RegistrySource {
            url: "public.registry.com".to_string(),
            priority: 2,
            auth: None,
        };

        assert_eq!(source.url, "public.registry.com");
        assert!(source.auth.is_none());
    }

    #[test]
    fn test_auth_config_none() {
        let auth = AuthConfig::None;
        match auth {
            AuthConfig::None => (), // Expected
            _ => panic!("Expected None variant"),
        }
    }

    #[test]
    fn test_resolve_auth_none() {
        let auth = AuthConfig::None;
        let resolved = auth.resolve().unwrap();
        match resolved {
            ResolvedAuth::None => (), // Expected
            _ => panic!("Expected ResolvedAuth::None"),
        }
    }

    #[test]
    fn test_resolved_auth_apply() {
        use reqwest;

        // Test Bearer auth
        let bearer = ResolvedAuth::Bearer("token123".to_string());
        let req = reqwest::Client::new().get("http://example.com");
        let _ = bearer.apply(req);

        // Test Basic auth
        let basic = ResolvedAuth::Basic {
            username: "user".to_string(),
            password: "pass".to_string(),
        };
        let req = reqwest::Client::new().get("http://example.com");
        let _ = basic.apply(req);

        // Test None auth
        let none = ResolvedAuth::None;
        let req = reqwest::Client::new().get("http://example.com");
        let _ = none.apply(req);
    }

    #[test]
    fn test_config_add_and_resolve_source() {
        // Create empty config without default sources
        let mut config = RegistryConfig {
            default: "pekohub.com".to_string(),
            sources: Vec::new(),
        };

        // Add sources in reverse priority order
        config.add_source(RegistrySource {
            url: "low.priority.com".to_string(),
            priority: 2,
            auth: None,
        });

        config.add_source(RegistrySource {
            url: "high.priority.com".to_string(),
            priority: 1,
            auth: None,
        });

        // Should be sorted by priority
        assert_eq!(config.sources[0].url, "high.priority.com");
        assert_eq!(config.sources[1].url, "low.priority.com");

        // Test resolve_source - should find exact match
        let resolved = config.resolve_source("high.priority.com");
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().url, "high.priority.com");

        // Test resolve_source fallback to first source
        let resolved = config.resolve_source("nonexistent.com");
        assert!(resolved.is_some()); // Falls back to first source
        assert_eq!(resolved.unwrap().url, "high.priority.com");

        // Test get_source
        let source = config.get_source("low.priority.com");
        assert!(source.is_some());
        assert_eq!(source.unwrap().priority, 2);
    }

    #[test]
    fn test_parse_runtime_toml_with_basic_auth() {
        // Use flattened format for auth (fields at same level due to #[serde(flatten)])
        let toml = r#"
[registry]
default = "secure.registry.com"

[[registry.sources]]
url = "secure.registry.com"
priority = 1
type = "basic"
user_env = "REG_USER"
password_env = "REG_PASS"
"#;
        let config = parse_runtime_toml(toml);
        assert_eq!(config.default, "secure.registry.com");
        assert_eq!(config.sources.len(), 1);

        match &config.sources[0].auth {
            Some(AuthConfig::Basic {
                user_env,
                password_env,
            }) => {
                assert_eq!(user_env, "REG_USER");
                assert_eq!(password_env, "REG_PASS");
            }
            other => panic!("Expected Basic auth, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_runtime_toml_with_multiple_sections() {
        let toml = r#"
[daemon]
port = 11435
host = "127.0.0.1"

[registry]
default = "custom.registry.com"

[[registry.sources]]
url = "custom.registry.com"
priority = 1

[providers]
anthropic_api_key_env = "ANTHROPIC_API_KEY"
"#;
        let config = parse_runtime_toml(toml);
        assert_eq!(config.default, "custom.registry.com");
        assert_eq!(config.sources.len(), 1);
    }
}
