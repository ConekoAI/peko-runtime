//! General Extension Adapter
//!
//! This adapter provides unconstrained access to all 22 hook points in the Extension Core.
//! Unlike type-specific adapters (Skill, MCP, etc.), the general adapter allows extensions
//! to declare any combination of hook points directly in their manifest.
//!
//! # Use Cases
//!
//! - Complex integrations that need multiple hook types (e.g., prompt + tools + events)
//! - Custom extensions that don't fit into standard categories
//! - Power users who need maximum flexibility
//!
//! # Manifest Format
//!
//! ```yaml
//! ---
//! id: "advanced-deploy-helper"
//! name: "Advanced Deployment Helper"
//! version: "1.0.0"
//! extension_type: "general"
//!
//! hooks:
//!   - point: "prompt.system_section"
//!     section: "deployment"
//!     priority: 100
//!     handler: "generate_deployment_guide"
//!
//!   - point: "tool.execute"
//!     tool_name: "deploy:*"
//!     handler: "handle_deploy_tool"
//!
//!   - point: "event.subscribe"
//!     topic_pattern: "instance.created"
//!     handler: "on_instance_created"
//! ```
//!
//! # Hook Point Reference
//!
//! All 22 hook points are available:
//!
//! ## Prompt Lifecycle
//! - `prompt.system_section` - Inject into system prompt (params: section, priority)
//! - `prompt.pre_process` - Modify messages before LLM
//! - `prompt.post_process` - Transform LLM response
//!
//! ## Tool Lifecycle
//! - `tool.register` - Register tools for native calling
//! - `tool.execute` - Intercept tool execution (params: tool_name)
//! - `tool.execute_async` - Async tool execution (params: tool_name)
//! - `tool.check_status` - Check async task status (params: tool_name)
//! - `tool.cancel` - Cancel async task (params: tool_name)
//! - `tool.result_transform` - Modify tool results
//!
//! ## Session Lifecycle
//! - `session.state_change` - Session creation/update/compaction
//! - `session.compaction` - Custom compaction strategies
//! - `session.context_build` - Modify context window
//!
//! ## I/O Lifecycle
//! - `io.channel_input` - Register input channels
//! - `io.channel_output` - Register output handlers
//! - `io.message_pre_send` - Transform outgoing messages
//! - `io.message_post_receive` - Transform incoming messages
//!
//! ## Event Lifecycle
//! - `event.subscribe` - Subscribe to system events (params: topic_pattern)
//! - `event.emit` - Emit custom events
//!
//! ## Agent Lifecycle
//! - `agent.init` - Agent startup hook
//! - `agent.shutdown` - Agent shutdown hook
//! - `agent.iteration` - Between loop iterations (params: iteration)

use crate::extensions::adapters::parsing;
use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter, HookBinding};
use crate::extensions::core::{HookContext, HookHandler, HookHandlerFactory, HookPoint};
use crate::extensions::types::{ExtensionManifest, HookInput, HookOutput, HookResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// General extension type identifier
pub const GENERAL_EXTENSION_TYPE: &str = "general";

/// Default priority for general extension hooks
pub const GENERAL_HOOK_PRIORITY: i32 = 100;

/// General extension adapter for full hook point access
#[derive(Debug)]
pub struct GeneralExtensionAdapter;

impl GeneralExtensionAdapter {
    /// Create a new general extension adapter
    pub fn new() -> Self {
        Self
    }

    /// Parse hook declarations from manifest
    fn parse_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookDeclaration> {
        // Try to get hooks from manifest metadata
        if let Some(hooks_value) = manifest.get("hooks") {
            if let Some(hooks_array) = hooks_value.as_array() {
                return hooks_array
                    .iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect();
            }
        }

        // Try parsing from raw config
        if let Ok(config) = serde_json::from_value::<GeneralExtensionConfig>(
            serde_json::json!(manifest.metadata.clone()),
        ) {
            return config.hooks;
        }

        Vec::new()
    }

    /// Parse a single hook declaration into a HookPoint
    fn parse_hook_point(&self, decl: &HookDeclaration) -> Option<HookPoint> {
        match decl.point.as_str() {
            // Prompt lifecycle
            "prompt.system_section" => {
                let section = decl
                    .params
                    .get("section")?
                    .as_str()?
                    .to_string();
                let priority = decl
                    .params
                    .get("priority")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(GENERAL_HOOK_PRIORITY as i64) as i32;
                Some(HookPoint::PromptSystemSection { section, priority })
            }
            "prompt.pre_process" => Some(HookPoint::PromptPreProcess),
            "prompt.post_process" => Some(HookPoint::PromptPostProcess),

            // Tool lifecycle
            "tool.register" => Some(HookPoint::ToolRegister),
            "tool.execute" => {
                let tool_name = decl
                    .params
                    .get("tool_name")?
                    .as_str()?
                    .to_string();
                Some(HookPoint::ToolExecute { tool_name })
            }
            "tool.execute_async" => {
                let tool_name = decl
                    .params
                    .get("tool_name")?
                    .as_str()?
                    .to_string();
                Some(HookPoint::ToolExecuteAsync { tool_name })
            }
            "tool.check_status" => {
                let tool_name = decl
                    .params
                    .get("tool_name")?
                    .as_str()?
                    .to_string();
                Some(HookPoint::ToolCheckStatus { tool_name })
            }
            "tool.cancel" => {
                let tool_name = decl
                    .params
                    .get("tool_name")?
                    .as_str()?
                    .to_string();
                Some(HookPoint::ToolCancel { tool_name })
            }
            "tool.result_transform" => Some(HookPoint::ToolResultTransform),

            // Session lifecycle
            "session.state_change" => Some(HookPoint::SessionStateChange),
            "session.compaction" => Some(HookPoint::SessionCompaction),
            "session.context_build" => Some(HookPoint::SessionContextBuild),

            // I/O lifecycle
            "io.channel_input" => Some(HookPoint::ChannelInput),
            "io.channel_output" => Some(HookPoint::ChannelOutput),
            "io.message_pre_send" => Some(HookPoint::MessagePreSend),
            "io.message_post_receive" => Some(HookPoint::MessagePostReceive),

            // Event lifecycle
            "event.subscribe" => {
                let topic_pattern = decl
                    .params
                    .get("topic_pattern")?
                    .as_str()?
                    .to_string();
                Some(HookPoint::EventSubscribe { topic_pattern })
            }
            "event.emit" => Some(HookPoint::EventEmit),

            // Agent lifecycle
            "agent.init" => Some(HookPoint::AgentInit),
            "agent.shutdown" => Some(HookPoint::AgentShutdown),
            "agent.iteration" => {
                let iteration = decl
                    .params
                    .get("iteration")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                Some(HookPoint::AgentIteration { iteration })
            }

            // Unknown hook point
            _ => {
                warn!("Unknown hook point: {}", decl.point);
                None
            }
        }
    }
}

impl Default for GeneralExtensionAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for GeneralExtensionAdapter {
    fn extension_type(&self) -> &'static str {
        GENERAL_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> super::ManifestFormat {
        super::ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["id", "name", "hooks"],
            file_name: "manifest.yaml",
        }
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        let declarations = self.parse_hooks(manifest);
        let mut bindings = Vec::new();

        for decl in declarations {
            if let Some(hook_point) = self.parse_hook_point(&decl) {
                bindings.push(HookBinding::new(
                    hook_point,
                    Box::new(GeneralHandlerFactory {
                        handler_name: decl.handler.clone(),
                        hook_type: decl.point.clone(),
                        manifest: manifest.clone(),
                    }),
                ));
            } else {
                warn!(
                    "Failed to parse hook declaration: {} for handler {}",
                    decl.point, decl.handler
                );
            }
        }

        bindings
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        info!("Initializing general extension: {}", manifest.id);
        debug!("Extension name: {}", manifest.name);

        let hooks = self.parse_hooks(manifest);
        if hooks.is_empty() {
            warn!("General extension {} has no hook declarations", manifest.id);
        } else {
            info!(
                "General extension {} registered {} hooks",
                manifest.id,
                hooks.len()
            );
        }

        Ok(ExtensionState::Unit)
    }

    async fn shutdown(&self, _state: ExtensionState) -> Result<()> {
        Ok(())
    }
}

/// Configuration for a general extension
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralExtensionConfig {
    /// Hook declarations
    #[serde(default)]
    pub hooks: Vec<HookDeclaration>,
}

/// Single hook declaration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDeclaration {
    /// Hook point name (e.g., "tool.execute", "event.subscribe")
    pub point: String,

    /// Handler identifier (extension-specific)
    pub handler: String,

    /// Hook-specific parameters (tool_name, section, priority, etc.)
    #[serde(flatten, default)]
    pub params: HashMap<String, serde_json::Value>,
}

/// Factory for creating general extension handlers
#[derive(Clone)]
struct GeneralHandlerFactory {
    handler_name: String,
    hook_type: String,
    manifest: ExtensionManifest,
}

impl std::fmt::Debug for GeneralHandlerFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneralHandlerFactory")
            .field("handler_name", &self.handler_name)
            .field("hook_type", &self.hook_type)
            .field("extension", &self.manifest.id)
            .finish()
    }
}

#[async_trait]
impl HookHandlerFactory for GeneralHandlerFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(GeneralHandler {
            handler_name: self.handler_name.clone(),
            hook_type: self.hook_type.clone(),
            extension_id: crate::extensions::types::ExtensionId::new(&self.manifest.id.0),
        })
    }
}

/// Handler for general extension hooks
#[derive(Clone)]
struct GeneralHandler {
    handler_name: String,
    hook_type: String,
    extension_id: crate::extensions::types::ExtensionId,
}

impl std::fmt::Debug for GeneralHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneralHandler")
            .field("handler_name", &self.handler_name)
            .field("hook_type", &self.hook_type)
            .field("extension_id", &self.extension_id.0)
            .finish()
    }
}

#[async_trait]
impl HookHandler for GeneralHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        debug!(
            "General handler '{}' invoked for extension '{}'",
            self.handler_name, self.extension_id
        );

        // By default, pass through - extensions should implement custom logic
        // by providing their own handler implementations
        match ctx.input {
            HookInput::Message(envelope) => HookResult::Continue(HookOutput::Message(envelope.content)),
            HookInput::Json(json) => HookResult::Continue(HookOutput::Json(json)),
            HookInput::ToolRegistry(access) => {
                HookResult::Continue(HookOutput::Json(serde_json::json!({
                    "tools": access.tools
                })))
            }
            HookInput::SystemEvent(event) => HookResult::Continue(HookOutput::Event(event)),
            HookInput::PromptBuild(_) => HookResult::Continue(HookOutput::Text(
                format!("General extension {} handler {}", self.extension_id.0, self.handler_name)
            )),
            _ => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        // This is determined by the factory at binding time
        // The handler itself is generic
        HookPoint::AgentInit
    }

    fn priority(&self) -> i32 {
        GENERAL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("{}:{}", self.extension_id.0, self.handler_name)
    }
}

/// Discover general extensions in a directory
pub async fn discover_general_extensions(dir: &Path) -> Result<Vec<DiscoveredGeneralExtension>> {
    let mut discovered = Vec::new();

    if !dir.exists() {
        debug!("General extensions directory does not exist: {}", dir.display());
        return Ok(discovered);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .context("Failed to read general extensions directory")?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Check for manifest.yaml
        let manifest_path = path.join("manifest.yaml");
        if manifest_path.exists() {
            match parsing::read_yaml_frontmatter_file(&manifest_path).await {
                Ok((yaml, _)) => {
                    match extract_hooks_from_yaml(&yaml) {
                        Ok(hooks) => {
                            match parsing::build_manifest_from_yaml(&yaml, GENERAL_EXTENSION_TYPE, &path) {
                                Ok(mut manifest) => {
                                    // Store hooks in manifest metadata
                                    if let Ok(hooks_json) = serde_json::to_value(&hooks) {
                                        manifest.set("hooks", hooks_json);
                                    }
                                    discovered.push(DiscoveredGeneralExtension { manifest, hooks });
                                    continue;
                                }
                                Err(e) => warn!("Failed to build manifest: {}", e),
                            }
                        }
                        Err(e) => warn!("Failed to extract hooks: {}", e),
                    }
                }
                Err(e) => warn!("Failed to read manifest at {}: {}", manifest_path.display(), e),
            }
        }

        // Check for manifest.json
        let manifest_path = path.join("manifest.json");
        if manifest_path.exists() {
            match parsing::parse_json_file::<serde_json::Value>(&manifest_path).await {
                Ok(json) => {
                    match extract_hooks_from_json(&json) {
                        Ok(hooks) => {
                            match build_manifest_from_json(&json, &path) {
                                Ok(manifest) => {
                                    discovered.push(DiscoveredGeneralExtension { manifest, hooks });
                                }
                                Err(e) => warn!("Failed to build manifest: {}", e),
                            }
                        }
                        Err(e) => warn!("Failed to extract hooks: {}", e),
                    }
                }
                Err(e) => warn!("Failed to read manifest at {}: {}", manifest_path.display(), e),
            }
        }
    }

    info!("Discovered {} general extensions", discovered.len());
    Ok(discovered)
}

/// Extract hook declarations from YAML
fn extract_hooks_from_yaml(yaml: &serde_yaml::Value) -> Result<Vec<HookDeclaration>> {
    yaml.get("hooks")
        .and_then(|h| serde_yaml::from_value(h.clone()).ok())
        .map(Ok)
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// Extract hook declarations from JSON
fn extract_hooks_from_json(json: &serde_json::Value) -> Result<Vec<HookDeclaration>> {
    json.get("hooks")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
        .map(Ok)
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// Build manifest from JSON (general extension specific)
fn build_manifest_from_json(json: &serde_json::Value, path: &Path) -> Result<ExtensionManifest> {
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .with_context(|| "Missing required field: id")?;
    let name = json
        .get("name")
        .and_then(|v| v.as_str())
        .with_context(|| "Missing required field: name")?;
    let version = json.get("version").and_then(|v| v.as_str()).unwrap_or("1.0.0");
    let description = json.get("description").and_then(|v| v.as_str()).unwrap_or("");

    let mut manifest =
        ExtensionManifest::new(id, GENERAL_EXTENSION_TYPE, name, description, version, path.to_path_buf());

    // Store hooks in manifest
    if let Some(hooks) = json.get("hooks") {
        manifest.set("hooks", hooks.clone());
    }

    Ok(manifest)
}

/// A discovered general extension before registration
#[derive(Debug, Clone)]
pub struct DiscoveredGeneralExtension {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Hook declarations
    pub hooks: Vec<HookDeclaration>,
}

/// Register general extensions with an ExtensionCore
pub async fn register_general_extensions_with_core(
    core: &crate::extensions::ExtensionCore,
    extensions: Vec<DiscoveredGeneralExtension>,
) -> Result<usize> {
    let adapter = GeneralExtensionAdapter::new();
    let mut registered = 0;

    for ext in extensions {
        let extension_id = ext.manifest.id.clone();

        // Resolve and register hooks
        let bindings = adapter.resolve_hooks(&ext.manifest);
        let hook_count = bindings.len();

        for binding in bindings {
            let handler = binding.handler_factory.create(ext.manifest.clone());
            let handler_arc: Arc<dyn crate::extensions::core::HookHandler> = Arc::from(handler);

            if let Err(e) = core.register_hook(binding.point, handler_arc, &extension_id).await {
                warn!("Failed to register hook for extension {}: {}", extension_id, e);
            }
        }

        registered += 1;
        info!("Registered general extension '{}' with {} hooks", extension_id, hook_count);
    }

    Ok(registered)
}

/// Convenience function to discover and register general extensions
pub async fn load_and_register_general_extensions(
    core: &crate::extensions::ExtensionCore,
    extensions_dir: &Path,
) -> Result<usize> {
    let extensions = discover_general_extensions(extensions_dir).await?;
    register_general_extensions_with_core(core, extensions).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_general_extension_adapter_manifest_format() {
        let adapter = GeneralExtensionAdapter::new();
        let format = adapter.manifest_format();

        assert!(matches!(format, super::super::ManifestFormat::YamlFrontmatterMarkdown { .. }));
    }

    #[test]
    fn test_parse_hook_declaration_prompt_section() {
        let adapter = GeneralExtensionAdapter::new();
        let decl = HookDeclaration {
            point: "prompt.system_section".to_string(),
            handler: "test_handler".to_string(),
            params: {
                let mut map = HashMap::new();
                map.insert("section".to_string(), serde_json::json!("test"));
                map.insert("priority".to_string(), serde_json::json!(50));
                map
            },
        };

        let hook_point = adapter.parse_hook_point(&decl);
        assert!(hook_point.is_some());
        assert!(matches!(hook_point.unwrap(), HookPoint::PromptSystemSection { section, priority } if section == "test" && priority == 50));
    }

    #[test]
    fn test_parse_hook_declaration_tool_execute() {
        let adapter = GeneralExtensionAdapter::new();
        let decl = HookDeclaration {
            point: "tool.execute".to_string(),
            handler: "test_handler".to_string(),
            params: {
                let mut map = HashMap::new();
                map.insert("tool_name".to_string(), serde_json::json!("my_tool"));
                map
            },
        };

        let hook_point = adapter.parse_hook_point(&decl);
        assert!(hook_point.is_some());
        assert!(matches!(hook_point.unwrap(), HookPoint::ToolExecute { tool_name } if tool_name == "my_tool"));
    }

    #[test]
    fn test_parse_hook_declaration_event_subscribe() {
        let adapter = GeneralExtensionAdapter::new();
        let decl = HookDeclaration {
            point: "event.subscribe".to_string(),
            handler: "test_handler".to_string(),
            params: {
                let mut map = HashMap::new();
                map.insert("topic_pattern".to_string(), serde_json::json!("instance.*"));
                map
            },
        };

        let hook_point = adapter.parse_hook_point(&decl);
        assert!(hook_point.is_some());
        assert!(matches!(hook_point.unwrap(), HookPoint::EventSubscribe { topic_pattern } if topic_pattern == "instance.*"));
    }

    #[test]
    fn test_parse_hook_declaration_unknown() {
        let adapter = GeneralExtensionAdapter::new();
        let decl = HookDeclaration {
            point: "unknown.hook".to_string(),
            handler: "test_handler".to_string(),
            params: HashMap::new(),
        };

        let hook_point = adapter.parse_hook_point(&decl);
        assert!(hook_point.is_none());
    }

    #[tokio::test]
    async fn test_discover_general_extensions() {
        let temp = TempDir::new().unwrap();

        // Create a test extension
        let ext_dir = temp.path().join("test-ext");
        tokio::fs::create_dir(&ext_dir).await.unwrap();

        let manifest_content = r#"---
id: test-ext
name: Test Extension
version: "1.0.0"
description: A test extension
hooks:
  - point: agent.init
    handler: init_handler
  - point: tool.execute
    tool_name: "*"
    handler: tool_handler
---
# Extension content
"#;

        tokio::fs::write(ext_dir.join("manifest.yaml"), manifest_content)
            .await
            .unwrap();

        let extensions = discover_general_extensions(temp.path()).await.unwrap();
        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].hooks.len(), 2);
    }

    #[tokio::test]
    async fn test_register_general_extensions() {
        let temp = TempDir::new().unwrap();

        // Create a test extension
        let ext_dir = temp.path().join("test-ext");
        tokio::fs::create_dir(&ext_dir).await.unwrap();

        let manifest_content = r#"---
id: test-ext
name: Test Extension
version: "1.0.0"
description: A test extension
hooks:
  - point: agent.init
    handler: init_handler
---
"#;

        tokio::fs::write(ext_dir.join("manifest.yaml"), manifest_content)
            .await
            .unwrap();

        let core = crate::extensions::ExtensionCore::new();
        let count = load_and_register_general_extensions(&core, temp.path()).await.unwrap();

        assert_eq!(count, 1);
        assert_eq!(core.hook_count().await, 1);
    }
}
