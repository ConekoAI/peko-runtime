//! MCP configuration types
//!
//! Defines configuration structures for MCP servers that can be loaded
//! from TOML configuration files.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for a single reserved parameter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReservedParamConfig {
    /// Source type: "runtime", "env", or "static"
    pub source: String,
    /// Field name (for runtime) or variable name (for env) or value (for static)
    pub field: Option<String>,
    /// Environment variable name (alternative to field for env source)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
    /// Static value (for static source)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ReservedParamConfig {
    /// Create a runtime parameter from a context field
    pub fn runtime(field: impl Into<String>) -> Self {
        Self {
            source: "runtime".to_string(),
            field: Some(field.into()),
            var: None,
            value: None,
            description: None,
        }
    }

    /// Create a parameter from an environment variable
    pub fn env(var: impl Into<String>) -> Self {
        Self {
            source: "env".to_string(),
            field: None,
            var: Some(var.into()),
            value: None,
            description: None,
        }
    }

    /// Create a static parameter with a hardcoded value
    pub fn static_value(val: impl Into<String>) -> Self {
        Self {
            source: "static".to_string(),
            field: None,
            var: None,
            value: Some(val.into()),
            description: None,
        }
    }

    /// Get the parameter value based on the source and context
    pub fn resolve(&self, ctx: Option<&crate::tools::ToolContext>) -> Value {
        match self.source.as_str() {
            "runtime" => {
                if let (Some(ctx), Some(field)) = (ctx, &self.field) {
                    match field.as_str() {
                        "agent_id" => ctx
                            .agent_id
                            .as_ref()
                            .map_or(Value::Null, |v| Value::String(v.clone())),
                        "session_id" => ctx
                            .session_id
                            .as_ref()
                            .map_or(Value::Null, |v| Value::String(v.clone())),
                        "peer_id" => ctx
                            .peer_id
                            .as_ref()
                            .map_or(Value::Null, |v| Value::String(v.clone())),
                        "workspace" => ctx
                            .workspace
                            .as_ref()
                            .map_or(Value::Null, |v| Value::String(v.clone())),
                        "run_id" => Value::String(ctx.run_id.clone()),
                        "tool_id" => Value::String(ctx.tool_id.clone()),
                        "tool_name" => Value::String(ctx.tool_name.clone()),
                        _ => Value::Null,
                    }
                } else {
                    Value::Null
                }
            }
            "env" => self
                .var
                .as_ref()
                .and_then(|v| std::env::var(v).ok())
                .map_or(Value::Null, Value::String),
            "static" => self
                .value
                .as_ref()
                .map_or(Value::Null, |v| Value::String(v.clone())),
            _ => Value::Null,
        }
    }
}

/// Transport type for MCP connections
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    /// Standard input/output (local subprocess)
    #[default]
    Stdio,
    /// Server-Sent Events (HTTP)
    Sse,
}

/// Environment variable configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// Configuration for a single MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name (unique identifier)
    pub name: String,

    /// Transport type
    #[serde(default)]
    pub transport: TransportType,

    /// Command to execute (for stdio transport)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Arguments for the command
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Working directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,

    /// Endpoint URL (for SSE transport)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Whether to auto-start this server
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,

    /// Health check interval in seconds
    #[serde(default = "default_health_check_interval_secs")]
    pub health_check_interval_secs: u64,

    /// Maximum restart attempts (0 = unlimited)
    #[serde(default)]
    pub max_restarts: u32,

    /// Timeout for initialization in seconds
    #[serde(default = "default_init_timeout_secs")]
    pub init_timeout_secs: u64,

    /// Timeout for tool calls in seconds
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,

    /// Reserved parameters to inject into tool calls
    /// These are hidden from the LLM but injected by the runtime
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub reserved_parameters: HashMap<String, ReservedParamConfig>,
}

fn default_auto_start() -> bool {
    true
}

fn default_health_check_interval_secs() -> u64 {
    30
}

fn default_init_timeout_secs() -> u64 {
    30
}

fn default_tool_timeout_secs() -> u64 {
    60
}

impl McpServerConfig {
    /// Create a new stdio server configuration
    pub fn stdio(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            transport: TransportType::Stdio,
            command: Some(command.into()),
            args,
            env: HashMap::new(),
            cwd: None,
            endpoint: None,
            auto_start: default_auto_start(),
            health_check_interval_secs: default_health_check_interval_secs(),
            max_restarts: 0,
            init_timeout_secs: default_init_timeout_secs(),
            tool_timeout_secs: default_tool_timeout_secs(),
            reserved_parameters: HashMap::new(),
        }
    }

    /// Create a new SSE server configuration
    pub fn sse(name: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: TransportType::Sse,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            endpoint: Some(endpoint.into()),
            auto_start: default_auto_start(),
            health_check_interval_secs: default_health_check_interval_secs(),
            max_restarts: 0,
            init_timeout_secs: default_init_timeout_secs(),
            tool_timeout_secs: default_tool_timeout_secs(),
            reserved_parameters: HashMap::new(),
        }
    }

    /// Set environment variables
    #[must_use]
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Set working directory
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set auto-start
    #[must_use]
    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }

    /// Set reserved parameters for injection
    #[must_use]
    pub fn with_reserved_parameters(
        mut self,
        params: HashMap<String, ReservedParamConfig>,
    ) -> Self {
        self.reserved_parameters = params;
        self
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        // Name must not be empty
        if self.name.is_empty() {
            anyhow::bail!("Server name cannot be empty");
        }

        // Validate based on transport type
        match self.transport {
            TransportType::Stdio => {
                if self.command.is_none() || self.command.as_ref().unwrap().is_empty() {
                    anyhow::bail!("Command is required for stdio transport");
                }
            }
            TransportType::Sse => {
                if self.endpoint.is_none() || self.endpoint.as_ref().unwrap().is_empty() {
                    anyhow::bail!("Endpoint is required for SSE transport");
                }
            }
        }

        Ok(())
    }
}

/// JSON format MCP configuration (compatible with Claude Desktop)
///
/// Example:
/// ```json
/// {
///   "mcpServers": {
///     "filesystem": {
///       "command": "npx",
///       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
///       "env": {}
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpJsonConfig {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpJsonServerConfig>,
}

/// Server configuration in JSON format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpJsonServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// Top-level MCP configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// List of MCP server configurations
    #[serde(default, rename = "server")]
    pub servers: Vec<McpServerConfig>,

    /// Global auto-start setting (can be overridden per-server)
    #[serde(default = "default_global_auto_start")]
    pub auto_start: bool,

    /// Global health check interval in seconds
    #[serde(default = "default_global_health_check_interval_secs")]
    pub health_check_interval_secs: u64,
}

fn default_global_auto_start() -> bool {
    true
}

fn default_global_health_check_interval_secs() -> u64 {
    30
}

impl McpConfig {
    /// Create a new empty configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a configuration with the given servers
    #[must_use]
    pub fn with_servers(servers: Vec<McpServerConfig>) -> Self {
        Self {
            servers,
            auto_start: default_global_auto_start(),
            health_check_interval_secs: default_global_health_check_interval_secs(),
        }
    }

    /// Add a server configuration
    pub fn add_server(&mut self, server: McpServerConfig) {
        self.servers.push(server);
    }

    /// Get a server configuration by name
    #[must_use]
    pub fn get_server(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Remove a server configuration by name
    pub fn remove_server(&mut self, name: &str) -> Option<McpServerConfig> {
        if let Some(pos) = self.servers.iter().position(|s| s.name == name) {
            Some(self.servers.remove(pos))
        } else {
            None
        }
    }

    /// Validate all server configurations
    pub fn validate(&self) -> anyhow::Result<()> {
        // Check for duplicate names
        let mut names = std::collections::HashSet::new();
        for server in &self.servers {
            if !names.insert(&server.name) {
                anyhow::bail!("Duplicate server name: {}", server.name);
            }
            server.validate()?;
        }
        Ok(())
    }

    /// Load configuration from a TOML string
    pub fn from_toml(toml: &str) -> anyhow::Result<Self> {
        let config: McpConfig = toml::from_str(toml)?;
        config.validate()?;
        Ok(config)
    }

    /// Convert to TOML string
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Load configuration from a JSON string (mcp.json format)
    ///
    /// This format is compatible with Claude Desktop and other MCP clients.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let json_config: McpJsonConfig = serde_json::from_str(json)?;

        let mut servers = Vec::new();
        for (name, server_config) in json_config.mcp_servers {
            servers.push(McpServerConfig {
                name,
                transport: TransportType::Stdio,
                command: Some(server_config.command),
                args: server_config.args,
                env: server_config.env,
                cwd: server_config.cwd,
                endpoint: None,
                auto_start: default_auto_start(),
                health_check_interval_secs: default_health_check_interval_secs(),
                max_restarts: 0,
                init_timeout_secs: default_init_timeout_secs(),
                tool_timeout_secs: default_tool_timeout_secs(),
                reserved_parameters: HashMap::new(),
            });
        }

        let config = Self {
            servers,
            auto_start: default_global_auto_start(),
            health_check_interval_secs: default_global_health_check_interval_secs(),
        };

        config.validate()?;
        Ok(config)
    }

    /// Convert to JSON string (mcp.json format)
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut mcp_servers = HashMap::new();

        for server in &self.servers {
            // Only stdio servers can be represented in JSON format
            if server.transport == TransportType::Stdio {
                mcp_servers.insert(
                    server.name.clone(),
                    McpJsonServerConfig {
                        command: server.command.clone().unwrap_or_default(),
                        args: server.args.clone(),
                        env: server.env.clone(),
                        cwd: server.cwd.clone(),
                    },
                );
            }
        }

        let json_config = McpJsonConfig { mcp_servers };
        Ok(serde_json::to_string_pretty(&json_config)?)
    }

    /// Load configuration from file (auto-detects format by extension)
    ///
    /// Supports both `.toml` and `.json` formats.
    pub async fn from_file(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let content = tokio::fs::read_to_string(path.as_ref()).await?;
        let path_str = path.as_ref().to_string_lossy();

        if path_str.ends_with(".json") {
            Self::from_json(&content)
        } else {
            // Default to TOML for .toml and other extensions
            Self::from_toml(&content)
        }
    }

    /// Load configuration from file synchronously (auto-detects format)
    pub fn from_file_sync(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let path_str = path.as_ref().to_string_lossy();

        if path_str.ends_with(".json") {
            Self::from_json(&content)
        } else {
            Self::from_toml(&content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_config() {
        let config =
            McpServerConfig::stdio("browser", "mcp-browser", vec!["--headless".to_string()])
                .with_env(HashMap::from([("KEY".to_string(), "value".to_string())]));

        assert_eq!(config.name, "browser");
        assert_eq!(config.transport, TransportType::Stdio);
        assert_eq!(config.command, Some("mcp-browser".to_string()));
        assert_eq!(config.args, vec!["--headless"]);
        assert_eq!(config.env.get("KEY"), Some(&"value".to_string()));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_sse_config() {
        let config = McpServerConfig::sse("remote", "https://example.com/mcp");

        assert_eq!(config.name, "remote");
        assert_eq!(config.transport, TransportType::Sse);
        assert_eq!(config.endpoint, Some("https://example.com/mcp".to_string()));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_empty_name() {
        let config = McpServerConfig::stdio("", "cmd", vec![]);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_stdio_missing_command() {
        let config = McpServerConfig {
            command: None,
            ..McpServerConfig::stdio("test", "cmd", vec![])
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_sse_missing_endpoint() {
        let config = McpServerConfig {
            endpoint: None,
            ..McpServerConfig::sse("test", "https://example.com")
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_roundtrip() {
        let mut config = McpConfig::new();
        config.add_server(McpServerConfig::stdio(
            "browser",
            "mcp-browser",
            vec!["--headless".to_string()],
        ));
        config.add_server(McpServerConfig::sse("remote", "https://example.com/mcp"));

        let toml = config.to_toml().unwrap();
        let parsed = McpConfig::from_toml(&toml).unwrap();

        assert_eq!(parsed.servers.len(), 2);
        assert_eq!(parsed.servers[0].name, "browser");
        assert_eq!(parsed.servers[1].name, "remote");
    }

    #[test]
    fn test_config_from_toml() {
        let toml = r#"
[[server]]
name = "browser"
transport = "stdio"
command = "mcp-browser"
args = ["--headless"]
auto_start = true

[[server]]
name = "memory"
transport = "sse"
endpoint = "https://memory.example.com/mcp"
health_check_interval_secs = 60
"#;

        let config = McpConfig::from_toml(toml).unwrap();
        assert_eq!(config.servers.len(), 2);

        let browser = config.get_server("browser").unwrap();
        assert_eq!(browser.transport, TransportType::Stdio);
        assert_eq!(browser.command, Some("mcp-browser".to_string()));

        let memory = config.get_server("memory").unwrap();
        assert_eq!(memory.transport, TransportType::Sse);
        assert_eq!(
            memory.endpoint,
            Some("https://memory.example.com/mcp".to_string())
        );
        assert_eq!(memory.health_check_interval_secs, 60);
    }

    #[test]
    fn test_duplicate_names() {
        let mut config = McpConfig::new();
        config.add_server(McpServerConfig::stdio("same", "cmd1", vec![]));
        config.add_server(McpServerConfig::stdio("same", "cmd2", vec![]));

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_reserved_param_config() {
        let runtime_param = ReservedParamConfig::runtime("agent_id");
        assert_eq!(runtime_param.source, "runtime");
        assert_eq!(runtime_param.field, Some("agent_id".to_string()));

        let env_param = ReservedParamConfig::env("API_KEY");
        assert_eq!(env_param.source, "env");
        assert_eq!(env_param.var, Some("API_KEY".to_string()));

        let static_param = ReservedParamConfig::static_value("production");
        assert_eq!(static_param.source, "static");
        assert_eq!(static_param.value, Some("production".to_string()));
    }

    #[test]
    fn test_config_with_reserved_params_from_toml() {
        let toml = r#"
[[server]]
name = "memory"
transport = "stdio"
command = "mcp-memory"

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
api_key = { source = "env", var = "API_KEY" }
environment = { source = "static", value = "production" }
"#;

        let config = McpConfig::from_toml(toml).unwrap();
        assert_eq!(config.servers.len(), 1);

        let memory = config.get_server("memory").unwrap();
        assert_eq!(memory.reserved_parameters.len(), 4);

        // Check runtime param
        let agent_id = memory.reserved_parameters.get("agent_id").unwrap();
        assert_eq!(agent_id.source, "runtime");
        assert_eq!(agent_id.field, Some("agent_id".to_string()));

        // Check env param
        let api_key = memory.reserved_parameters.get("api_key").unwrap();
        assert_eq!(api_key.source, "env");
        assert_eq!(api_key.var, Some("API_KEY".to_string()));

        // Check static param
        let env = memory.reserved_parameters.get("environment").unwrap();
        assert_eq!(env.source, "static");
        assert_eq!(env.value, Some("production".to_string()));
    }
}
