//! Extension Manager
//!
//! This module provides unified lifecycle management for extensions:
//! - Discovery from standard locations
//! - Installation and uninstallation
//! - Enable/disable control
//! - Bundling and packaging

use crate::extensions::adapters::{ExtensionTypeAdapter, ExtensionState};
use crate::extensions::core::ExtensionCore;
use crate::extensions::types::{ExtensionId, ExtensionManifest, HookId};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Extension Manager - Central management for all extensions
#[derive(Debug)]
pub struct ExtensionManager {
    adapters: HashMap<String, Box<dyn ExtensionTypeAdapter>>,
    extensions: HashMap<ExtensionId, LoadedExtension>,
    storage: ExtensionStorage,
    core: Arc<ExtensionCore>,
    extension_states: HashMap<ExtensionId, ExtensionState>,
}

/// An extension that has been loaded into the manager
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    pub extension_type: String,
    pub hook_ids: Vec<HookId>,
    pub enabled: bool,
    pub path: PathBuf,
}

/// Storage backend for extension metadata
#[derive(Debug)]
pub struct ExtensionStorage {
    storage_dir: Option<PathBuf>,
}

impl ExtensionStorage {
    pub fn new() -> Self {
        Self { storage_dir: None }
    }

    pub fn with_dir(storage_dir: PathBuf) -> Self {
        Self {
            storage_dir: Some(storage_dir),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.storage_dir.as_deref()
    }

    pub fn copy_to_storage(&self, source: &Path, extension_id: &ExtensionId) -> Result<PathBuf> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;

        let target_dir = storage_dir.join(&extension_id.0);

        if target_dir.exists() {
            std::fs::remove_dir_all(&target_dir)
                .with_context(|| format!("Failed to remove existing extension at {:?}", target_dir))?;
        }

        copy_dir_recursive(source, &target_dir)
            .with_context(|| format!("Failed to copy extension from {:?} to {:?}", source, target_dir))?;

        Ok(target_dir)
    }

    pub fn remove_from_storage(&self, extension_id: &ExtensionId) -> Result<()> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;

        let target_dir = storage_dir.join(&extension_id.0);

        if target_dir.exists() {
            std::fs::remove_dir_all(&target_dir)
                .with_context(|| format!("Failed to remove extension at {:?}", target_dir))?;
        }

        Ok(())
    }
}

impl Default for ExtensionStorage {
    fn default() -> Self {
        Self::new()
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Result of a load operation
#[derive(Debug, Default)]
pub struct LoadReport {
    pub loaded: Vec<ExtensionId>,
    pub failed: Vec<(PathBuf, anyhow::Error)>,
}

/// Bundle of multiple extensions
#[derive(Debug, Clone)]
pub struct ExtensionBundle {
    pub name: String,
    pub extensions: Vec<ExtensionManifest>,
    pub metadata: BundleMetadata,
}

/// Metadata for an extension bundle
#[derive(Debug, Default, Clone)]
pub struct BundleMetadata {
    pub version: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub conflicts: Vec<String>,
}

impl ExtensionManager {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            extensions: HashMap::new(),
            storage: ExtensionStorage::new(),
            core: Arc::new(ExtensionCore::new()),
            extension_states: HashMap::new(),
        }
    }

    pub fn with_core(core: Arc<ExtensionCore>) -> Self {
        Self {
            adapters: HashMap::new(),
            extensions: HashMap::new(),
            storage: ExtensionStorage::new(),
            core,
            extension_states: HashMap::new(),
        }
    }

    pub fn with_storage_dir(mut self, storage_dir: PathBuf) -> Self {
        self.storage = ExtensionStorage::with_dir(storage_dir);
        self
    }

    pub fn core(&self) -> &ExtensionCore {
        &self.core
    }

    pub fn core_arc(&self) -> Arc<ExtensionCore> {
        self.core.clone()
    }

    pub fn register_adapter(&mut self, adapter: Box<dyn ExtensionTypeAdapter>) {
        let ext_type = adapter.extension_type().to_string();
        debug!("Registering adapter for extension type: {}", ext_type);
        self.adapters.insert(ext_type, adapter);
    }

    fn detect_extension_type(&self, path: &Path) -> Option<&dyn ExtensionTypeAdapter> {
        for adapter in self.adapters.values() {
            let format = adapter.manifest_format();
            if format.detect(path) {
                debug!("Detected extension type '{}' at {:?}", adapter.extension_type(), path);
                return Some(adapter.as_ref());
            }
        }
        None
    }

    fn detect_extension_type_string(&self, path: &Path) -> Option<String> {
        for adapter in self.adapters.values() {
            let format = adapter.manifest_format();
            if format.detect(path) {
                return Some(adapter.extension_type().to_string());
            }
        }
        None
    }

    async fn load_extension_internal(
        &self,
        path: &Path,
        adapter: &dyn ExtensionTypeAdapter,
    ) -> Result<(ExtensionId, Vec<HookId>, ExtensionManifest)> {
        let format = adapter.manifest_format();
        let manifest_path = format
            .manifest_path(path)
            .context("Could not determine manifest path")?;

        let manifest = self.parse_manifest(&manifest_path, adapter).await?;
        let extension_id = manifest.id.clone();
        let ext_type = adapter.extension_type().to_string();

        // Initialize the extension if stateful
        let state = adapter.initialize(&manifest).await?;

        // Resolve and register hooks
        let bindings = adapter.resolve_hooks(&manifest);
        let mut hook_ids = Vec::new();

        for binding in bindings {
            let handler = binding.handler_factory.create(manifest.clone());
            let handler_arc: Arc<dyn crate::extensions::core::HookHandler> = handler.into();
            let registration = self
                .core
                .register_hook(binding.point, handler_arc, &extension_id)
                .await?;
            hook_ids.push(registration.id);
        }

        info!(
            "Loaded extension '{}' ({}) with {} hooks",
            extension_id,
            ext_type,
            hook_ids.len()
        );

        // Store state after hooks are registered (so self can be borrowed mutably after)
        if !state.is_unit() {
            // Note: We return the state to be stored by the caller
        }

        Ok((extension_id, hook_ids, manifest))
    }

    async fn parse_manifest(
        &self,
        path: &Path,
        adapter: &dyn ExtensionTypeAdapter,
    ) -> Result<ExtensionManifest> {
        let content = tokio::fs::read_to_string(path).await?;

        match adapter.manifest_format() {
            crate::extensions::adapters::ManifestFormat::YamlFrontmatterMarkdown { .. } => {
                self.parse_yaml_frontmatter(&content, path)
            }
            crate::extensions::adapters::ManifestFormat::Json { .. } => {
                self.parse_json_manifest(&content, path)
            }
            crate::extensions::adapters::ManifestFormat::Toml { .. } => {
                self.parse_toml_manifest(&content, path)
            }
            _ => anyhow::bail!("Custom manifest formats must be handled by the adapter"),
        }
    }

    fn parse_yaml_frontmatter(&self, content: &str, path: &Path) -> Result<ExtensionManifest> {
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
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

        let mut manifest: ExtensionManifest = serde_yaml::from_str(&frontmatter)
            .with_context(|| format!("Failed to parse YAML frontmatter in {:?}", path))?;

        manifest.path = base_dir.to_path_buf();

        Ok(manifest)
    }

    fn parse_json_manifest(&self, content: &str, path: &Path) -> Result<ExtensionManifest> {
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut manifest: ExtensionManifest = serde_json::from_str(content)
            .with_context(|| format!("Failed to parse JSON manifest in {:?}", path))?;
        manifest.path = base_dir.to_path_buf();
        Ok(manifest)
    }

    fn parse_toml_manifest(&self, content: &str, path: &Path) -> Result<ExtensionManifest> {
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut manifest: ExtensionManifest = toml::from_str(content)
            .with_context(|| format!("Failed to parse TOML manifest in {:?}", path))?;
        manifest.path = base_dir.to_path_buf();
        Ok(manifest)
    }

    pub async fn load_all(&mut self) -> Result<LoadReport> {
        let mut report = LoadReport::default();
        let discovery_paths = discovery_paths::all();

        for base_path in discovery_paths {
            if !base_path.exists() {
                continue;
            }

            debug!("Scanning for extensions in {:?}", base_path);

            let entries = match std::fs::read_dir(&base_path) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!("Failed to read directory {:?}: {}", base_path, e);
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                match self.try_load_extension(&path).await {
                    Ok(id) => {
                        report.loaded.push(id);
                    }
                    Err(e) => {
                        warn!("Failed to load extension from {:?}: {}", path, e);
                        report.failed.push((path, e));
                    }
                }
            }
        }

        info!(
            "Load complete: {} loaded, {} failed",
            report.loaded.len(),
            report.failed.len()
        );

        Ok(report)
    }

    async fn try_load_extension(&mut self, path: &Path) -> Result<ExtensionId> {
        let ext_type = self
            .detect_extension_type_string(path)
            .context("No adapter found for extension")?;
        
        // Get the extension type string first, then look up the adapter
        let adapter = self.adapters.get(&ext_type)
            .context("Adapter not found")?;
        
        // Clone needed data before calling load_extension_internal
        let ext_type = adapter.extension_type().to_string();
        let adapter_ref: &dyn ExtensionTypeAdapter = adapter.as_ref();

        let (extension_id, hook_ids, manifest) = self.load_extension_internal(path, adapter_ref).await?;

        let loaded_ext = LoadedExtension {
            manifest,
            extension_type: ext_type,
            hook_ids,
            enabled: true,
            path: path.to_path_buf(),
        };

        self.extensions.insert(extension_id.clone(), loaded_ext);

        Ok(extension_id)
    }

    pub async fn install(&mut self, path: &Path) -> Result<ExtensionId> {
        if !path.exists() {
            anyhow::bail!("Extension path does not exist: {:?}", path);
        }

        let ext_type = self
            .detect_extension_type_string(path)
            .context(format!("No adapter found for extension at {:?}", path))?;
        
        // Get the adapter and extract data we need
        let adapter = self.adapters.get(&ext_type)
            .context("Adapter not found")?;
        
        // Clone data we need from the adapter
        let adapter_ref = adapter.as_ref();
        let ext_type_name = adapter_ref.extension_type().to_string();
        let format = adapter_ref.manifest_format();
        let manifest_path = format
            .manifest_path(path)
            .context("Could not determine manifest path")?;
        
        // Parse manifest before any mutable borrow
        let manifest = self.parse_manifest(&manifest_path, adapter_ref).await?;
        let extension_id = manifest.id.clone();

        // Copy to storage if storage is configured
        let target_path = if self.storage.dir().is_some() {
            self.storage.copy_to_storage(path, &extension_id)?
        } else {
            path.to_path_buf()
        };

        // Get adapter again for load_extension_internal
        let adapter = self.adapters.get(&ext_type)
            .context("Adapter not found")?;
        let adapter_ref = adapter.as_ref();

        // Load the extension
        let (installed_id, hook_ids, _) = if self.storage.dir().is_some() {
            self.load_extension_internal(&target_path, adapter_ref).await?
        } else {
            self.load_extension_internal(path, adapter_ref).await?
        };
        
        assert_eq!(installed_id, extension_id);

        let loaded_ext = LoadedExtension {
            manifest,
            extension_type: ext_type,
            hook_ids,
            enabled: true,
            path: target_path,
        };

        self.extensions.insert(extension_id.clone(), loaded_ext);

        info!("Installed extension '{}' ({})", extension_id, ext_type_name);

        Ok(extension_id)
    }

    pub async fn uninstall(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .remove(id)
            .context(format!("Extension '{}' not found", id))?;

        // Unregister all hooks
        for hook_id in &loaded_ext.hook_ids {
            if let Err(e) = self.core.unregister_hook(hook_id).await {
                warn!("Failed to unregister hook {} for extension {}: {}", hook_id, id, e);
            }
        }

        // Shutdown if stateful
        if let Some(state) = self.extension_states.remove(id) {
            if let Some(adapter) = self.adapters.get(&loaded_ext.extension_type) {
                if let Err(e) = adapter.shutdown(state).await {
                    warn!("Error shutting down extension {}: {}", id, e);
                }
            }
        }

        // Remove from storage
        if let Err(e) = self.storage.remove_from_storage(id) {
            warn!("Failed to remove extension from storage: {}", e);
        }

        info!("Uninstalled extension '{}'", id);

        Ok(())
    }

    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .get_mut(id)
            .context(format!("Extension '{}' not found", id))?;

        for hook_id in &loaded_ext.hook_ids {
            self.core.enable_hook(hook_id).await?;
        }

        loaded_ext.enabled = true;
        info!("Enabled extension '{}'", id);

        Ok(())
    }

    pub async fn disable(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .get_mut(id)
            .context(format!("Extension '{}' not found", id))?;

        for hook_id in &loaded_ext.hook_ids {
            self.core.disable_hook(hook_id).await?;
        }

        loaded_ext.enabled = false;
        info!("Disabled extension '{}'", id);

        Ok(())
    }

    pub fn list_extensions(&self) -> Vec<&LoadedExtension> {
        self.extensions.values().collect()
    }

    pub fn get_extension(&self, id: &ExtensionId) -> Option<&LoadedExtension> {
        self.extensions.get(id)
    }

    pub fn create_bundle(&self, ids: Vec<ExtensionId>, name: &str) -> Result<ExtensionBundle> {
        let mut extensions = Vec::new();
        let mut dependencies = Vec::new();
        let mut conflicts = Vec::new();

        for id in ids {
            let ext = self
                .extensions
                .get(&id)
                .context(format!("Extension '{}' not found for bundling", id))?;
            extensions.push(ext.manifest.clone());

            // Collect dependencies from metadata if present
            if let Some(deps) = ext.manifest.get("dependencies") {
                if let Some(deps_array) = deps.as_array() {
                    for dep in deps_array {
                        if let Some(dep_str) = dep.as_str() {
                            dependencies.push(dep_str.to_string());
                        }
                    }
                }
            }

            // Collect conflicts from metadata if present
            if let Some(conf) = ext.manifest.get("conflicts") {
                if let Some(conf_array) = conf.as_array() {
                    for c in conf_array {
                        if let Some(c_str) = c.as_str() {
                            conflicts.push(c_str.to_string());
                        }
                    }
                }
            }
        }

        let metadata = BundleMetadata {
            version: "1.0.0".to_string(),
            description: format!("Bundle containing {} extensions", extensions.len()),
            dependencies,
            conflicts,
        };

        Ok(ExtensionBundle {
            name: name.to_string(),
            extensions,
            metadata,
        })
    }

    pub async fn install_bundle(&mut self, bundle: ExtensionBundle) -> Result<Vec<ExtensionId>> {
        let mut installed_ids = Vec::new();

        // Check for conflicts
        for conflict in &bundle.metadata.conflicts {
            if self.extensions.contains_key(&ExtensionId::new(conflict)) {
                anyhow::bail!("Bundle conflicts with installed extension: {}", conflict);
            }
        }

        // Install each extension in the bundle
        for manifest in &bundle.extensions {
            let id = manifest.id.clone();
            let path = manifest.path.clone();

            if !path.exists() {
                warn!("Extension path does not exist: {:?}", path);
                continue;
            }

            match self.install(&path).await {
                Ok(installed_id) => {
                    installed_ids.push(installed_id);
                }
                Err(e) => {
                    warn!("Failed to install extension '{}' from bundle: {}", id, e);
                }
            }
        }

        info!(
            "Installed bundle '{}' with {}/{} extensions",
            bundle.name,
            installed_ids.len(),
            bundle.extensions.len()
        );

        Ok(installed_ids)
    }
}

impl Default for ExtensionManager {
    fn default() -> Self {
        Self::new()
    }
}

pub mod discovery_paths {
    use std::path::PathBuf;

    pub fn user_config() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pekobot/extensions"))
    }

    pub fn user_data() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("pekobot/extensions"))
    }

    pub fn project_local() -> PathBuf {
        PathBuf::from(".pekobot/extensions")
    }

    pub fn system_wide() -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            Some(PathBuf::from("/usr/share/pekobot/extensions"))
        }
        #[cfg(target_os = "macos")]
        {
            Some(PathBuf::from("/Library/Application Support/pekobot/extensions"))
        }
        #[cfg(target_os = "windows")]
        {
            Some(PathBuf::from("C:\\ProgramData\\pekobot\\extensions"))
        }
    }

    pub fn all() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(p) = user_config() {
            paths.push(p);
        }
        if let Some(p) = user_data() {
            paths.push(p);
        }
        paths.push(project_local());
        if let Some(p) = system_wide() {
            paths.push(p);
        }

        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_manager_creation() {
        let manager = ExtensionManager::new();
        assert!(manager.list_extensions().is_empty());
    }

    #[test]
    fn test_discovery_paths() {
        let paths = discovery_paths::all();
        assert!(!paths.is_empty());
        assert!(paths.contains(&PathBuf::from(".pekobot/extensions")));
    }

    #[test]
    fn test_extension_bundle() {
        let bundle = ExtensionBundle {
            name: "test-bundle".to_string(),
            extensions: vec![],
            metadata: BundleMetadata {
                version: "1.0.0".to_string(),
                description: "Test bundle".to_string(),
                dependencies: vec![],
                conflicts: vec![],
            },
        };

        assert_eq!(bundle.name, "test-bundle");
    }
}
