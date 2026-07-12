use crate::common::process::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
use crate::daemon::background_runtime::starter::{ExtensionRuntimeStarter, StarterContext};
use crate::extensions::mcp::protocol::{
    client::ServerRequestHandler,
    config::{McpServerConfig, TransportType},
    sampling::SamplingRequestHandler,
};
use crate::extensions::mcp::runtime::adapter::McpRuntimeAdapter;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Starter for MCP extensions.
///
/// Supports:
/// - **ADR-024 unified manifest** (`manifest.yaml` with `extension_type: "mcp"` and `mcp_servers`)
/// - **MCP Registry** (`server.json`)
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

            let config: McpServerConfig = serde_json::from_value(server_json).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse mcp_servers config for '{}': {}",
                    server_name,
                    e
                )
            })?;

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

            let config: McpServerConfig = serde_json::from_value(server_json).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse mcp_servers config for '{}': {}",
                    server_name,
                    e
                )
            })?;

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

        if transport_type != "stdio" && transport_type != "sse" {
            warn!(
                server_name = %server_name,
                transport_type = %transport_type,
                "Non-stdio/sse transport in server.json is not yet auto-started by BackgroundRuntimeManager"
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
        let endpoint = transport
            .get("endpoint")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut server_config = serde_json::Map::new();
        server_config.insert("name".to_string(), serde_json::json!(server_name));
        server_config.insert("transport".to_string(), serde_json::json!(transport_type));
        if let Some(cmd) = command {
            server_config.insert("command".to_string(), serde_json::json!(cmd));
        }
        if !args.is_empty() {
            server_config.insert("args".to_string(), serde_json::json!(args));
        }
        if transport_type == "sse" {
            if let Some(ep) = endpoint {
                server_config.insert("endpoint".to_string(), serde_json::json!(ep));
            }
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
        mcp_servers.insert(
            server_name.to_string(),
            serde_json::Value::Object(server_config),
        );

        Some(serde_json::Value::Object(mcp_servers))
    }

    /// Try to parse a single server config from a YAML value.
    fn try_parse_single_server_config(
        config: &serde_yaml::Value,
    ) -> anyhow::Result<McpServerConfig> {
        let json = serde_json::to_value(config)
            .map_err(|e| anyhow::anyhow!("Failed to convert config section: {e}"))?;
        let cfg: McpServerConfig = serde_json::from_value(json)
            .map_err(|e| anyhow::anyhow!("Failed to parse config section: {e}"))?;
        Ok(cfg)
    }

    /// Start a single MCP server (stdio or SSE) via BackgroundRuntimeManager.
    async fn start_server_config(
        &self,
        config: &McpServerConfig,
        ext_dir: &Path,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        // F19: starter runs without a principal scope (it's daemon-driven
        // auto-start), so pass an unlimited meter. Tool-call-driven
        // auto-start builds its own SamplingRequestHandler in
        // `McpManager::start_server` with the caller's meter.
        let request_handler: Option<Arc<dyn ServerRequestHandler>> =
            ctx.resolver.as_ref().map(|resolver| {
                Arc::new(SamplingRequestHandler::new(
                    Arc::clone(resolver),
                    Arc::new(crate::quota::QuotaMeter::unlimited()),
                ))
                    as Arc<dyn ServerRequestHandler>
            });

        let adapter = Arc::new(McpRuntimeAdapter::new(
            config.clone(),
            Arc::clone(&ctx.mcp_client_registry),
            request_handler,
            ctx.vault.clone(),
        ));

        let restart_policy = RestartPolicy {
            max_restarts: if config.max_restarts == 0 {
                u32::MAX
            } else {
                config.max_restarts
            },
            ..Default::default()
        };

        let spawn_config = match config.transport {
            TransportType::Stdio => {
                let command = config.command.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("MCP server '{}' missing command", config.name)
                })?;

                let cwd = config.cwd.clone().unwrap_or_else(|| ext_dir.to_path_buf());

                let process_config = ProcessSpawnConfig::new(command)
                    .args(config.args.clone())
                    .cwd(cwd);

                RuntimeSpawnConfig::Process(process_config)
            }
            TransportType::Sse => {
                let endpoint = config.endpoint.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("MCP server '{}' missing endpoint", config.name)
                })?;

                RuntimeSpawnConfig::External {
                    endpoint: endpoint.clone(),
                    connect_timeout: Duration::from_secs(config.init_timeout_secs),
                }
            }
        };

        ctx.background_runtime_manager
            .start(config.name.clone(), spawn_config, adapter, restart_policy)
            .await?;

        info!(
            "MCP server '{}' started via BackgroundRuntimeManager",
            config.name
        );
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

    async fn start(&self, extension_id: &str, ctx: &StarterContext) -> anyhow::Result<()> {
        let ext_dir = ctx.data_dir.join("extensions").join(extension_id);

        let configs = self.parse_server_configs(&ext_dir).await?;

        if configs.is_empty() {
            anyhow::bail!(
                "No MCP server configurations found in extension '{}'",
                extension_id
            );
        }

        for config in &configs {
            self.start_server_config(config, &ext_dir, ctx).await?;
        }

        Ok(())
    }

    async fn auto_start(&self, ctx: &StarterContext) -> anyhow::Result<Vec<String>> {
        let extensions_dir = ctx.data_dir.join("extensions");
        let mut started = Vec::new();

        if !extensions_dir.exists() {
            return Ok(started);
        }

        let mut entries = tokio::fs::read_dir(&extensions_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let ext_dir = entry.path();
            if !ext_dir.is_dir() {
                continue;
            }

            let configs = match self.parse_server_configs(&ext_dir).await {
                Ok(configs) => configs,
                Err(e) => {
                    debug!("Skipping auto-start for {}: {}", ext_dir.display(), e);
                    continue;
                }
            };

            for config in configs {
                if !config.auto_start {
                    continue;
                }

                match self.start_server_config(&config, &ext_dir, ctx).await {
                    Ok(()) => {
                        info!("Auto-started MCP server '{}'", config.name);
                        started.push(config.name);
                    }
                    Err(e) => {
                        warn!("Failed to auto-start MCP server '{}': {}", config.name, e);
                    }
                }
            }
        }

        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_parse_sse_manifest() {
        let tmp = TempDir::new().unwrap();
        let ext_dir = tmp.path();

        let manifest = serde_json::json!({
            "mcp_servers": {
                "web-server": {
                    "transport": "sse",
                    "endpoint": "http://localhost:8080/sse",
                    "auto_start": true
                }
            }
        });
        tokio::fs::write(
            ext_dir.join("manifest.yaml"),
            serde_yaml::to_string(&manifest).unwrap(),
        )
        .await
        .unwrap();

        let starter = McpRuntimeStarter::new();
        let configs = starter.parse_server_configs(ext_dir).await.unwrap();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "web-server");
        assert_eq!(configs[0].transport, TransportType::Sse);
        assert_eq!(
            configs[0].endpoint,
            Some("http://localhost:8080/sse".to_string())
        );
        assert!(configs[0].auto_start);
    }

    #[tokio::test]
    async fn test_registry_manifest_preserves_sse_endpoint() {
        let tmp = TempDir::new().unwrap();
        let server_dir = tmp.path().join("web-server");
        std::fs::create_dir_all(&server_dir).unwrap();

        let server_json = serde_json::json!({
            "name": "web-server",
            "version": "1.0.0",
            "transport": {
                "type": "sse",
                "endpoint": "http://localhost:8080/sse"
            }
        });
        tokio::fs::write(server_dir.join("server.json"), server_json.to_string())
            .await
            .unwrap();

        let registry_manifest = McpRuntimeStarter::registry_manifest_to_mcp_servers(
            &server_json,
            &server_dir.join("server.json"),
        )
        .unwrap();

        let servers = registry_manifest.as_object().unwrap();
        let config = servers.get("web-server").unwrap();
        assert_eq!(config.get("transport").unwrap(), "sse");
        assert_eq!(config.get("endpoint").unwrap(), "http://localhost:8080/sse");
        assert_eq!(config.get("auto_start").unwrap(), true);
    }
}
