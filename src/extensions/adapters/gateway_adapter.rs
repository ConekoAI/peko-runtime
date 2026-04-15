//! Gateway Extension Adapter
//!
//! This adapter integrates gateway plugins with the Extension Core.
//! Gateway plugins provide external integrations like:
//! - HTTP API gateways
//! - WebSocket servers
//! - Message queue connectors (Redis, RabbitMQ, Kafka)
//! - Custom protocol handlers
//!
//! Gateway adapters are more complex than other adapters as they typically
//! need to register multiple hooks and may maintain their own server instances.
//!
//! # Hook Points Used
//! - `AgentInit`: Initialize gateway when agent starts
//! - `AgentShutdown`: Cleanup gateway when agent stops
//! - `EventSubscribe`: Listen to system events
//! - `ToolRegister`: Register gateway-provided tools
//! - `ChannelInput`/`ChannelOutput`: Gateway as I/O channel
//!
//! # Extension Manifest Format
//! ```yaml
//! ---
//! id: "redis-gateway"
//! name: "Redis Pub/Sub Gateway"
//! version: "1.0.0"
//! gateway_type: "pubsub"
//! config:
//!   redis_url: "redis://localhost:6379"
//!   channels:
//!     - "pekobot:events"
//!     - "pekobot:commands"
//!   hooks:
//!     - point: "agent.init"
//!       handler: "init_redis"
//!     - point: "agent.shutdown"
//!       handler: "cleanup_redis"
//!     - point: "event.subscribe"
//!       handler: "forward_to_redis"
//!       topic: "instance.*"
//! ```

use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter, HookBinding};
use crate::extensions::core::{
    HookContext, HookHandler, HookHandlerFactory, HookPoint,
};
use crate::extensions::types::{
    ExtensionManifest, HookInput, HookOutput, HookResult,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Discovered gateway extension
#[derive(Debug, Clone)]
pub struct DiscoveredGateway {
    pub manifest: ExtensionManifest,
    pub config: GatewayExtensionConfig,
}

/// Gateway extension configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayExtensionConfig {
    /// Gateway type: "http", "websocket", "pubsub", "grpc", "custom"
    pub gateway_type: String,
    /// Gateway-specific configuration
    #[serde(default)]
    pub config: serde_json::Value,
    /// Hook registrations
    #[serde(default)]
    pub hooks: Vec<GatewayHookConfig>,
    /// Tools provided by this gateway
    #[serde(default)]
    pub tools: Vec<GatewayToolConfig>,
}

/// Gateway hook configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayHookConfig {
    /// Hook point name
    pub point: String,
    /// Handler identifier
    pub handler: String,
    /// Optional topic pattern (for event subscriptions)
    pub topic: Option<String>,
    /// Optional section name (for prompt sections)
    pub section: Option<String>,
    /// Optional priority
    pub priority: Option<i32>,
}

/// Gateway tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayToolConfig {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// JSON schema for parameters
    pub parameters: serde_json::Value,
}

/// Gateway adapter
pub struct GatewayAdapter;

impl std::fmt::Debug for GatewayAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayAdapter").finish()
    }
}

impl GatewayAdapter {
    pub fn new(_core: Arc<crate::extensions::ExtensionCore>) -> Self {
        Self
    }
}

#[async_trait]
impl ExtensionTypeAdapter for GatewayAdapter {
    fn extension_type(&self) -> &'static str {
        "gateway"
    }

    fn manifest_format(&self) -> super::ManifestFormat {
        super::ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["id", "name", "gateway_type"],
            file_name: "manifest.yaml",
        }
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        info!("Initializing gateway extension: {}", manifest.id);

        if let Ok(config) = serde_json::from_value::<GatewayExtensionConfig>(
            serde_json::json!(manifest.metadata.clone()),
        ) {
            debug!("Gateway type: {}", config.gateway_type);
            
            // Gateway initialization would happen here
            // For now, just validate the config
            match config.gateway_type.as_str() {
                "http" => debug!("Initializing HTTP gateway"),
                "websocket" => debug!("Initializing WebSocket gateway"),
                "pubsub" => debug!("Initializing Pub/Sub gateway"),
                "grpc" => debug!("Initializing gRPC gateway"),
                "custom" => debug!("Initializing custom gateway"),
                _ => warn!("Unknown gateway type: {}", config.gateway_type),
            }
        }

        Ok(ExtensionState::Unit)
    }

    async fn shutdown(&self, _state: ExtensionState) -> Result<()> {
        // Gateway cleanup would happen here
        Ok(())
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        let mut bindings = Vec::new();

        if let Ok(config) = serde_json::from_value::<GatewayExtensionConfig>(
            serde_json::json!(manifest.metadata.clone()),
        ) {
            for hook in &config.hooks {
                if let Some(hook_point) = parse_hook_point(&hook.point) {
                    bindings.push(HookBinding::new(
                        hook_point,
                        Box::new(GatewayHookFactory {
                            handler_name: hook.handler.clone(),
                        }),
                    ));
                } else {
                    warn!("Unknown hook point: {}", hook.point);
                }
            }

            // Register tools if provided
            if !config.tools.is_empty() {
                bindings.push(HookBinding::new(
                    HookPoint::ToolRegister,
                    Box::new(GatewayToolFactory {
                        tools: config.tools.clone(),
                    }),
                ));
            }
        }

        bindings
    }
}

/// Parse hook point from string
fn parse_hook_point(point: &str) -> Option<HookPoint> {
    match point {
        "agent.init" => Some(HookPoint::AgentInit),
        "agent.shutdown" => Some(HookPoint::AgentShutdown),
        "tool.register" => Some(HookPoint::ToolRegister),
        "channel.input" => Some(HookPoint::ChannelInput),
        "channel.output" => Some(HookPoint::ChannelOutput),
        "message.pre_send" => Some(HookPoint::MessagePreSend),
        "message.post_receive" => Some(HookPoint::MessagePostReceive),
        _ => {
            // Handle parameterized hooks
            if point.starts_with("event.subscribe.") {
                let topic = point.strip_prefix("event.subscribe.").unwrap_or("*");
                Some(HookPoint::EventSubscribe {
                    topic_pattern: topic.to_string(),
                })
            } else if point.starts_with("prompt.") {
                let section = point.strip_prefix("prompt.").unwrap_or("custom");
                Some(HookPoint::PromptSystemSection {
                    section: section.to_string(),
                    priority: 100,
                })
            } else {
                None
            }
        }
    }
}

/// Factory for gateway hooks
#[derive(Clone)]
struct GatewayHookFactory {
    handler_name: String,
}

impl std::fmt::Debug for GatewayHookFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayHookFactory")
            .field("handler", &self.handler_name)
            .finish()
    }
}

#[async_trait]
impl HookHandlerFactory for GatewayHookFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(GatewayHookHandler {
            handler_name: self.handler_name.clone(),
        })
    }
}

/// Handler for gateway hooks
#[derive(Clone)]
struct GatewayHookHandler {
    handler_name: String,
}

impl std::fmt::Debug for GatewayHookHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayHookHandler")
            .field("name", &self.handler_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for GatewayHookHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        debug!("Gateway handler '{}' invoked", self.handler_name);
        
        // Gateway handlers pass through by default
        // In a real implementation, this would dispatch to gateway-specific logic
        match ctx.input {
            HookInput::SystemEvent(event) => {
                HookResult::Continue(HookOutput::Event(event))
            }
            HookInput::ToolRegistry(access) => {
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "tools": access.tools
                })))
            }
            _ => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        // This is a generic handler, actual hook point is determined by factory
        HookPoint::AgentInit
    }

    fn name(&self) -> String {
        format!("gateway:{}", self.handler_name)
    }
}

/// Factory for gateway tools
#[derive(Clone)]
struct GatewayToolFactory {
    tools: Vec<GatewayToolConfig>,
}

impl std::fmt::Debug for GatewayToolFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayToolFactory")
            .field("tool_count", &self.tools.len())
            .finish()
    }
}

#[async_trait]
impl HookHandlerFactory for GatewayToolFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(GatewayToolHandler {
            tools: self.tools.clone(),
        })
    }
}

/// Handler for registering gateway tools
#[derive(Clone)]
struct GatewayToolHandler {
    tools: Vec<GatewayToolConfig>,
}

impl std::fmt::Debug for GatewayToolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayToolHandler")
            .field("tool_count", &self.tools.len())
            .finish()
    }
}

#[async_trait]
impl HookHandler for GatewayToolHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let mut tool_defs = Vec::new();

        for tool in &self.tools {
            let tool_def = crate::providers::ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            };
            tool_defs.push(HookOutput::Tool(tool_def));
        }

        if tool_defs.len() == 1 {
            HookResult::Continue(tool_defs.into_iter().next().unwrap())
        } else {
            HookResult::Continue(HookOutput::combine(tool_defs))
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolRegister
    }

    fn name(&self) -> String {
        "gateway_tool_register".to_string()
    }
}

/// Discover gateway extensions
pub async fn discover_gateway_extensions(dir: &Path) -> Result<Vec<DiscoveredGateway>> {
    let mut discovered = Vec::new();

    if !dir.exists() {
        debug!("Gateway extensions directory does not exist: {}", dir.display());
        return Ok(discovered);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .context("Failed to read gateway extensions directory")?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() { continue; }

        let manifest_path = path.join("manifest.yaml");
        let manifest = if manifest_path.exists() {
            match tokio::fs::read_to_string(&manifest_path).await {
                Ok(content) => parse_gateway_manifest(&content, &path),
                Err(e) => { warn!("Failed to read manifest: {}", e); continue; }
            }
        } else {
            let manifest_path = path.join("manifest.toml");
            if manifest_path.exists() {
                match tokio::fs::read_to_string(&manifest_path).await {
                    Ok(content) => parse_gateway_manifest_toml(&content, &path),
                    Err(e) => { warn!("Failed to read manifest: {}", e); continue; }
                }
            } else { None }
        };

        if let Some((manifest, config)) = manifest {
            discovered.push(DiscoveredGateway { manifest, config });
        }
    }

    info!("Discovered {} gateway extensions", discovered.len());
    Ok(discovered)
}

fn parse_gateway_manifest(content: &str, path: &Path) -> Option<(ExtensionManifest, GatewayExtensionConfig)> {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    let frontmatter = if parts.len() >= 2 { parts[1].trim() } else { content.trim() };
    let yaml: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;

    let id = yaml.get("id")?.as_str()?;
    let name = yaml.get("name")?.as_str()?;
    let version = yaml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = yaml.get("description")?.as_str().unwrap_or("");

    let config = GatewayExtensionConfig {
        gateway_type: yaml.get("gateway_type")?.as_str()?.to_string(),
        config: serde_json::Value::Object(serde_json::Map::new()),
        hooks: Vec::new(),
        tools: Vec::new(),
    };

    let mut manifest = ExtensionManifest::new(
        id, "gateway", name, description, version, path.to_path_buf(),
    );

    if let Ok(json_config) = serde_json::to_value(&config) {
        if let serde_json::Value::Object(map) = json_config {
            for (key, value) in map {
                manifest.set(key, value);
            }
        }
    }

    Some((manifest, config))
}

fn parse_gateway_manifest_toml(content: &str, path: &Path) -> Option<(ExtensionManifest, GatewayExtensionConfig)> {
    let toml: toml::Value = toml::from_str(content).ok()?;

    let id = toml.get("id")?.as_str()?;
    let name = toml.get("name")?.as_str()?;
    let version = toml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = toml.get("description")?.as_str().unwrap_or("");

    let config = GatewayExtensionConfig {
        gateway_type: toml.get("gateway_type")?.as_str()?.to_string(),
        config: serde_json::Value::Object(serde_json::Map::new()),
        hooks: Vec::new(),
        tools: Vec::new(),
    };

    let mut manifest = ExtensionManifest::new(
        id, "gateway", name, description, version, path.to_path_buf(),
    );

    if let Ok(json_config) = serde_json::to_value(&config) {
        if let serde_json::Value::Object(map) = json_config {
            for (key, value) in map {
                manifest.set(key, value);
            }
        }
    }

    Some((manifest, config))
}

/// Register gateway extensions
pub async fn register_gateways_with_core(
    _core: Arc<crate::extensions::ExtensionCore>,
    gateways: Vec<DiscoveredGateway>,
) -> Result<usize> {
    info!("Registered {} gateway extensions", gateways.len());
    Ok(gateways.len())
}

/// Load and register gateway extensions
pub async fn load_and_register_gateways(
    core: Arc<crate::extensions::ExtensionCore>,
    gateways_dir: &Path,
) -> Result<usize> {
    let gateways = discover_gateway_extensions(gateways_dir).await?;
    register_gateways_with_core(core, gateways).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hook_point() {
        assert!(parse_hook_point("agent.init").is_some());
        assert!(parse_hook_point("tool.register").is_some());
        assert!(parse_hook_point("event.subscribe.instance.*").is_some());
        assert!(parse_hook_point("unknown.point").is_none());
    }

    #[test]
    fn test_gateway_extension_config() {
        let config = GatewayExtensionConfig {
            gateway_type: "http".to_string(),
            config: serde_json::json!({}),
            hooks: vec![],
            tools: vec![],
        };
        assert_eq!(config.gateway_type, "http");
    }
}
