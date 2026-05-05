//! MCP Runtime Starter
//!
//! Implements `ExtensionRuntimeStarter` for MCP extensions.
//!
//! Reads MCP extension manifests (ADR-024 unified format with `mcp_servers` section
//! or legacy `config.toml`/`config.json`), creates `McpRuntimeAdapter`s, and starts
/// them via the shared `BackgroundRuntimeManager`.

use super::starter::{ExtensionRuntimeStarter, StarterContext};
use crate::common::process::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
use crate::mcp::{
    config::{McpServerConfig, TransportType},
};
use crate::extensions::runtime::mcp_runtime_adapter::McpRuntimeAdapter;
use std::sync::Arc;
use tracing::{info, warn};

/// Starter for MCP extensions.
///
/// Supports both:
/// - **ADR-024 unified manifest** (`manifest.yaml` with `extension_type: "mcp"` and `mcp_servers`)
/// - **Legacy config** (`config.toml` or `config.json` with server definitions)
#[derive(Debug)]
pub struct McpRuntimeStarter;

impl McpRuntimeStarter {
    /// Create a new MCP runtime starter
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Parse MCP server configs from an extension directory.
    ///
    /// Tries unified manifest first, then falls back to legacy config files.
    async fn parse_server_configs(
        &self,
        ext_dir: &std::path::Path,
    ) -> anyhow::Result<Vec<McpServerConfig>> {
        let manifest_path = ext_dir.join("manifest.yaml");

        if manifest_path.exists() {
            let content = tokio::fs::read_to_string(&manifest_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read manifest.yaml: {e}"))?;

            let manifest: serde_yaml::Value = serde_yaml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse manifest.yaml: {e}"))?;

            // ADR-024 unified format: mcp_servers section
            if let Some(servers) = manifest.get("mcp_servers") {
                return Self::parse_mcp_servers_from_yaml(servers);
            }

            // Legacy: config section with embedded server config
            if let Some(config) = manifest.get("config") {
                if let Ok(cfg) = Self::try_parse_single_server_config(config) {
                    return Ok(vec![cfg]);
                }
            }
        }

        // Tier 1: MCP Registry server.json
        let server_json_path = ext_dir.join("server.json");
        if server_json_path.exists() {
            let content = tokio::fs::read_to_string(&server_json_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read server.json: {e}"))?;
            let registry_manifest: serde_json::Value = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse server.json: {e}"))?;
            if let Some(mcp_servers) =
                Self::registry_manifest_to_mcp_servers(&registry_manifest, &server_json_path)
            {
                return Self::parse_mcp_servers_from_json(&mcp_servers);
            }
        }

        // Fallback: legacy config.toml or config.json
        let toml_path = ext_dir.join("config.toml");
        let json_config_path = ext_dir.join("config.json");

        if toml_path.exists() {
            let content = tokio::fs::read_to_string(&toml_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read config.toml: {e}"))?;
            let config: crate::mcp::config::McpConfig = toml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse config.toml: {e}"))?;
            return Ok(config.servers);
        }

        if json_config_path.exists() {
            let content = tokio::fs::read_to_string(&json_config_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read config.json: {e}"))?;
            let config: crate::mcp::config::McpConfig = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse config.json: {e}"))?;
            return Ok(config.servers);
        }

        anyhow::bail!("No MCP server configuration found in extension directory")
    }

    /// Parse `mcp_servers` section from unified manifest.
    fn parse_mcp_servers_from_yaml(
        servers: &serde_yaml::Value,
    ) -> anyhow::Result<Vec<McpServerConfig>> {
        let servers_map = servers
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("mcp_servers must be a mapping"))?;

        let mut configs = Vec::new();

        for (name, value) in servers_map {
            let server_name = name
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("mcp_servers key must be a string"))?;

            let mut server_json = serde_json::to_value(value)
                .map_err(|e| anyhow::anyhow!("Failed to convert mcp_servers value: {e}"))?;

            // Ensure name field is present
            if let Some(obj) = server_json.as_object_mut() {
                if !obj.contains_key("name") {
                    obj.insert("name".to_string(), serde_json::json!(server_name));
                }
            }

            let config: McpServerConfig = serde_json::from_value(server_json)
                .map_err(|e| anyhow::anyhow!("Failed to parse mcp_servers config for '{}': {}", server_name, e))?;

            configs.push(config);
        }

        Ok(configs)
    }

    /// Parse `mcp_servers` section from a JSON value (used for server.json Tier 1 format).
    fn parse_mcp_servers_from_json(
        servers: &serde_json::Value,
    ) -> anyhow::Result<Vec<McpServerConfig>> {
        let servers_map = servers
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("mcp_servers must be an object"))?;

        let mut configs = Vec::new();

        for (server_name, value) in servers_map {
            let mut server_json = value.clone();

            // Ensure name field is present
            if let Some(obj) = server_json.as_object_mut() {
                if !obj.contains_key("name") {
                    obj.insert("name".to_string(), serde_json::json!(server_name));
                }
            }

            let config: McpServerConfig = serde_json::from_value(server_json)
                .map_err(|e| anyhow::anyhow!("Failed to parse mcp_servers config for '{}': {}", server_name, e))?;

            configs.push(config);
        }

        Ok(configs)
    }

    /// Convert an MCP Registry `server.json` into the `mcp_servers` structure.
    ///
    /// Looks at `transport` (top-level) or `packages[].transport` to build the
    /// server config map: `{ "server_name": { "command": "...", "args": [...] } }`.
    fn registry_manifest_to_mcp_servers(
        registry_manifest: &serde_json::Value,
        server_json_path: &std::path::Path,
    ) -> Option<serde_json::Value> {
        let server_name = registry_manifest
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Try top-level transport first
        let transport = registry_manifest.get("transport");
        // Fall back to first package's transport
        let transport = transport.or_else(|| {
            registry_manifest
                .get("packages")
                .and_then(|p| p.as_array())
                .and_then(|arr| arr.first())
                .and_then(|pkg| pkg.get("transport"))
        });

        let transport = transport?;
        let transport_type = transport
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("stdio");

        if transport_type != "stdio" {
            warn!(
                server_name = %server_name,
                transport_type = %transport_type,
                "Non-stdio transport in server.json is not yet auto-started by BackgroundRuntimeManager"
            );
            return None;
        }

        let command = transport
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let args = transport
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut server_config = serde_json::Map::new();
        server_config.insert("name".to_string(), serde_json::json!(server_name));
        server_config.insert("transport".to_string(), serde_json::json!(transport_type));
        if let Some(cmd) = command {
            server_config.insert("command".to_string(), serde_json::json!(cmd));
        }
        if !args.is_empty() {
            server_config.insert("args".to_string(), serde_json::json!(args));
        }
        server_config.insert("auto_start".to_string(), serde_json::json!(true));

        // Set cwd to the directory containing server.json so relative paths work
        if let Some(parent) = server_json_path.parent() {
            server_config.insert(
                "cwd".to_string(),
                serde_json::json!(parent.to_string_lossy().to_string()),
            );
        }

        let mut mcp_servers = serde_json::Map::new();
        mcp_servers.insert(server_name.to_string(), serde_json::Value::Object(server_config));

        Some(serde_json::Value::Object(mcp_servers))
    }

    /// Try to parse a single server config from a YAML value.
    fn try_parse_single_server_config(config: &serde_yaml::Value) -> anyhow::Result<McpServerConfig> {
        let json = serde_json::to_value(config)
            .map_err(|e| anyhow::anyhow!("Failed to convert config section: {e}"))?;
        let cfg: McpServerConfig = serde_json::from_value(json)
            .map_err(|e| anyhow::anyhow!("Failed to parse config section: {e}"))?;
        Ok(cfg)
    }

    /// Start a single stdio MCP server via BackgroundRuntimeManager.
    async fn start_stdio_server(
        &self,
        config: &McpServerConfig,
        ext_dir: &std::path::Path,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        let command = config
            .command
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' missing command", config.name))?;

        let cwd = config.cwd.clone().unwrap_or_else(|| ext_dir.to_path_buf());

        let process_config = ProcessSpawnConfig::new(command)
            .args(config.args.clone())
            .cwd(cwd);

        let spawn_config = RuntimeSpawnConfig::Process(process_config);

        let adapter = Arc::new(McpRuntimeAdapter::new(
            config.clone(),
            Arc::clone(&ctx.mcp_client_registry),
        ));

        let restart_policy = RestartPolicy {
            max_restarts: if config.max_restarts == 0 {
                u32::MAX
            } else {
                config.max_restarts
            },
            ..Default::default()
        };

        ctx.background_runtime_manager
            .start(config.name.clone(), spawn_config, adapter, restart_policy)
            .await?;

        info!("MCP server '{}' started via BackgroundRuntimeManager", config.name);
        Ok(())
    }
}

impl Default for McpRuntimeStarter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ExtensionRuntimeStarter for McpRuntimeStarter {
    fn extension_type(&self) -> &'static str {
        "mcp"
    }

    async fn start(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        let ext_dir = ctx.data_dir.join("extensions").join(extension_id);

        let configs = self.parse_server_configs(&ext_dir).await?;

        if configs.is_empty() {
            anyhow::bail!("No MCP server configurations found in extension '{}'", extension_id);
        }

        for config in &configs {
            match config.transport {
                TransportType::Stdio => {
                    self.start_stdio_server(config, &ext_dir, ctx).await?;
                }
                TransportType::Sse => {
                    // SSE transports are external connections, not supervised child processes.
                    // They are handled by McpManager directly, not BackgroundRuntimeManager.
                    warn!(
                        "MCP server '{}' uses SSE transport — not started via BackgroundRuntimeManager. \
                         Use McpManager::start_server() for SSE connections.",
                        config.name
                    );
                }
            }
        }

        Ok(())
    }
}
