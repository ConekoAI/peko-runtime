//! Extension Type Adapters
//!
//! This module contains adapters that map specific extension formats to the
//! Extension Core's hook points. Each adapter implements the `ExtensionTypeAdapter`
//! trait.
//!
//! # Shared Utilities
//!
//! The [`parsing`] module provides common manifest parsing functions to reduce
//! code duplication across adapters:
//!
//! - `parse_yaml_frontmatter()` - Extract YAML frontmatter from markdown
//! - `parse_yaml_frontmatter_typed<T>()` - Parse and deserialize frontmatter
//! - `extract_extension_fields()` - Get common id/name/version/description fields
//! - `build_manifest_from_yaml()` - Construct ExtensionManifest from YAML
//! - `discover_extensions()` - Generic extension discovery helper
//!
//! # Available Adapters
//!
//! | Adapter | Extension Type | Status |
//! |---------|---------------|--------|
//! | `SkillAdapter` | SKILL.md files | Phase 2 |
//! | `McpAdapter` | MCP server configs | Phase 4 |
//! | `UniversalToolAdapter` | Universal tool manifests | Phase 3 |
//! | `GatewayAdapter` | Gateway plugins (I/O channels) | Phase 6 |
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
pub mod builtin_tool_adapter;
pub mod skill_adapter;
pub mod universal_tool_adapter;
pub mod mcp_adapter;
pub mod gateway_adapter;
pub mod general_adapter;

// Re-export built-in tool adapter
pub use builtin_tool_adapter::BuiltinToolAdapter;

// Re-export skill adapter types
pub use skill_adapter::{DiscoveredSkill, SkillAdapter, load_skills_from_directory, register_skills_with_core};

// Re-export universal tool adapter types
pub use universal_tool_adapter::{DiscoveredUniversalTool, UniversalToolAdapter, load_tools_from_directory, register_tools_with_core};

// Re-export MCP adapter types
pub use mcp_adapter::{DiscoveredMcpServer, McpAdapter, load_servers_from_directory, register_servers_with_core};

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

/// Shared manifest parsing utilities
///
/// This module provides common parsing functions used across all adapters
/// to reduce code duplication when parsing YAML frontmatter, TOML, and JSON manifests.
pub mod parsing {
    use anyhow::{Context, Result};
    use serde::de::DeserializeOwned;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// Parse YAML frontmatter from markdown content
    ///
    /// Extracts the content between `---` delimiters at the start of the file.
    /// Returns the frontmatter as a string and the body content separately.
    ///
    /// # Arguments
    /// * `content` - The full markdown content with YAML frontmatter
    ///
    /// # Returns
    /// * `Ok((frontmatter, body))` - Tuple of frontmatter and body content
    /// * `Err` - If frontmatter delimiters are missing or malformed
    ///
    /// # Example
    /// ```
    /// let content = r#"---
    /// name: my-extension
    /// version: 1.0.0
    /// ---
    /// # Content
    /// Body here
    /// "#;
    /// let (frontmatter, body) = parse_yaml_frontmatter(content).unwrap();
    /// ```
    pub fn parse_yaml_frontmatter(content: &str) -> Result<(String, String)> {
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

        let body = lines.collect::<Vec<_>>().join("\n");
        Ok((frontmatter_lines.join("\n"), body))
    }

    /// Parse YAML frontmatter and deserialize into a type
    ///
    /// # Type Parameters
    /// * `T` - The type to deserialize into (must implement DeserializeOwned)
    ///
    /// # Arguments
    /// * `content` - The full markdown content with YAML frontmatter
    ///
    /// # Returns
    /// * `Ok((metadata, body))` - Deserialized metadata and body content
    pub fn parse_yaml_frontmatter_typed<T: DeserializeOwned>(
        content: &str,
    ) -> Result<(T, String)> {
        let (frontmatter, body) = parse_yaml_frontmatter(content)?;
        let metadata: T = serde_yaml::from_str(&frontmatter)
            .context("Failed to parse YAML frontmatter")?;
        Ok((metadata, body))
    }

    /// Parse a YAML frontmatter markdown file at the given path
    ///
    /// # Type Parameters
    /// * `T` - The type to deserialize frontmatter into
    ///
    /// # Arguments
    /// * `path` - Path to the markdown file
    ///
    /// # Returns
    /// * `Ok((metadata, body))` - Deserialized metadata and body content
    pub async fn parse_yaml_frontmatter_file<T: DeserializeOwned>(
        path: &Path,
    ) -> Result<(T, String)> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {:?}", path))?;
        parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in: {:?}", path))
    }

    /// Parse a TOML file into a type
    ///
    /// # Type Parameters
    /// * `T` - The type to deserialize into
    ///
    /// # Arguments
    /// * `path` - Path to the TOML file
    pub async fn parse_toml_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read TOML file: {:?}", path))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML file: {:?}", path))
    }

    /// Parse a JSON file into a type
    ///
    /// # Type Parameters
    /// * `T` - The type to deserialize into
    ///
    /// # Arguments
    /// * `path` - Path to the JSON file
    pub async fn parse_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read JSON file: {:?}", path))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON file: {:?}", path))
    }

    /// Extract a required string field from YAML value
    ///
    /// # Arguments
    /// * `yaml` - The YAML value object
    /// * `field` - Field name to extract
    ///
    /// # Returns
    /// * `Ok(String)` - The field value
    /// * `Err` - If field is missing or not a string
    pub fn require_string_field(yaml: &serde_yaml::Value, field: &str) -> Result<String> {
        yaml.get(field)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .with_context(|| format!("Missing or invalid required field: {}", field))
    }

    /// Extract an optional string field from YAML value
    ///
    /// # Arguments
    /// * `yaml` - The YAML value object
    /// * `field` - Field name to extract
    /// * `default` - Default value if field is missing
    pub fn optional_string_field(
        yaml: &serde_yaml::Value,
        field: &str,
        default: &str,
    ) -> String {
        yaml.get(field)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    /// Extract common extension fields from YAML frontmatter
    ///
    /// Returns the standard extension fields used across all extension types:
    /// - id
    /// - name
    /// - version (defaults to "1.0.0")
    /// - description (defaults to "")
    ///
    /// # Arguments
    /// * `yaml` - The parsed YAML value
    pub fn extract_extension_fields(
        yaml: &serde_yaml::Value,
    ) -> Result<(String, String, String, String)> {
        let id = require_string_field(yaml, "id")
            .or_else(|_| require_string_field(yaml, "name"))?; // Fallback to 'name' for skills
        let name = require_string_field(yaml, "name")?;
        let version = optional_string_field(yaml, "version", "1.0.0");
        let description = optional_string_field(yaml, "description", "");

        Ok((id, name, version, description))
    }

    /// Same as extract_extension_fields but for TOML
    pub fn extract_extension_fields_toml(
        toml: &toml::Value,
    ) -> Result<(String, String, String, String)> {
        let id = toml
            .get("id")
            .or_else(|| toml.get("name"))
            .and_then(|v| v.as_str())
            .with_context(|| "Missing required field: id or name")?;
        let name = toml
            .get("name")
            .and_then(|v| v.as_str())
            .with_context(|| "Missing required field: name")?;
        let version = toml
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("1.0.0");
        let description = toml
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok((id.to_string(), name.to_string(), version.to_string(), description.to_string()))
    }

    /// Convert a serde_yaml::Value to serde_json::Value
    ///
    /// Useful for storing YAML metadata in ExtensionManifest
    pub fn yaml_to_json(yaml: serde_yaml::Value) -> serde_json::Value {
        match yaml {
            serde_yaml::Value::Null => serde_json::Value::Null,
            serde_yaml::Value::Bool(b) => serde_json::Value::Bool(b),
            serde_yaml::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    serde_json::Value::Number(i.into())
                } else if let Some(f) = n.as_f64() {
                    serde_json::json!(f)
                } else {
                    serde_json::Value::Null
                }
            }
            serde_yaml::Value::String(s) => serde_json::Value::String(s),
            serde_yaml::Value::Sequence(seq) => {
                serde_json::Value::Array(seq.into_iter().map(yaml_to_json).collect())
            }
            serde_yaml::Value::Mapping(map) => {
                let json_map: serde_json::Map<String, serde_json::Value> = map
                    .into_iter()
                    .filter_map(|(k, v)| {
                        k.as_str()
                            .map(|key| (key.to_string(), yaml_to_json(v)))
                    })
                    .collect();
                serde_json::Value::Object(json_map)
            }
            serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value),
        }
    }

    /// Build an ExtensionManifest from parsed YAML fields
    ///
    /// # Arguments
    /// * `yaml` - The parsed YAML value containing extension metadata
    /// * `extension_type` - The extension type identifier (e.g., "skill", "channel")
    /// * `path` - The base path of the extension
    ///
    /// # Returns
    /// * `Ok(ExtensionManifest)` - The constructed manifest
    pub fn build_manifest_from_yaml(
        yaml: &serde_yaml::Value,
        extension_type: &str,
        path: &Path,
    ) -> Result<crate::extensions::ExtensionManifest> {
        let (id, name, version, description) = extract_extension_fields(yaml)?;

        let mut manifest = crate::extensions::ExtensionManifest::new(
            &id,
            extension_type,
            &name,
            &description,
            &version,
            path.to_path_buf(),
        );

        // Store all additional fields as metadata
        if let serde_yaml::Value::Mapping(map) = yaml {
            for (k, v) in map {
                if let Some(key) = k.as_str() {
                    // Skip already-set fields
                    if !["id", "name", "version", "description"].contains(&key) {
                        manifest.set(key, yaml_to_json(v.clone()));
                    }
                }
            }
        }

        Ok(manifest)
    }

    /// Build an ExtensionManifest from parsed TOML fields
    pub fn build_manifest_from_toml(
        toml: &toml::Value,
        extension_type: &str,
        path: &Path,
    ) -> Result<crate::extensions::ExtensionManifest> {
        let (id, name, version, description) = extract_extension_fields_toml(toml)?;

        let mut manifest = crate::extensions::ExtensionManifest::new(
            &id,
            extension_type,
            &name,
            &description,
            &version,
            path.to_path_buf(),
        );

        // Store all additional fields as metadata
        if let toml::Value::Table(table) = toml {
            for (key, value) in table {
                if !["id", "name", "version", "description"].contains(&key.as_str()) {
                    if let Ok(json_val) = serde_json::to_value(value) {
                        manifest.set(key, json_val);
                    }
                }
            }
        }

        Ok(manifest)
    }

    /// Discover extensions in a directory using a detector function
    ///
    /// # Arguments
    /// * `dir` - Directory to scan
    /// * `detector` - Function that checks if a path contains an extension
    /// * `parser` - Async function that parses the extension at the given path
    ///
    /// # Returns
    /// * `Ok(Vec<T>)` - List of discovered extensions
    pub async fn discover_extensions<T, D, P, Fut>(
        dir: &Path,
        detector: D,
        mut parser: P,
    ) -> Result<Vec<T>>
    where
        D: Fn(&Path) -> bool,
        P: FnMut(&Path) -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let mut discovered = Vec::new();

        if !dir.exists() {
            return Ok(discovered);
        }

        let mut entries = tokio::fs::read_dir(dir)
            .await
            .with_context(|| format!("Failed to read directory: {:?}", dir))?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            if detector(&path) {
                if let Some(item) = parser(&path).await? {
                    discovered.push(item);
                }
            }
        }

        Ok(discovered)
    }

    /// Check if a directory contains a file
    pub fn has_file(dir: &Path, filename: &str) -> bool {
        dir.join(filename).exists()
    }

    /// Read and parse a YAML frontmatter file asynchronously
    pub async fn read_yaml_frontmatter_file(path: &Path) -> Result<(serde_yaml::Value, String)> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {:?}", path))?;
        let (frontmatter, body) = parse_yaml_frontmatter(&content)?;
        let yaml: serde_yaml::Value = serde_yaml::from_str(&frontmatter)
            .context("Failed to parse YAML frontmatter")?;
        Ok((yaml, body))
    }

    /// Read and parse a TOML file asynchronously
    pub async fn read_toml_file(path: &Path) -> Result<toml::Value> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {:?}", path))?;
        toml::from_str(&content).context("Failed to parse TOML")
    }

    /// Find the executable file for a tool/capability in a directory
    ///
    /// This function looks for common executable patterns (.py, .js, .sh, or no extension)
    /// and falls back to finding any file that isn't manifest.json.
    ///
    /// # Arguments
    /// * `tool_path` - The directory to search
    /// * `tool_name` - The name of the tool (used for common patterns)
    ///
    /// # Returns
    /// * `Some(PathBuf)` - Path to the executable if found
    /// * `None` - If no executable was found
    pub async fn find_executable(tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        // Try common patterns first
        let candidates = [
            tool_path.join(format!("{}.py", tool_name)),
            tool_path.join(format!("{}.js", tool_name)),
            tool_path.join(format!("{}.sh", tool_name)),
            tool_path.join(tool_name),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.clone());
            }
        }

        // Fallback: find any file that's not manifest.json
        let mut entries = tokio::fs::read_dir(tool_path).await.ok()?;
        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    if name != "manifest.json" {
                        return Some(path);
                    }
                }
            }
        }

        None
    }

    /// Synchronous version of find_executable
    ///
    /// Used in contexts where async is not available (e.g., parse_manifest trait method).
    pub fn find_executable_sync(tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        // Try common patterns first
        let candidates = [
            tool_path.join(format!("{}.py", tool_name)),
            tool_path.join(format!("{}.js", tool_name)),
            tool_path.join(format!("{}.sh", tool_name)),
            tool_path.join(tool_name),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.clone());
            }
        }

        // Fallback: find any file that's not manifest.json
        let entries = std::fs::read_dir(tool_path).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    if name != "manifest.json" {
                        return Some(path);
                    }
                }
            }
        }

        None
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
    /// Returns all registered extension type adapters:
    /// - SkillAdapter: For SKILL.md based extensions
    /// - UniversalToolAdapter: For universal tool protocol extensions
    /// - McpAdapter: For MCP server extensions
    /// - GatewayAdapter: For gateway plugin extensions (includes I/O channels)
    pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>> {
        vec![
            Box::new(SkillAdapter::new()),
            Box::new(UniversalToolAdapter::new()),
            Box::new(McpAdapter::with_default_manager()),
            // Note: GatewayAdapter requires ExtensionCore
            // and is registered by the ExtensionManager when needed
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
