//! Extension Type Adapters
//!
//! This module contains adapters that map specific extension formats to the
//! Extension Core's hook points. Each adapter implements the `ExtensionTypeAdapter`
//! trait.
//!
//! # Available Adapters
//!
//! | Adapter | Extension Type | Status |
//! |---------|---------------|--------|
//! | `SkillAdapter` | SKILL.md files | Phase 2 |
//! | `McpAdapter` | MCP server configs | Phase 4 |
//! | `UniversalToolAdapter` | Universal tool manifests | Phase 3 |
//! | `ChannelAdapter` | I/O channels | Phase 5 |
//! | `HookAdapter` | Event/webhook handlers | Phase 6 |
//! | `GatewayAdapter` | Gateway plugins | Phase 6 |
//!
//! # Creating Custom Adapters
//!
//! ```rust,ignore
//! use pekobot::extensions::adapters::ExtensionTypeAdapter;
//!
//! pub struct MyCustomAdapter;
//!
//! impl ExtensionTypeAdapter for MyCustomAdapter {
//!     fn extension_type(&self) -> &'static str {
//!         "custom:my-type"
//!     }
//!
//!     fn manifest_format(&self) -> ManifestFormat {
//!         ManifestFormat::Json {
//!             schema: "my-schema".to_string(),
//!             file_name: "manifest.json",
//!         }
//!     }
//!
//!     fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
//!         vec![HookBinding::prompt_section("custom", MyHandlerFactory)]
//!     }
//! }
//! ```

// Extension type adapters
pub mod skill_adapter;
pub mod universal_tool_adapter;
pub mod mcp_adapter;
pub mod channel_adapter;
pub mod hook_adapter;
pub mod gateway_adapter;
pub mod general_adapter;

// Re-export skill adapter types
pub use skill_adapter::{DiscoveredSkill, SkillAdapter, load_skills_from_directory, register_skills_with_core};

// Re-export universal tool adapter types
pub use universal_tool_adapter::{DiscoveredUniversalTool, UniversalToolAdapter, load_tools_from_directory, register_tools_with_core};

// Re-export MCP adapter types
pub use mcp_adapter::{DiscoveredMcpServer, McpAdapter, load_servers_from_directory, register_servers_with_core};

// Re-export channel adapter types
pub use channel_adapter::{DiscoveredChannel, ChannelAdapter, ChannelExtensionConfig, MessageTransformerConfig, TransformType, discover_channel_extensions, load_and_register_channels, register_channels_with_core};

// Re-export hook adapter types
pub use hook_adapter::{DiscoveredHook, HookAdapter, HookExtensionConfig, EventSubscription, EventFilterConfig, WebhookConfig, CronConfig, discover_hook_extensions, load_and_register_hooks, register_hooks_with_core};

// Re-export gateway adapter types
pub use gateway_adapter::{DiscoveredGateway, GatewayAdapter, GatewayExtensionConfig, GatewayHookConfig, GatewayToolConfig, discover_gateway_extensions, load_and_register_gateways, register_gateways_with_core};

// Re-export general extension adapter types
pub use general_adapter::{DiscoveredGeneralExtension, GeneralExtensionAdapter, GeneralExtensionConfig, HookDeclaration, discover_general_extensions, load_and_register_general_extensions, register_general_extensions_with_core};

// Re-export the adapter trait when implemented
// pub use adapter_trait::ExtensionTypeAdapter;

/// Adapter trait definition (to be implemented)
///
/// This trait defines the interface that all extension type adapters must implement.
/// It provides the bridge between extension formats (SKILL.md, MCP config, etc.)
/// and the Extension Core's hook points.
#[async_trait::async_trait]
pub trait ExtensionTypeAdapter: Send + Sync + std::fmt::Debug {
    /// Get the extension type identifier
    ///
    /// This is a unique string that identifies the extension type.
    /// Standard types: "skill", "mcp", "universal-tool", "channel", "hook", "gateway"
    /// Custom types should use the "custom:" prefix.
    fn extension_type(&self) -> &'static str;

    /// Get the manifest format for this extension type
    ///
    /// Defines how extension manifests are detected and parsed.
    fn manifest_format(&self) -> ManifestFormat;

    /// Resolve hook bindings for a manifest
    ///
    /// Given an extension manifest, returns the list of hook bindings
    /// that connect the extension to hook points.
    fn resolve_hooks(&self, manifest: &crate::extensions::ExtensionManifest) -> Vec<HookBinding>;

    /// Initialize the extension
    ///
    /// Called when the extension is loaded. For stateful extensions
    /// (like MCP servers), this should establish connections.
    ///
    /// Default implementation returns empty state.
    async fn initialize(
        &self,
        _manifest: &crate::extensions::ExtensionManifest,
    ) -> anyhow::Result<ExtensionState> {
        Ok(ExtensionState::Unit)
    }

    /// Shutdown the extension
    ///
    /// Called when the extension is being unloaded. Should clean up
    /// any resources (connections, processes, etc.).
    ///
    /// Default implementation does nothing.
    async fn shutdown(&self, _state: ExtensionState) -> anyhow::Result<()> {
        Ok(())
    }

    /// Check if an extension is healthy
    ///
    /// For stateful extensions, this should verify the connection
    /// or process is still alive.
    ///
    /// Default implementation returns true.
    async fn is_healthy(&self, _state: &ExtensionState) -> bool {
        true
    }

    /// Parse a manifest file for this extension type
    ///
    /// This method allows adapters to customize manifest parsing.
    /// The default implementation handles standard formats (JSON, TOML, YAML frontmatter).
    ///
    /// For custom formats (like SKILL.md with its own frontmatter schema),
    /// adapters should override this method.
    ///
    /// # Arguments
    /// * `path` - Path to the manifest file
    /// * `content` - Content of the manifest file
    ///
    /// # Returns
    /// Parsed ExtensionManifest
    fn parse_manifest(
        &self,
        path: &std::path::Path,
        content: &str,
    ) -> anyhow::Result<crate::extensions::ExtensionManifest> {
        use anyhow::Context;

        match self.manifest_format() {
            ManifestFormat::YamlFrontmatterMarkdown { .. } => {
                parse_yaml_frontmatter_markdown(path, content)
            }
            ManifestFormat::Json { .. } => {
                serde_json::from_str(content)
                    .with_context(|| format!("Failed to parse JSON manifest at {:?}", path))
            }
            ManifestFormat::Toml { .. } => {
                toml::from_str(content)
                    .with_context(|| format!("Failed to parse TOML manifest at {:?}", path))
            }
            ManifestFormat::Custom { .. } => {
                anyhow::bail!("Custom manifest formats must implement parse_manifest")
            }
        }
    }
}

/// Parse YAML frontmatter from a markdown file
///
/// This is the default implementation used by the ExtensionTypeAdapter trait.
/// It expects the YAML to be directly deserializable into ExtensionManifest.
fn parse_yaml_frontmatter_markdown(
    path: &std::path::Path,
    content: &str,
) -> anyhow::Result<crate::extensions::ExtensionManifest> {
    use anyhow::Context;

    let mut lines = content.lines().peekable();

    // Must start with ---
    match lines.next() {
        Some("---") => {}
        _ => anyhow::bail!("YAML frontmatter must start with ---"),
    }

    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;

    for line in lines.by_ref() {
        if line == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    if !found_end {
        anyhow::bail!("YAML frontmatter must end with ---");
    }

    let frontmatter = frontmatter_lines.join("\n");

    let mut manifest: crate::extensions::ExtensionManifest = serde_yaml::from_str(&frontmatter)
        .with_context(|| format!("Failed to parse YAML frontmatter in {:?}", path))?;

    manifest.path = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();

    Ok(manifest)
}

/// Manifest format definitions
#[derive(Debug, Clone)]
pub enum ManifestFormat {
    /// YAML frontmatter in markdown file
    YamlFrontmatterMarkdown {
        /// Required frontmatter fields
        required_fields: Vec<&'static str>,
        /// File name to look for
        file_name: &'static str,
    },

    /// JSON file
    Json {
        /// Schema identifier
        schema: String,
        /// File name to look for
        file_name: &'static str,
    },

    /// TOML file
    Toml {
        /// Schema identifier
        schema: String,
        /// File name to look for
        file_name: &'static str,
    },

    /// Custom detection logic
    Custom {
        /// Function to detect if path contains this extension type
        detector: fn(&std::path::Path) -> bool,
    },
}

impl ManifestFormat {
    /// Detect if a path contains a manifest of this format
    pub fn detect(&self, path: &std::path::Path) -> bool {
        match self {
            Self::YamlFrontmatterMarkdown { file_name, .. } => path.join(file_name).exists(),
            Self::Json { file_name, .. } => path.join(file_name).exists(),
            Self::Toml { file_name, .. } => path.join(file_name).exists(),
            Self::Custom { detector } => detector(path),
        }
    }

    /// Get the manifest file path
    pub fn manifest_path(&self, base_path: &std::path::Path) -> Option<std::path::PathBuf> {
        match self {
            Self::YamlFrontmatterMarkdown { file_name, .. }
            | Self::Json { file_name, .. }
            | Self::Toml { file_name, .. } => Some(base_path.join(file_name)),
            Self::Custom { .. } => None,
        }
    }
}

/// Extension state for stateful extensions
#[derive(Debug)]
pub enum ExtensionState {
    /// No state
    Unit,

    /// MCP client connection
    #[cfg(feature = "mcp")]
    McpClient(crate::mcp::McpClient),

    /// Generic boxed state
    Boxed(Box<dyn std::any::Any + Send + Sync>),
}

impl ExtensionState {
    /// Check if state is empty
    pub fn is_unit(&self) -> bool {
        matches!(self, Self::Unit)
    }
}

/// Re-export from core for convenience
pub use crate::extensions::core::HookBinding;

/// Adapter registration trait
///
/// Implemented by types that can provide extension adapters.
pub trait AdapterProvider {
    /// Get all adapters provided by this type
    fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>>;
}

/// Built-in adapter provider
pub struct BuiltInAdapters;

impl BuiltInAdapters {
    /// Create a new built-in adapter provider
    pub fn new() -> Self {
        Self
    }

    /// Get all built-in adapters
    ///
    /// Returns all registered extension type adapters:
    /// - SkillAdapter: For SKILL.md based extensions
    /// - UniversalToolAdapter: For universal tool protocol extensions
    /// - McpAdapter: For MCP server extensions
    /// - ChannelAdapter: For I/O channel extensions
    /// - HookAdapter: For event/webhook extensions
    /// - GatewayAdapter: For gateway plugin extensions
    /// - GeneralExtensionAdapter: For extensions needing full hook point access
    pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>> {
        vec![
            Box::new(SkillAdapter::new()),
            Box::new(UniversalToolAdapter::new()),
            Box::new(McpAdapter::with_default_manager()),
            // Note: ChannelAdapter, HookAdapter, GatewayAdapter require ExtensionCore
            // and are registered by the ExtensionManager when needed
        ]
    }
}

impl Default for BuiltInAdapters {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_manifest_format_yaml_detection() {
        let format = ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "SKILL.md",
        };

        // Would need actual file system for full test
        assert!(!format.detect(Path::new("/nonexistent")));
        assert_eq!(
            format.manifest_path(Path::new("/tmp/skill")),
            Some(PathBuf::from("/tmp/skill/SKILL.md"))
        );
    }

    #[test]
    fn test_extension_state() {
        let state = ExtensionState::Unit;
        assert!(state.is_unit());
    }

    #[test]
    fn test_built_in_adapters() {
        let provider = BuiltInAdapters::new();
        let adapters = provider.adapters();
        assert!(!adapters.is_empty()); // Should have Skill, MCP, UniversalTool adapters
        assert_eq!(adapters.len(), 3); // Currently 3 adapters registered
    }
}
