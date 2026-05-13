//! Extension Type Adapter Framework
//!
//! This module contains the generic adapter framework that all extension type
//! implementations must use. Extension-specific adapters live in
//! `crate::extensions::<type>::adapter`, not here.


/// Re-export from core for convenience
pub use crate::extension::core::HookBinding;

/// Adapter trait definition
///
/// This trait defines the interface that all extension type adapters must implement.
#[async_trait::async_trait]
pub trait ExtensionTypeAdapter: Send + Sync + std::fmt::Debug {
    /// Get the extension type identifier
    fn extension_type(&self) -> &'static str;

    /// Get the manifest format for this extension type
    fn manifest_format(&self) -> ManifestFormat;

    /// Resolve hook bindings for a manifest
    fn resolve_hooks(
        &self,
        manifest: &crate::extension::types::ExtensionManifest,
    ) -> Vec<HookBinding>;

    /// Initialize the extension
    async fn initialize(
        &self,
        _manifest: &crate::extension::types::ExtensionManifest,
    ) -> anyhow::Result<ExtensionState> {
        Ok(ExtensionState::Unit)
    }

    /// Shutdown the extension
    async fn shutdown(&self, _state: ExtensionState) -> anyhow::Result<()> {
        Ok(())
    }

    /// Check if an extension is healthy
    async fn is_healthy(&self, _state: &ExtensionState) -> bool {
        true
    }

    /// Register tools provided by this extension with the unified registry.
    async fn register_tools(
        &self,
        _core: &crate::extension::core::ExtensionCore,
        _manifest: &crate::extension::types::ExtensionManifest,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }

    /// Parse a manifest file for this extension type
    fn parse_manifest(
        &self,
        path: &std::path::Path,
        content: &str,
    ) -> anyhow::Result<crate::extension::types::ExtensionManifest> {
        use anyhow::Context;

        match self.manifest_format() {
            ManifestFormat::YamlFrontmatterMarkdown { .. } => {
                parse_yaml_frontmatter_markdown(path, content)
            }
            ManifestFormat::Yaml { .. } => parse_pure_yaml_manifest(path, content),
            ManifestFormat::Json { .. } => serde_json::from_str(content)
                .with_context(|| format!("Failed to parse JSON manifest at {path:?}")),
            ManifestFormat::Toml { .. } => toml::from_str(content)
                .with_context(|| format!("Failed to parse TOML manifest at {path:?}")),
            ManifestFormat::Custom { .. } => {
                anyhow::bail!("Custom manifest formats must implement parse_manifest")
            }
        }
    }
}

/// Parse YAML frontmatter from a markdown file
fn parse_yaml_frontmatter_markdown(
    path: &std::path::Path,
    content: &str,
) -> anyhow::Result<crate::extension::types::ExtensionManifest> {
    use anyhow::Context;

    let mut lines = content.lines().peekable();

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

    let mut manifest: crate::extension::types::ExtensionManifest =
        serde_yaml::from_str(&frontmatter)
            .with_context(|| format!("Failed to parse YAML frontmatter in {path:?}"))?;

    manifest.path = path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    Ok(manifest)
}

/// Parse a pure YAML manifest file
fn parse_pure_yaml_manifest(
    path: &std::path::Path,
    content: &str,
) -> anyhow::Result<crate::extension::types::ExtensionManifest> {
    use anyhow::Context;

    let yaml: serde_yaml::Value = serde_yaml::from_str(content)
        .with_context(|| format!("Failed to parse YAML manifest at {path:?}"))?;

    let mut manifest = parsing::build_manifest_from_yaml(&yaml, "", path)
        .with_context(|| format!("Failed to build manifest from YAML at {path:?}"))?;

    if let Some(ext_type) = yaml.get("extension_type").and_then(|v| v.as_str()) {
        manifest.extension_type = ext_type.to_string();
    }

    Ok(manifest)
}

/// Shared manifest parsing utilities
pub mod parsing {
    use anyhow::{Context, Result};
    use serde::de::DeserializeOwned;
    use std::path::{Path, PathBuf};

    pub fn parse_yaml_frontmatter(content: &str) -> Result<(String, String)> {
        let mut lines = content.lines().peekable();
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

    pub fn parse_yaml_frontmatter_typed<T: DeserializeOwned>(content: &str) -> Result<(T, String)> {
        let (frontmatter, body) = parse_yaml_frontmatter(content)?;
        let metadata: T =
            serde_yaml::from_str(&frontmatter).context("Failed to parse YAML frontmatter")?;
        Ok((metadata, body))
    }

    pub async fn parse_yaml_frontmatter_file<T: DeserializeOwned>(
        path: &Path,
    ) -> Result<(T, String)> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {path:?}"))?;
        parse_yaml_frontmatter_typed(&content)
            .with_context(|| format!("Failed to parse frontmatter in: {path:?}"))
    }

    pub async fn parse_toml_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read TOML file: {path:?}"))?;
        toml::from_str(&content).with_context(|| format!("Failed to parse TOML file: {path:?}"))
    }

    pub async fn parse_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read JSON file: {path:?}"))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON file: {path:?}"))
    }

    pub fn require_string_field(yaml: &serde_yaml::Value, field: &str) -> Result<String> {
        yaml.get(field)
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
            .with_context(|| format!("Missing or invalid required field: {field}"))
    }

    pub fn optional_string_field(yaml: &serde_yaml::Value, field: &str, default: &str) -> String {
        yaml.get(field)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    pub fn extract_extension_fields(
        yaml: &serde_yaml::Value,
    ) -> Result<(String, String, String, String)> {
        let id =
            require_string_field(yaml, "id").or_else(|_| require_string_field(yaml, "name"))?;
        let name = require_string_field(yaml, "name")?;
        let version = optional_string_field(yaml, "version", "1.0.0");
        let description = optional_string_field(yaml, "description", "");
        Ok((id, name, version, description))
    }

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
        Ok((
            id.to_string(),
            name.to_string(),
            version.to_string(),
            description.to_string(),
        ))
    }

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
                    .filter_map(|(k, v)| k.as_str().map(|key| (key.to_string(), yaml_to_json(v))))
                    .collect();
                serde_json::Value::Object(json_map)
            }
            serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value),
        }
    }

    pub fn build_manifest_from_yaml(
        yaml: &serde_yaml::Value,
        extension_type: &str,
        path: &Path,
    ) -> Result<crate::extension::types::ExtensionManifest> {
        let (id, name, version, description) = extract_extension_fields(yaml)?;
        let mut manifest = crate::extension::types::ExtensionManifest::new(
            &id,
            extension_type,
            &name,
            &description,
            &version,
            path.to_path_buf(),
        );
        if let serde_yaml::Value::Mapping(map) = yaml {
            for (k, v) in map {
                if let Some(key) = k.as_str() {
                    if !["id", "name", "version", "description"].contains(&key) {
                        manifest.set(key, yaml_to_json(v.clone()));
                    }
                }
            }
        }
        Ok(manifest)
    }

    pub fn build_manifest_from_toml(
        toml: &toml::Value,
        extension_type: &str,
        path: &Path,
    ) -> Result<crate::extension::types::ExtensionManifest> {
        let (id, name, version, description) = extract_extension_fields_toml(toml)?;
        let mut manifest = crate::extension::types::ExtensionManifest::new(
            &id,
            extension_type,
            &name,
            &description,
            &version,
            path.to_path_buf(),
        );
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
            .with_context(|| format!("Failed to read directory: {dir:?}"))?;
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

    pub fn has_file(dir: &Path, filename: &str) -> bool {
        dir.join(filename).exists()
    }

    pub async fn read_yaml_frontmatter_file(path: &Path) -> Result<(serde_yaml::Value, String)> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {path:?}"))?;
        let (frontmatter, body) = parse_yaml_frontmatter(&content)?;
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&frontmatter).context("Failed to parse YAML frontmatter")?;
        Ok((yaml, body))
    }

    pub async fn read_toml_file(path: &Path) -> Result<toml::Value> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {path:?}"))?;
        toml::from_str(&content).context("Failed to parse TOML")
    }

    pub async fn find_executable(tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        let candidates = [
            tool_path.join(format!("{tool_name}.py")),
            tool_path.join(format!("{tool_name}.js")),
            tool_path.join(format!("{tool_name}.sh")),
            tool_path.join(tool_name),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.clone());
            }
        }
        let mut entries = tokio::fs::read_dir(tool_path).await.ok()?;
        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    if name != "manifest.yaml" {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    pub fn find_executable_sync(tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        let candidates = [
            tool_path.join(format!("{tool_name}.py")),
            tool_path.join(format!("{tool_name}.js")),
            tool_path.join(format!("{tool_name}.sh")),
            tool_path.join(tool_name),
        ];
        for candidate in &candidates {
            if candidate.exists() {
                return Some(candidate.clone());
            }
        }
        let entries = std::fs::read_dir(tool_path).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name() {
                    if name != "manifest.yaml" {
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
    YamlFrontmatterMarkdown {
        required_fields: Vec<&'static str>,
        file_name: &'static str,
    },
    Yaml {
        schema: String,
        file_name: &'static str,
    },
    Json {
        schema: String,
        file_name: &'static str,
    },
    Toml {
        schema: String,
        file_name: &'static str,
    },
    Custom {
        detector: fn(&std::path::Path) -> bool,
    },
}

impl ManifestFormat {
    pub fn detect(&self, path: &std::path::Path) -> bool {
        match self {
            Self::YamlFrontmatterMarkdown { file_name, .. } => path.join(file_name).exists(),
            Self::Yaml { file_name, .. } => path.join(file_name).exists(),
            Self::Json { file_name, .. } => path.join(file_name).exists(),
            Self::Toml { file_name, .. } => path.join(file_name).exists(),
            Self::Custom { detector } => detector(path),
        }
    }

    pub fn manifest_path(&self, base_path: &std::path::Path) -> Option<std::path::PathBuf> {
        match self {
            Self::YamlFrontmatterMarkdown { file_name, .. }
            | Self::Yaml { file_name, .. }
            | Self::Json { file_name, .. }
            | Self::Toml { file_name, .. } => Some(base_path.join(file_name)),
            Self::Custom { .. } => None,
        }
    }
}

/// Extract extension_type from a pure YAML manifest file.
pub fn extract_extension_type_from_yaml(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read manifest at {path:?}"))?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse YAML manifest at {path:?}"))?;
    Ok(yaml
        .get("extension_type")
        .and_then(serde_yaml::Value::as_str)
        .map(std::string::ToString::to_string))
}

/// Extension state for stateful extensions
#[derive(Debug)]
pub enum ExtensionState {
    Unit,
    Boxed(Box<dyn std::any::Any + Send + Sync>),
}

impl ExtensionState {
    pub fn is_unit(&self) -> bool {
        matches!(self, Self::Unit)
    }
}

pub mod validation;

pub use validation::{ExtensionValidationService, ValidationReport};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[test]
    fn test_manifest_format_yaml_detection() {
        let format = ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "SKILL.md",
        };
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

    // Note: BuiltInAdapters test moved to src/extensions/mod.rs tests
    // because BuiltInAdapters depends on extension type implementations

    #[test]
    fn test_extract_extension_type_from_yaml_with_type() {
        let temp = TempDir::new().unwrap();
        let manifest = temp.path().join("manifest.yaml");
        std::fs::write(
            &manifest,
            "id: test\nname: Test\nextension_type: universal-tool\n",
        )
        .unwrap();
        let result = extract_extension_type_from_yaml(&manifest).unwrap();
        assert_eq!(result, Some("universal-tool".to_string()));
    }

    #[test]
    fn test_extract_extension_type_from_yaml_without_type() {
        let temp = TempDir::new().unwrap();
        let manifest = temp.path().join("manifest.yaml");
        std::fs::write(&manifest, "id: test\nname: Test\n").unwrap();
        let result = extract_extension_type_from_yaml(&manifest).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_extension_type_from_yaml_custom_prefix() {
        let temp = TempDir::new().unwrap();
        let manifest = temp.path().join("manifest.yaml");
        std::fs::write(
            &manifest,
            "id: test\nname: Test\nextension_type: custom:my-org/type\n",
        )
        .unwrap();
        let result = extract_extension_type_from_yaml(&manifest).unwrap();
        assert_eq!(result, Some("custom:my-org/type".to_string()));
    }

    #[test]
    fn test_extract_extension_type_from_yaml_invalid_yaml() {
        let temp = TempDir::new().unwrap();
        let manifest = temp.path().join("manifest.yaml");
        std::fs::write(&manifest, "not: valid: yaml: : :").unwrap();
        assert!(extract_extension_type_from_yaml(&manifest).is_err());
    }

    #[test]
    fn test_extract_extension_type_from_yaml_missing_file() {
        let temp = TempDir::new().unwrap();
        let manifest = temp.path().join("manifest.yaml");
        assert!(extract_extension_type_from_yaml(&manifest).is_err());
    }
}
