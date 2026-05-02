//! Extension Manager
//!
//! This module provides unified lifecycle management for extensions:
//! - Discovery from standard locations
//! - Installation and uninstallation
//! - Enable/disable control
//! - Bundling and packaging

use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter};
use crate::extensions::core::ExtensionCore;
// Re-export storage types for backward compatibility
pub use crate::extensions::manager::storage::ExtensionStorage;

use crate::extensions::manager::discovery::{DiscoveredExtension, discovery_paths};
use crate::extensions::types::{ExtensionId, ExtensionManifest, HookId};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

// Re-export submodules
pub mod discovery;
pub mod storage;

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
///
/// Note: Enable/disable state is NOT stored here. It is managed by the agent/team
/// configuration (AgentConfig.tools.enabled whitelist). `ExtensionManager` handles
/// loading and lifecycle; access control is determined by configuration.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    pub extension_type: String,
    pub hook_ids: Vec<HookId>,
    pub path: PathBuf,
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
    #[must_use]
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

    #[must_use]
    pub fn with_storage(storage: ExtensionStorage) -> Self {
        Self {
            adapters: HashMap::new(),
            extensions: HashMap::new(),
            storage,
            core: Arc::new(ExtensionCore::new()),
            extension_states: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_storage_dir(mut self, storage_dir: PathBuf) -> Self {
        self.storage = ExtensionStorage::with_dir(storage_dir);
        self
    }

    #[must_use]
    pub fn core(&self) -> &ExtensionCore {
        &self.core
    }

    #[must_use]
    pub fn core_arc(&self) -> Arc<ExtensionCore> {
        self.core.clone()
    }

    pub fn register_adapter(&mut self, adapter: Box<dyn ExtensionTypeAdapter>) {
        let ext_type = adapter.extension_type().to_string();
        debug!("Registering adapter for extension type: {}", ext_type);
        self.adapters.insert(ext_type, adapter);
    }

    fn detect_extension_type(&self, path: &Path) -> Option<&dyn ExtensionTypeAdapter> {
        self.detect_extension_type_string(path)
            .and_then(|ext_type| self.adapters.get(&ext_type).map(|a| a.as_ref()))
    }

    /// Detect extension type using the three-tier hierarchy (ADR-024).
    ///
    /// Tier 1: Ecosystem standards (SKILL.md, server.json)
    /// Tier 2: Unified manifest (manifest.yaml with `extension_type`)
    /// Tier 3: Legacy fallback (manifest.json, config.toml, untyped manifest.yaml)
    fn detect_extension_type_string(&self, path: &Path) -> Option<String> {
        use crate::extensions::adapters::extract_extension_type_from_yaml;
        use tracing::warn;

        // ─── TIER 1: Ecosystem Standards ─────────────────────────────────────

        // SKILL.md → skill adapter
        if path.join("SKILL.md").exists() {
            tracing::debug!("Detected Tier 1 ecosystem standard: SKILL.md -> skill");
            return Some("skill".to_string());
        }

        // server.json → mcp adapter (bare MCP Registry standard)
        if path.join("server.json").exists() {
            tracing::debug!("Detected Tier 1 ecosystem standard: server.json -> mcp");
            return Some("mcp".to_string());
        }

        // ─── TIER 2: Unified Manifest ────────────────────────────────────────

        let manifest_yaml = path.join("manifest.yaml");
        if manifest_yaml.exists() {
            match extract_extension_type_from_yaml(&manifest_yaml) {
                Ok(Some(ext_type)) => {
                    tracing::debug!(
                        "Detected Tier 2 unified manifest: manifest.yaml -> {}",
                        ext_type
                    );
                    return Some(ext_type);
                }
                Ok(None) => {
                    // manifest.yaml exists but has no extension_type — fall through to Tier 3
                    tracing::debug!(
                        "manifest.yaml exists but has no extension_type; checking Tier 3 fallback"
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to parse manifest.yaml: {}; checking Tier 3 fallback", e);
                }
            }
        }

        // ─── TIER 3: Legacy Fallback (deprecated) ────────────────────────────

        // manifest.json → universal-tool adapter (legacy)
        if path.join("manifest.json").exists() {
            warn!(
                "Legacy manifest.json detected at {}. Use manifest.yaml with extension_type: 'universal-tool' instead.",
                path.display()
            );
            return Some("universal-tool".to_string());
        }

        // config.toml / config.json → mcp adapter (legacy)
        if path.join("config.toml").exists() || path.join("config.json").exists() {
            warn!(
                "Legacy config.toml/config.json detected at {}. Use manifest.yaml with extension_type: 'mcp' or ship a server.json for bare MCP servers instead.",
                path.display()
            );
            return Some("mcp".to_string());
        }

        // Untyped manifest.yaml → general adapter (the natural default)
        if manifest_yaml.exists() {
            warn!(
                "Untyped manifest.yaml detected at {}. Add extension_type: 'general' (or the appropriate type) to silence this warning.",
                path.display()
            );
            return Some("general".to_string());
        }

        tracing::debug!("No extension manifest detected at {}", path.display());
        None
    }

    async fn load_extension_internal(
        &self,
        path: &Path,
        adapter: &dyn ExtensionTypeAdapter,
    ) -> Result<(ExtensionId, Vec<HookId>, ExtensionManifest)> {
        let format = adapter.manifest_format();

        // For custom formats, the path itself may be the manifest file
        // or a directory containing the manifest file
        let manifest_path = match format.manifest_path(path) {
            Some(p) => p,
            None => {
                if path.is_file() {
                    path.to_path_buf()
                } else if path.is_dir() {
                    // Try to find config.toml or config.json in the directory
                    let toml_path = path.join("config.toml");
                    let json_path = path.join("config.json");
                    if toml_path.exists() {
                        toml_path
                    } else if json_path.exists() {
                        json_path
                    } else {
                        anyhow::bail!("Could not determine manifest path for {path:?}")
                    }
                } else {
                    anyhow::bail!("Could not determine manifest path for {path:?}")
                }
            }
        };

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

        // Register tools via the unified registry (single canonical path)
        let tool_count = adapter.register_tools(&self.core, &manifest).await.unwrap_or(0);

        info!(
            "Loaded extension '{}' ({}) with {} hooks and {} tools",
            extension_id,
            ext_type,
            hook_ids.len(),
            tool_count,
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
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read manifest at {path:?}"))?;

        // Use the adapter's parse_manifest method to allow custom parsing
        adapter
            .parse_manifest(path, &content)
            .with_context(|| format!("Failed to parse manifest at {path:?}"))
    }

    pub async fn load_all(&mut self) -> Result<LoadReport> {
        let mut report = LoadReport::default();
        let mut scanned_paths = std::collections::HashSet::new();

        // Collect all paths to scan (discovery paths + storage)
        let mut all_paths = discovery_paths::all();
        if let Some(storage_dir) = self.storage.dir() {
            all_paths.push(storage_dir.to_path_buf());
        }

        for base_path in all_paths {
            if !base_path.exists() {
                continue;
            }

            // Avoid scanning the same path twice
            let canonical = std::fs::canonicalize(&base_path).unwrap_or(base_path.clone());
            if scanned_paths.contains(&canonical) {
                continue;
            }
            scanned_paths.insert(canonical);

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
        let adapter = self.adapters.get(&ext_type).context("Adapter not found")?;

        // Clone needed data before calling load_extension_internal
        let ext_type = adapter.extension_type().to_string();
        let adapter_ref: &dyn ExtensionTypeAdapter = adapter.as_ref();

        let (extension_id, hook_ids, manifest) =
            self.load_extension_internal(path, adapter_ref).await?;

        let loaded_ext = LoadedExtension {
            manifest,
            extension_type: ext_type,
            hook_ids,
            path: path.to_path_buf(),
        };

        self.extensions.insert(extension_id.clone(), loaded_ext);

        Ok(extension_id)
    }

    pub async fn install(&mut self, path: &Path) -> Result<ExtensionId> {
        if !path.exists() {
            anyhow::bail!("Extension path does not exist: {path:?}");
        }

        let ext_type = self
            .detect_extension_type_string(path)
            .context(format!("No adapter found for extension at {path:?}"))?;

        // Get the adapter and extract data we need
        let adapter = self.adapters.get(&ext_type).context("Adapter not found")?;

        // Clone data we need from the adapter
        let adapter_ref = adapter.as_ref();
        let ext_type_name = adapter_ref.extension_type().to_string();
        let format = adapter_ref.manifest_format();

        // For custom formats, the path itself may be the manifest file
        let manifest_path = match format.manifest_path(path) {
            Some(p) => p,
            None => {
                // Custom format: check if path is a file (direct manifest) or directory
                if path.is_file() {
                    path.to_path_buf()
                } else if path.is_dir() {
                    // For directories with Custom format, look for manifest files
                    let candidates = vec![
                        path.join("config.toml"),
                        path.join("config.json"),
                        path.join("manifest.json"),
                        path.join("manifest.toml"),
                    ];
                    candidates
                        .into_iter()
                        .find(|p| p.exists())
                        .context(format!(
                            "Could not find manifest file in directory {path:?}"
                        ))?
                } else {
                    anyhow::bail!("Could not determine manifest path for {path:?}")
                }
            }
        };

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
        let adapter = self.adapters.get(&ext_type).context("Adapter not found")?;
        let adapter_ref = adapter.as_ref();

        // Load the extension
        let (installed_id, hook_ids, _) = if self.storage.dir().is_some() {
            self.load_extension_internal(&target_path, adapter_ref)
                .await?
        } else {
            self.load_extension_internal(path, adapter_ref).await?
        };

        assert_eq!(installed_id, extension_id);

        let loaded_ext = LoadedExtension {
            manifest,
            extension_type: ext_type,
            hook_ids,
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
            .context(format!("Extension '{id}' not found"))?;

        // Unregister all hooks
        for hook_id in &loaded_ext.hook_ids {
            if let Err(e) = self.core.unregister_hook(hook_id).await {
                warn!(
                    "Failed to unregister hook {} for extension {}: {}",
                    hook_id, id, e
                );
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

    /// Enable hooks for an extension at runtime.
    ///
    /// Note: This only affects hook registration state, not tool access.
    /// Tool access is controlled by AgentConfig.tools.enabled whitelist.
    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .get(id)
            .context(format!("Extension '{id}' not found"))?;

        for hook_id in &loaded_ext.hook_ids {
            self.core.enable_hook(hook_id).await?;
        }

        info!("Enabled extension hooks for '{}'", id);

        Ok(())
    }

    /// Disable hooks for an extension at runtime.
    ///
    /// Note: This only affects hook registration state, not tool access.
    /// Tool access is controlled by AgentConfig.tools.enabled whitelist.
    pub async fn disable(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .get(id)
            .context(format!("Extension '{id}' not found"))?;

        for hook_id in &loaded_ext.hook_ids {
            self.core.disable_hook(hook_id).await?;
        }

        info!("Disabled extension hooks for '{}'", id);

        Ok(())
    }

    #[must_use]
    pub fn list_extensions(&self) -> Vec<&LoadedExtension> {
        self.extensions.values().collect()
    }

    #[must_use]
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
                .context(format!("Extension '{id}' not found for bundling"))?;
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
                anyhow::bail!("Bundle conflicts with installed extension: {conflict}");
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

    // ============================================================================
    // Built-in Tool Support (removed in ADR-019; use ExtensionCore directly)
    // ============================================================================

    /// Scan a specific directory for extensions without loading them
    ///
    /// This is used for:
    /// - Legacy tools directory (~/.pekobot/tools/)
    /// - Workspace custom tools (./tools/)
    /// - CAP catalog discovery
    ///
    /// Unlike `load_all()`, this scans an arbitrary path, not just `discovery_paths`.
    pub async fn scan_directory(&self, path: &Path) -> Result<Vec<DiscoveredExtension>> {
        let mut discovered = Vec::new();

        if !path.exists() {
            tracing::debug!("Extensions directory does not exist: {}", path.display());
            return Ok(discovered);
        }

        tracing::debug!("Scanning directory: {}", path.display());
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if !path.is_dir() {
                tracing::trace!("Skipping non-directory: {}", name);
                continue;
            }

            tracing::debug!("Checking directory: {}", name);

            // Try to detect extension type using registered adapters
            if let Some(adapter) = self.detect_extension_type(&path) {
                let format = adapter.manifest_format();
                tracing::info!(
                    "Detected extension type '{}' in {}",
                    adapter.extension_type(),
                    name
                );

                // Get manifest path - for Custom formats, we need to determine it manually
                let manifest_path = if let Some(p) = format.manifest_path(&path) {
                    p
                } else {
                    // For Custom formats, check common manifest file names
                    let candidates = vec![
                        path.join("config.toml"),
                        path.join("config.json"),
                        path.join("manifest.json"),
                        path.join("manifest.toml"),
                    ];
                    candidates
                        .into_iter()
                        .find(|p| p.exists())
                        .unwrap_or_else(|| path.join("config.toml"))
                };

                discovered.push(DiscoveredExtension {
                    path,
                    manifest_path,
                    extension_type: adapter.extension_type().to_string(),
                });
            } else {
                tracing::debug!("No adapter detected extension type for: {}", name);
            }
        }

        Ok(discovered)
    }

    /// Load extensions from a directory without installing to storage
    ///
    /// This loads extensions into the manager and registers their hooks,
    /// but does NOT copy them to the storage directory.
    ///
    /// Returns the IDs of loaded extensions.
    pub async fn load_from_directory(&mut self, path: &Path) -> Result<Vec<ExtensionId>> {
        tracing::info!("Scanning directory for extensions: {}", path.display());
        let discovered = self.scan_directory(path).await?;
        tracing::info!("Discovered {} extensions", discovered.len());
        let mut loaded_ids = Vec::new();

        for discovered_ext in discovered {
            // Check if already loaded
            let manifest_content = tokio::fs::read_to_string(&discovered_ext.manifest_path).await?;
            let adapter = self
                .adapters
                .get(&discovered_ext.extension_type)
                .context("Adapter not found for extension type")?;

            let manifest =
                adapter.parse_manifest(&discovered_ext.manifest_path, &manifest_content)?;

            if self.extensions.contains_key(&manifest.id) {
                debug!("Extension '{}' already loaded, skipping", manifest.id);
                continue;
            }

            // Load the extension
            match self.try_load_extension(&discovered_ext.path).await {
                Ok(id) => {
                    loaded_ids.push(id);
                }
                Err(e) => {
                    warn!(
                        "Failed to load extension from {:?}: {}",
                        discovered_ext.path, e
                    );
                }
            }
        }

        Ok(loaded_ids)
    }

    // get_tool_instances, create_tool_instance, and create_mcp_tool_instances
    // removed in ADR-019. All tool execution now flows through ExtensionCore
    // hooks; ExtensionManager is strictly a lifecycle manager.
}

impl Default for ExtensionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_extension_manager_creation() {
        let manager = ExtensionManager::new();
        assert!(manager.list_extensions().is_empty());
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

    // ─── ADR-024: Three-tier detection hierarchy tests ─────────────────────

    #[test]
    fn test_detect_tier1_skill_md() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-skill");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("SKILL.md"), "---\nname: My Skill\n---\n").unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("skill".to_string())
        );
    }

    #[test]
    fn test_detect_tier1_server_json() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-mcp");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("server.json"), r#"{"name": "test"}"#).unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("mcp".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_manifest_yaml_with_extension_type() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-gateway");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: gw\nname: Gateway\nextension_type: gateway\ngateway_type: pubsub\n",
        )
        .unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("gateway".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_universal_tool_yaml() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-tool");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: calc\nname: Calculator\nextension_type: universal-tool\n",
        )
        .unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("universal-tool".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_general_yaml() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-general");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: gen\nname: General\nextension_type: general\n",
        )
        .unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("general".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_custom_type() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-custom");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: custom\nname: Custom\nextension_type: custom:my-org/type\n",
        )
        .unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("custom:my-org/type".to_string())
        );
    }

    #[test]
    fn test_detect_tier3_legacy_manifest_json() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("legacy-tool");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("manifest.json"), r#"{"name": "legacy"}"#).unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("universal-tool".to_string())
        );
    }

    #[test]
    fn test_detect_tier3_legacy_config_toml() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("legacy-mcp");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("config.toml"), "[[servers]]\nname = \"legacy\"\n").unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("mcp".to_string())
        );
    }

    #[test]
    fn test_detect_tier3_untyped_manifest_yaml() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("untyped");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("manifest.yaml"), "id: untyped\nname: Untyped\n").unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("general".to_string())
        );
    }

    #[test]
    fn test_detect_tier1_skill_takes_precedence_over_tier2() {
        // If both SKILL.md and manifest.yaml exist, Tier 1 wins
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("hybrid");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("SKILL.md"), "---\nname: Skill\n---\n").unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: hybrid\nname: Hybrid\nextension_type: general\n",
        )
        .unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("skill".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_takes_precedence_over_tier3() {
        // If both manifest.yaml (typed) and manifest.json exist, Tier 2 wins
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("hybrid2");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: hybrid\nname: Hybrid\nextension_type: gateway\ngateway_type: http\n",
        )
        .unwrap();
        std::fs::write(ext_dir.join("manifest.json"), r#"{"name": "legacy"}"#).unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(
            manager.detect_extension_type_string(&ext_dir),
            Some("gateway".to_string())
        );
    }

    #[test]
    fn test_detect_nothing_found() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("empty");
        std::fs::create_dir(&ext_dir).unwrap();

        let manager = ExtensionManager::new();
        assert_eq!(manager.detect_extension_type_string(&ext_dir), None);
    }
}
