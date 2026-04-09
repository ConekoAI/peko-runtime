//! Channel Extension Adapter
//!
//! This adapter integrates channel I/O extensions with the Extension Core.
//! It enables extensions to:
//! - Transform outgoing messages (MessagePreSend hook)
//! - Transform incoming messages (MessagePostReceive hook)
//! - Register custom channel configurations
//!
//! Unlike Skill/Tool/MCP adapters, the Channel adapter focuses on I/O transformations
//! rather than adding capabilities. It wraps the existing Channel trait to inject
//! extension behavior at message boundaries.
//!
//! # Hook Points Used
//! - `MessagePreSend`: Transform messages before sending to channel
//! - `MessagePostReceive`: Transform messages after receiving from channel

use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter, HookBinding};
use crate::extensions::core::{
    HookContext, HookHandler, HookHandlerFactory, HookPoint,
};
use crate::extensions::types::{
    ExtensionManifest, HookInput, HookOutput, HookResult, MessageEnvelope,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Discovered channel extension
#[derive(Debug, Clone)]
pub struct DiscoveredChannel {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Channel configuration
    pub config: ChannelExtensionConfig,
}

/// Channel extension configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelExtensionConfig {
    /// Channel type identifier (e.g., "slack", "discord", "telegram")
    pub channel_type: String,
    /// Human-readable name
    pub name: String,
    /// Description
    #[serde(default)]
    pub description: String,
    /// Message transformers to apply
    #[serde(default)]
    pub transformers: Vec<MessageTransformerConfig>,
    /// Whether this extension handles input
    #[serde(default)]
    pub handles_input: bool,
    /// Whether this extension handles output
    #[serde(default)]
    pub handles_output: bool,
}

/// Message transformer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTransformerConfig {
    /// Transformer type: "pre_send", "post_receive"
    pub transform_type: String,
    /// Transformer name/pattern
    pub name: String,
    /// Priority (higher = earlier)
    #[serde(default = "default_transformer_priority")]
    pub priority: i32,
}

fn default_transformer_priority() -> i32 {
    100
}

/// Channel adapter for Extension system
pub struct ChannelAdapter {
    /// Extension core for hook registration
    core: Arc<crate::extensions::ExtensionCore>,
}

impl std::fmt::Debug for ChannelAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelAdapter")
            .field("core", &"<ExtensionCore>")
            .finish()
    }
}

impl ChannelAdapter {
    /// Create a new channel adapter
    pub fn new(core: Arc<crate::extensions::ExtensionCore>) -> Self {
        Self { core }
    }

    /// Register a message transformer
    pub async fn register_transformer(
        &self,
        extension_id: &crate::extensions::types::ExtensionId,
        transform_type: TransformType,
        handler: Arc<dyn HookHandler>,
    ) -> Result<()> {
        let hook_point = match transform_type {
            TransformType::PreSend => HookPoint::MessagePreSend,
            TransformType::PostReceive => HookPoint::MessagePostReceive,
        };

        let _ = self.core
            .register_hook(hook_point, handler, extension_id)
            .await
            .context("Failed to register message transformer")?;
        Ok(())
    }

    /// Transform a message before sending
    pub async fn transform_pre_send(
        &self,
        envelope: MessageEnvelope,
    ) -> Result<MessageEnvelope> {
        let input = HookInput::Message(envelope);
        let result = self.core.invoke_hook(HookPoint::MessagePreSend, input).await;

        match result {
            HookResult::Continue(HookOutput::Message(transformed)) | HookResult::Replace(HookOutput::Message(transformed)) => {
                debug!("Message transformed by pre_send hooks");
                Ok(MessageEnvelope::new(transformed))
            }
            HookResult::PassThrough | HookResult::Continue(_) | HookResult::Replace(_) => {
                Err(anyhow::anyhow!("Hook did not return transformed message"))
            }
            HookResult::Handled => {
                Err(anyhow::anyhow!("Hook consumed message without returning transformation"))
            }
            HookResult::Error(e) => {
                warn!("Pre-send transformation failed: {}", e);
                Err(e)
            }
        }
    }

    /// Transform a message after receiving
    pub async fn transform_post_receive(
        &self,
        envelope: MessageEnvelope,
    ) -> Result<MessageEnvelope> {
        let input = HookInput::Message(envelope);
        let result = self.core.invoke_hook(HookPoint::MessagePostReceive, input).await;

        match result {
            HookResult::Continue(HookOutput::Message(transformed)) | HookResult::Replace(HookOutput::Message(transformed)) => {
                debug!("Message transformed by post_receive hooks");
                Ok(MessageEnvelope::new(transformed))
            }
            HookResult::PassThrough | HookResult::Continue(_) | HookResult::Replace(_) => {
                Err(anyhow::anyhow!("Hook did not return transformed message"))
            }
            HookResult::Handled => {
                Err(anyhow::anyhow!("Hook consumed message without returning transformation"))
            }
            HookResult::Error(e) => {
                warn!("Post-receive transformation failed: {}", e);
                Err(e)
            }
        }
    }
}

#[async_trait]
impl ExtensionTypeAdapter for ChannelAdapter {
    fn extension_type(&self) -> &'static str {
        "channel"
    }

    fn manifest_format(&self) -> super::ManifestFormat {
        super::ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["id", "name", "channel_type"],
            file_name: "manifest.yaml",
        }
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        info!("Initializing channel extension: {}", manifest.id);

        let config: ChannelExtensionConfig =
            serde_json::from_value(serde_json::json!(manifest.metadata.clone()))
                .context("Failed to parse channel extension config")?;

        debug!("Channel config: {:?}", config);
        Ok(ExtensionState::Unit)
    }

    async fn shutdown(&self, _state: ExtensionState) -> Result<()> {
        Ok(())
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        let mut bindings = Vec::new();

        if let Ok(config) = serde_json::from_value::<ChannelExtensionConfig>(
            serde_json::json!(manifest.metadata.clone()),
        ) {
            for transformer in &config.transformers {
                let hook_point = match transformer.transform_type.as_str() {
                    "pre_send" => HookPoint::MessagePreSend,
                    "post_receive" => HookPoint::MessagePostReceive,
                    _ => {
                        warn!("Unknown transformer type: {}", transformer.transform_type);
                        continue;
                    }
                };

                bindings.push(HookBinding::new(
                    hook_point,
                    Box::new(MessageTransformerFactory {
                        transformer_name: transformer.name.clone(),
                        transform_type: match transformer.transform_type.as_str() {
                            "pre_send" => TransformType::PreSend,
                            "post_receive" => TransformType::PostReceive,
                            _ => TransformType::PreSend,
                        },
                    }),
                ));
            }
        }

        bindings
    }
}

/// Type of message transformation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformType {
    PreSend,
    PostReceive,
}

/// Factory for message transformer handlers
#[derive(Clone)]
struct MessageTransformerFactory {
    transformer_name: String,
    transform_type: TransformType,
}

impl std::fmt::Debug for MessageTransformerFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageTransformerFactory")
            .field("transformer_name", &self.transformer_name)
            .field("transform_type", &self.transform_type)
            .finish()
    }
}

#[async_trait]
impl HookHandlerFactory for MessageTransformerFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(MessageTransformerHandler {
            transformer_name: self.transformer_name.clone(),
            transform_type: self.transform_type,
        })
    }
}

/// Handler that transforms messages
#[derive(Clone)]
struct MessageTransformerHandler {
    transformer_name: String,
    transform_type: TransformType,
}

impl std::fmt::Debug for MessageTransformerHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageTransformerHandler")
            .field("transformer_name", &self.transformer_name)
            .field("transform_type", &self.transform_type)
            .finish()
    }
}

#[async_trait]
impl HookHandler for MessageTransformerHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        match ctx.input {
            HookInput::Message(envelope) => {
                debug!(
                    "Applying transformer {} for {:?}",
                    self.transformer_name, self.transform_type
                );
                HookResult::Continue(HookOutput::Message(envelope.content))
            }
            _ => {
                warn!("Message transformer received unexpected input type");
                HookResult::PassThrough
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        match self.transform_type {
            TransformType::PreSend => HookPoint::MessagePreSend,
            TransformType::PostReceive => HookPoint::MessagePostReceive,
        }
    }

    fn name(&self) -> String {
        format!("{}:{:?}", self.transformer_name, self.transform_type)
    }
}

/// Discover channel extensions in a directory
pub async fn discover_channel_extensions(dir: &Path) -> Result<Vec<DiscoveredChannel>> {
    let mut discovered = Vec::new();

    if !dir.exists() {
        debug!("Channel extensions directory does not exist: {}", dir.display());
        return Ok(discovered);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .context("Failed to read channel extensions directory")?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .context("Failed to read directory entry")?
    {
        let path = entry.path();
        
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("manifest.yaml");
        let manifest = if manifest_path.exists() {
            match tokio::fs::read_to_string(&manifest_path).await {
                Ok(content) => parse_channel_manifest(&content, &path),
                Err(e) => {
                    warn!("Failed to read manifest at {}: {}", manifest_path.display(), e);
                    continue;
                }
            }
        } else {
            let manifest_path = path.join("manifest.toml");
            if manifest_path.exists() {
                match tokio::fs::read_to_string(&manifest_path).await {
                    Ok(content) => parse_channel_manifest_toml(&content, &path),
                    Err(e) => {
                        warn!("Failed to read manifest at {}: {}", manifest_path.display(), e);
                        continue;
                    }
                }
            } else {
                None
            }
        };

        if let Some((manifest, config)) = manifest {
            discovered.push(DiscoveredChannel { manifest, config });
        }
    }

    info!("Discovered {} channel extensions", discovered.len());
    Ok(discovered)
}

/// Parse channel manifest from YAML content
fn parse_channel_manifest(content: &str, path: &Path) -> Option<(ExtensionManifest, ChannelExtensionConfig)> {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    
    let frontmatter = if parts.len() >= 2 {
        parts[1].trim()
    } else {
        content.trim()
    };

    let yaml: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;

    let id = yaml.get("id")?.as_str()?;
    let name = yaml.get("name")?.as_str()?;
    let version = yaml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = yaml.get("description")?.as_str().unwrap_or("");

    let config = ChannelExtensionConfig {
        channel_type: yaml.get("channel_type")?.as_str()?.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        transformers: Vec::new(),
        handles_input: yaml.get("handles_input").and_then(|v| v.as_bool()).unwrap_or(true),
        handles_output: yaml.get("handles_output").and_then(|v| v.as_bool()).unwrap_or(true),
    };

    let mut manifest = ExtensionManifest::new(
        id,
        "channel",
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

/// Parse channel manifest from TOML content
fn parse_channel_manifest_toml(content: &str, path: &Path) -> Option<(ExtensionManifest, ChannelExtensionConfig)> {
    let toml: toml::Value = toml::from_str(content).ok()?;

    let id = toml.get("id")?.as_str()?;
    let name = toml.get("name")?.as_str()?;
    let version = toml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = toml.get("description")?.as_str().unwrap_or("");

    let config = ChannelExtensionConfig {
        channel_type: toml.get("channel_type")?.as_str()?.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        transformers: Vec::new(),
        handles_input: toml.get("handles_input").and_then(|v| v.as_bool()).unwrap_or(true),
        handles_output: toml.get("handles_output").and_then(|v| v.as_bool()).unwrap_or(true),
    };

    let mut manifest = ExtensionManifest::new(
        id,
        "channel",
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

/// Register discovered channel extensions with the Extension Core
pub async fn register_channels_with_core(
    core: Arc<crate::extensions::ExtensionCore>,
    channels: Vec<DiscoveredChannel>,
) -> Result<usize> {
    let adapter = Arc::new(ChannelAdapter::new(core.clone()));
    let mut registered = 0;

    for channel in channels {
        let extension_id = channel.manifest.id.clone();
        
        for transformer in &channel.config.transformers {
            let transform_type = match transformer.transform_type.as_str() {
                "pre_send" => TransformType::PreSend,
                "post_receive" => TransformType::PostReceive,
                _ => continue,
            };

            let handler = Arc::new(MessageTransformerHandler {
                transformer_name: transformer.name.clone(),
                transform_type,
            });

            if let Err(e) = adapter.register_transformer(&extension_id, transform_type, handler).await {
                warn!("Failed to register transformer {}: {}", transformer.name, e);
            } else {
                debug!("Registered transformer: {}", transformer.name);
            }
        }

        registered += 1;
    }

    info!("Registered {} channel extensions", registered);
    Ok(registered)
}

/// Convenience function to discover and register channel extensions
pub async fn load_and_register_channels(
    core: Arc<crate::extensions::ExtensionCore>,
    channels_dir: &Path,
) -> Result<usize> {
    let channels = discover_channel_extensions(channels_dir).await?;
    register_channels_with_core(core, channels).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_type() {
        assert_eq!(format!("{:?}", TransformType::PreSend), "PreSend");
        assert_eq!(format!("{:?}", TransformType::PostReceive), "PostReceive");
    }

    #[test]
    fn test_channel_extension_config_defaults() {
        let config = ChannelExtensionConfig {
            channel_type: "slack".to_string(),
            name: "Slack".to_string(),
            description: "Slack integration".to_string(),
            transformers: vec![],
            handles_input: true,
            handles_output: true,
        };

        assert_eq!(config.channel_type, "slack");
        assert!(config.handles_input);
        assert!(config.handles_output);
    }
}
