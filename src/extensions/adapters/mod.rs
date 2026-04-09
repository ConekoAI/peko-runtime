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
    /// Note: This will return adapters as they are implemented in
    /// subsequent phases. Currently returns empty.
    pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>> {
        vec![
            // Phase 2: Box::new(SkillAdapter::new()),
            // Phase 3: Box::new(UniversalToolAdapter::new()),
            // Phase 4: Box::new(McpAdapter::new()),
            // Phase 5: Box::new(ChannelAdapter::new()),
            // Phase 6: Box::new(HookAdapter::new()),
            // Phase 6: Box::new(GatewayAdapter::new()),
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
        assert!(adapters.is_empty()); // Will be populated in later phases
    }
}
