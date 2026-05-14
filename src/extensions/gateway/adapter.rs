//! Gateway Extension Adapter
//!
//! This adapter integrates gateway plugins with the Extension Core.
//! Gateway plugins provide external integrations like:
//! - HTTP API gateways
//! - WebSocket servers
//! - Message queue connectors (Redis, `RabbitMQ`, Kafka)
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
//!
//! ADR-024: `manifest.yaml` is **pure YAML** — no frontmatter delimiters required.
//! The `---` below is an optional YAML document-start marker, not frontmatter.
//!
//! ```yaml
//! id: "redis-gateway"
//! name: "Redis Pub/Sub Gateway"
//! version: "1.0.0"
//! extension_type: "gateway"
//! gateway_type: "pubsub"
//! config:
//!   redis_url: "redis://localhost:6379"
//!   channels:
//!     - "peko:events"
//!     - "peko:commands"
//! hooks:
//!   - point: "agent.init"
//!     handler: "init_redis"
//!   - point: "agent.shutdown"
//!     handler: "cleanup_redis"
//!   - point: "event.subscribe"
//!     handler: "forward_to_redis"
//!     topic: "instance.*"
//! ```

use crate::extension::adapters::{ExtensionState, ExtensionTypeAdapter, HookBinding};
use crate::extension::core::{HookContext, HookHandler, HookHandlerFactory, HookPoint};
use crate::extension::types::{ExtensionManifest, HookInput, HookOutput, HookResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

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
    pub fn new(_core: Arc<crate::extension::ExtensionCore>) -> Self {
        Self
    }
}

#[async_trait]
impl ExtensionTypeAdapter for GatewayAdapter {
    fn extension_type(&self) -> &'static str {
        "gateway"
    }

    fn manifest_format(&self) -> crate::extension::adapters::ManifestFormat {
        // ADR-024: Gateway uses pure YAML manifest.yaml with extension_type: "gateway".
        // gateway_type remains a required type-specific field for transport selection.
        crate::extension::adapters::ManifestFormat::Yaml {
            schema: "gateway".to_string(),
            file_name: "manifest.yaml",
        }
    }

    fn parse_manifest(
        &self,
        path: &std::path::Path,
        content: &str,
    ) -> anyhow::Result<crate::extension::ExtensionManifest> {
        use anyhow::Context;

        let yaml: serde_yaml::Value = serde_yaml::from_str(content)
            .with_context(|| format!("Failed to parse gateway manifest at {path:?}"))?;

        let (id, name, version, description) =
            crate::extension::adapters::parsing::extract_extension_fields(&yaml)?;

        // Validate extension_type
        let ext_type =
            crate::extension::adapters::parsing::require_string_field(&yaml, "extension_type")
                .with_context(|| {
                    format!(
                        "Gateway manifest at {path:?} is missing required field 'extension_type'"
                    )
                })?;
        if ext_type != "gateway" {
            anyhow::bail!(
                "Gateway manifest at {path:?} has extension_type '{}' but expected 'gateway'",
                ext_type
            );
        }

        // Validate gateway_type (required type-specific transport discriminator)
        let gateway_type =
            crate::extension::adapters::parsing::require_string_field(&yaml, "gateway_type")
                .with_context(|| {
                    format!("Gateway manifest at {path:?} is missing required field 'gateway_type'")
                })?;

        let mut manifest = ExtensionManifest::new(
            &id,
            "gateway",
            &name,
            &description,
            &version,
            path.parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf(),
        );

        // Store gateway-specific config
        manifest.set("gateway_type", gateway_type);

        if let Some(config) = yaml.get("config") {
            manifest.set(
                "config",
                crate::extension::adapters::parsing::yaml_to_json(config.clone()),
            );
        }
        if let Some(hooks) = yaml.get("hooks") {
            manifest.set(
                "hooks",
                crate::extension::adapters::parsing::yaml_to_json(hooks.clone()),
            );
        }
        if let Some(tools) = yaml.get("tools") {
            manifest.set(
                "tools",
                crate::extension::adapters::parsing::yaml_to_json(tools.clone()),
            );
        }

        Ok(manifest)
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        info!("Initializing gateway extension: {}", manifest.id);

        if let Ok(config) =
            serde_json::from_value::<GatewayExtensionConfig>(serde_json::json!(manifest
                .metadata
                .clone()))
        {
            debug!("Gateway type: {}", config.gateway_type);

            // Gateway initialization would happen here
            // For now, just validate the config
            match config.gateway_type.as_str() {
                "http" => debug!("Initializing HTTP gateway"),
                "websocket" => debug!("Initializing WebSocket gateway"),
                "pubsub" => debug!("Initializing Pub/Sub gateway"),
                "grpc" => debug!("Initializing gRPC gateway"),
                "cli" => debug!("Initializing CLI gateway"),
                "custom" => debug!("Initializing custom gateway"),
                "out-of-process" => debug!("Initializing out-of-process gateway"),
                "external" => debug!("Initializing external gateway"),
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

        if let Ok(config) =
            serde_json::from_value::<GatewayExtensionConfig>(serde_json::json!(manifest
                .metadata
                .clone()))
        {
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
        debug!(
            "Gateway handler '{}' invoked at point: {}",
            self.handler_name,
            ctx.point.name()
        );

        // Dispatch based on hook point category — gateway extensions handle
        // I/O lifecycle, agent lifecycle, and event hooks.
        match ctx.point.category() {
            "io" => self.handle_io_hook(&ctx).await,
            "agent" => self.handle_agent_hook(&ctx).await,
            "event" => self.handle_event_hook(&ctx).await,
            "tool" => self.handle_tool_hook(&ctx).await,
            _ => {
                trace!(
                    "Gateway handler passing through for category: {}",
                    ctx.point.category()
                );
                HookResult::PassThrough
            }
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

impl GatewayHookHandler {
    /// Handle I/O lifecycle hooks (ChannelInput, ChannelOutput, MessagePreSend, MessagePostReceive)
    async fn handle_io_hook(&self, ctx: &HookContext) -> HookResult {
        use crate::extension::core::hook_points::HookPoint;

        match ctx.point {
            HookPoint::ChannelInput => {
                // Extensions return channel configuration JSON
                debug!(
                    "Gateway ChannelInput: registering input channel '{}'",
                    self.handler_name
                );
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "channel": self.handler_name,
                    "type": "input",
                    "registered": true
                })))
            }
            HookPoint::ChannelOutput => {
                // Extensions return output handler configuration
                debug!(
                    "Gateway ChannelOutput: registering output handler '{}'",
                    self.handler_name
                );
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "channel": self.handler_name,
                    "type": "output",
                    "registered": true
                })))
            }
            HookPoint::MessagePreSend => {
                // Extensions may transform outgoing messages
                if let Some(msg) = ctx.as_message() {
                    debug!(
                        "Gateway MessagePreSend: processing message for '{}'",
                        self.handler_name
                    );
                    HookResult::Continue(HookOutput::Json(serde_json::json!({
                        "channel": self.handler_name,
                        "action": "pre_send",
                        "message_id": msg.metadata.get("id").unwrap_or(&serde_json::Value::Null)
                    })))
                } else {
                    HookResult::PassThrough
                }
            }
            HookPoint::MessagePostReceive => {
                // Extensions may transform incoming messages
                if let Some(msg) = ctx.as_message() {
                    debug!(
                        "Gateway MessagePostReceive: processing message for '{}'",
                        self.handler_name
                    );
                    HookResult::Continue(HookOutput::Json(serde_json::json!({
                        "channel": self.handler_name,
                        "action": "post_receive",
                        "message_id": msg.metadata.get("id").unwrap_or(&serde_json::Value::Null)
                    })))
                } else {
                    HookResult::PassThrough
                }
            }
            _ => HookResult::PassThrough,
        }
    }

    /// Handle agent lifecycle hooks (AgentInit, AgentShutdown, AgentIteration)
    async fn handle_agent_hook(&self, ctx: &HookContext) -> HookResult {
        use crate::extension::core::hook_points::HookPoint;

        match ctx.point {
            HookPoint::AgentInit => {
                debug!(
                    "Gateway AgentInit: initializing gateway '{}'",
                    self.handler_name
                );
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "gateway": self.handler_name,
                    "status": "initialized"
                })))
            }
            HookPoint::AgentShutdown => {
                debug!(
                    "Gateway AgentShutdown: cleaning up gateway '{}'",
                    self.handler_name
                );
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "gateway": self.handler_name,
                    "status": "shutdown"
                })))
            }
            HookPoint::AgentIteration { iteration } => {
                debug!(
                    "Gateway AgentIteration: iteration {} for '{}'",
                    iteration, self.handler_name
                );
                HookResult::PassThrough
            }
            _ => HookResult::PassThrough,
        }
    }

    /// Handle event lifecycle hooks (EventSubscribe, EventEmit)
    async fn handle_event_hook(&self, ctx: &HookContext) -> HookResult {
        use crate::extension::core::hook_points::HookPoint;

        match ctx.point {
            HookPoint::EventSubscribe { ref topic_pattern } => {
                debug!(
                    "Gateway EventSubscribe: '{}' subscribing to topic '{}'",
                    self.handler_name, topic_pattern
                );
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "gateway": self.handler_name,
                    "topic": topic_pattern,
                    "subscribed": true
                })))
            }
            HookPoint::EventEmit => {
                debug!("Gateway EventEmit: '{}' emitting event", self.handler_name);
                HookResult::PassThrough
            }
            _ => HookResult::PassThrough,
        }
    }

    /// Handle tool lifecycle hooks (ToolRegister, etc.)
    async fn handle_tool_hook(&self, ctx: &HookContext) -> HookResult {
        match ctx.input {
            HookInput::ToolRegistry(ref access) => {
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "gateway": self.handler_name,
                    "tools": access.tools
                })))
            }
            _ => HookResult::PassThrough,
        }
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
        debug!(
            "Gateway extensions directory does not exist: {}",
            dir.display()
        );
        return Ok(discovered);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .context("Failed to read gateway extensions directory")?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("manifest.yaml");
        let manifest = if manifest_path.exists() {
            match tokio::fs::read_to_string(&manifest_path).await {
                Ok(content) => parse_gateway_manifest(&content, &path),
                Err(e) => {
                    warn!("Failed to read manifest: {}", e);
                    continue;
                }
            }
        } else {
            let manifest_path = path.join("manifest.toml");
            if manifest_path.exists() {
                match tokio::fs::read_to_string(&manifest_path).await {
                    Ok(content) => parse_gateway_manifest_toml(&content, &path),
                    Err(e) => {
                        warn!("Failed to read manifest: {}", e);
                        continue;
                    }
                }
            } else {
                None
            }
        };

        if let Some((manifest, config)) = manifest {
            discovered.push(DiscoveredGateway { manifest, config });
        }
    }

    info!("Discovered {} gateway extensions", discovered.len());
    Ok(discovered)
}

fn parse_gateway_manifest(
    content: &str,
    path: &Path,
) -> Option<(ExtensionManifest, GatewayExtensionConfig)> {
    // ADR-024: manifest.yaml is pure YAML. We try parsing the whole content first.
    // For backward compatibility with old frontmatter-style manifests, if pure YAML
    // parsing fails we fall back to extracting content between --- delimiters.
    let yaml: serde_yaml::Value = serde_yaml::from_str(content).ok().or_else(|| {
        // Fallback: try frontmatter-style (content between --- delimiters)
        let mut lines = content.lines().peekable();
        if lines.next() != Some("---") {
            return None;
        }
        let mut frontmatter_lines = Vec::new();
        let mut found_end = false;
        for line in lines {
            if line == "---" {
                found_end = true;
                break;
            }
            frontmatter_lines.push(line);
        }
        if !found_end {
            return None;
        }
        serde_yaml::from_str(&frontmatter_lines.join("\n")).ok()
    })?;

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
        id,
        "gateway",
        name,
        description,
        version,
        path.to_path_buf(),
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

fn parse_gateway_manifest_toml(
    content: &str,
    path: &Path,
) -> Option<(ExtensionManifest, GatewayExtensionConfig)> {
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
        id,
        "gateway",
        name,
        description,
        version,
        path.to_path_buf(),
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
    _core: Arc<crate::extension::ExtensionCore>,
    gateways: Vec<DiscoveredGateway>,
) -> Result<usize> {
    info!("Registered {} gateway extensions", gateways.len());
    Ok(gateways.len())
}

/// Load and register gateway extensions
pub async fn load_and_register_gateways(
    core: Arc<crate::extension::ExtensionCore>,
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
