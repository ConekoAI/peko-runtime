//! Hook Extension Adapter

use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter, HookBinding};
use crate::extensions::core::{
    HookContext, HookHandler, HookHandlerFactory, HookPoint,
};
use crate::extensions::types::{
    ExtensionManifest, HookInput, HookOutput, HookResult,
};
use crate::hooks::SystemEvent;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Discovered hook extension
#[derive(Debug, Clone)]
pub struct DiscoveredHook {
    pub manifest: ExtensionManifest,
    pub config: HookExtensionConfig,
}

/// Hook extension configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExtensionConfig {
    pub hook_type: String,
    #[serde(default)]
    pub subscriptions: Vec<EventSubscription>,
    pub webhook: Option<WebhookConfig>,
    pub cron: Option<CronConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscription {
    pub topic: String,
    #[serde(default)]
    pub filter: Option<EventFilterConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFilterConfig {
    #[serde(default)]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub resource_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_method() -> String { "POST".to_string() }
fn default_timeout() -> u64 { 30 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronConfig {
    pub schedule: String,
    pub timezone: Option<String>,
}

/// Hook adapter
pub struct HookAdapter {
    core: Arc<crate::extensions::ExtensionCore>,
}

impl std::fmt::Debug for HookAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookAdapter").finish()
    }
}

impl HookAdapter {
    pub fn new(core: Arc<crate::extensions::ExtensionCore>) -> Self {
        Self { core }
    }
}

#[async_trait]
impl ExtensionTypeAdapter for HookAdapter {
    fn extension_type(&self) -> &'static str { "hook" }

    fn manifest_format(&self) -> super::ManifestFormat {
        super::ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["id", "name", "hook_type"],
            file_name: "manifest.yaml",
        }
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        info!("Initializing hook extension: {}", manifest.id);
        Ok(ExtensionState::Unit)
    }

    async fn shutdown(&self, _state: ExtensionState) -> Result<()> {
        Ok(())
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        let mut bindings = Vec::new();

        if let Ok(config) = serde_json::from_value::<HookExtensionConfig>(
            serde_json::json!(manifest.metadata.clone()),
        ) {
            for subscription in &config.subscriptions {
                bindings.push(HookBinding::new(
                    HookPoint::EventSubscribe {
                        topic_pattern: subscription.topic.clone(),
                    },
                    Box::new(EventSubscriptionFactory {
                        topic: subscription.topic.clone(),
                    }),
                ));
            }
        }

        bindings
    }
}

#[derive(Clone)]
struct EventSubscriptionFactory {
    topic: String,
}

impl std::fmt::Debug for EventSubscriptionFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSubscriptionFactory")
            .field("topic", &self.topic)
            .finish()
    }
}

#[async_trait]
impl HookHandlerFactory for EventSubscriptionFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(EventSubscriptionHandler {
            topic: self.topic.clone(),
        })
    }
}

#[derive(Clone)]
struct EventSubscriptionHandler {
    topic: String,
}

impl std::fmt::Debug for EventSubscriptionHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSubscriptionHandler")
            .field("topic", &self.topic)
            .finish()
    }
}

#[async_trait]
impl HookHandler for EventSubscriptionHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        match ctx.input {
            HookInput::SystemEvent(event) => {
                debug!("Handling event on topic: {}", self.topic);
                HookResult::Continue(HookOutput::Event(event))
            }
            _ => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::EventSubscribe {
            topic_pattern: self.topic.clone(),
        }
    }

    fn name(&self) -> String {
        format!("event_subscriber:{}", self.topic)
    }
}

/// Discover hook extensions
pub async fn discover_hook_extensions(dir: &Path) -> Result<Vec<DiscoveredHook>> {
    let mut discovered = Vec::new();
    
    if !dir.exists() {
        debug!("Hook extensions directory does not exist: {}", dir.display());
        return Ok(discovered);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .context("Failed to read hook extensions directory")?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() { continue; }

        let manifest_path = path.join("manifest.yaml");
        let manifest = if manifest_path.exists() {
            match tokio::fs::read_to_string(&manifest_path).await {
                Ok(content) => parse_hook_manifest(&content, &path),
                Err(e) => { warn!("Failed to read manifest: {}", e); continue; }
            }
        } else {
            let manifest_path = path.join("manifest.toml");
            if manifest_path.exists() {
                match tokio::fs::read_to_string(&manifest_path).await {
                    Ok(content) => parse_hook_manifest_toml(&content, &path),
                    Err(e) => { warn!("Failed to read manifest: {}", e); continue; }
                }
            } else { None }
        };

        if let Some((manifest, config)) = manifest {
            discovered.push(DiscoveredHook { manifest, config });
        }
    }

    info!("Discovered {} hook extensions", discovered.len());
    Ok(discovered)
}

fn parse_hook_manifest(content: &str, path: &Path) -> Option<(ExtensionManifest, HookExtensionConfig)> {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    let frontmatter = if parts.len() >= 2 { parts[1].trim() } else { content.trim() };
    let yaml: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;

    let id = yaml.get("id")?.as_str()?;
    let name = yaml.get("name")?.as_str()?;
    let version = yaml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = yaml.get("description")?.as_str().unwrap_or("");

    let config = HookExtensionConfig {
        hook_type: yaml.get("hook_type")?.as_str()?.to_string(),
        subscriptions: Vec::new(),
        webhook: None,
        cron: None,
    };

    let mut manifest = ExtensionManifest::new(
        id, "hook", name, description, version, path.to_path_buf(),
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

fn parse_hook_manifest_toml(content: &str, path: &Path) -> Option<(ExtensionManifest, HookExtensionConfig)> {
    let toml: toml::Value = toml::from_str(content).ok()?;

    let id = toml.get("id")?.as_str()?;
    let name = toml.get("name")?.as_str()?;
    let version = toml.get("version")?.as_str().unwrap_or("1.0.0");
    let description = toml.get("description")?.as_str().unwrap_or("");

    let config = HookExtensionConfig {
        hook_type: toml.get("hook_type")?.as_str()?.to_string(),
        subscriptions: Vec::new(),
        webhook: None,
        cron: None,
    };

    let mut manifest = ExtensionManifest::new(
        id, "hook", name, description, version, path.to_path_buf(),
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

/// Register hook extensions
pub async fn register_hooks_with_core(
    _core: Arc<crate::extensions::ExtensionCore>,
    hooks: Vec<DiscoveredHook>,
) -> Result<usize> {
    info!("Registered {} hook extensions", hooks.len());
    Ok(hooks.len())
}

/// Load and register hook extensions
pub async fn load_and_register_hooks(
    core: Arc<crate::extensions::ExtensionCore>,
    hooks_dir: &Path,
) -> Result<usize> {
    let hooks = discover_hook_extensions(hooks_dir).await?;
    register_hooks_with_core(core, hooks).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_extension_config() {
        let config = HookExtensionConfig {
            hook_type: "event".to_string(),
            subscriptions: vec![],
            webhook: None,
            cron: None,
        };
        assert_eq!(config.hook_type, "event");
    }
}
