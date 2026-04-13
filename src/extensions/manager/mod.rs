//! Extension Manager
//!
//! This module provides unified lifecycle management for extensions:
//! - Discovery from standard locations
//! - Installation and uninstallation
//! - Enable/disable control
//! - Bundling and packaging

use crate::extensions::adapters::{ExtensionTypeAdapter, ExtensionState};
use crate::extensions::core::{ExtensionCore, HookPoint};
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
/// 
/// Note: Enable/disable state is NOT stored here. It is managed by the agent/team
/// configuration (AgentConfig.tools.enabled whitelist). ExtensionManager handles
/// loading and lifecycle; access control is determined by configuration.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    pub extension_type: String,
    pub hook_ids: Vec<HookId>,
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

        // Handle single file (e.g., MCP config.toml) vs directory
        if source.is_file() {
            // Create target directory and copy the file into it
            std::fs::create_dir_all(&target_dir)
                .with_context(|| format!("Failed to create target directory {:?}", target_dir))?;
            let file_name = source.file_name()
                .context("Invalid source file name")?;
            let target_file = target_dir.join(file_name);
            std::fs::copy(source, &target_file)
                .with_context(|| format!("Failed to copy file from {:?} to {:?}", source, target_file))?;
        } else {
            copy_dir_recursive(source, &target_dir)
                .with_context(|| format!("Failed to copy extension from {:?} to {:?}", source, target_dir))?;
        }

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

/// Information about a built-in tool
#[derive(Debug, Clone)]
pub struct BuiltinToolInfo {
    /// Full extension ID (e.g., "builtin:shell")
    pub id: String,
    /// Tool name without prefix (e.g., "shell")
    pub name: String,
    /// Whether the tool is currently enabled
    pub enabled: bool,
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

    pub fn with_storage(storage: ExtensionStorage) -> Self {
        Self {
            adapters: HashMap::new(),
            extensions: HashMap::new(),
            storage,
            core: Arc::new(ExtensionCore::new()),
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
        tracing::debug!("detect_extension_type: checking {} with {} adapters", path.display(), self.adapters.len());
        for adapter in self.adapters.values() {
            let format = adapter.manifest_format();
            let detected = format.detect(path);
            tracing::debug!("  Adapter '{}': detected = {}", adapter.extension_type(), detected);
            if detected {
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
                        anyhow::bail!("Could not determine manifest path for {:?}", path)
                    }
                } else {
                    anyhow::bail!("Could not determine manifest path for {:?}", path)
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

        info!(
            "Loaded extension '{}' ({}) with {} hooks",
            extension_id,
            ext_type,
            hook_ids.len()
        );

        // For universal tools, also register with the unified registry (ADR-018b)
        // This ensures tools appear in list_tool_definitions() queries
        if ext_type == "universal-tool" {
            use crate::extensions::adapters::universal_tool_adapter::UniversalToolAdapter;
            let adapter = UniversalToolAdapter::new();
            info!("Registering universal tool '{}' with unified registry...", extension_id);
            if let Err(e) = adapter.register_tool(&self.core, &manifest).await {
                warn!("Failed to register universal tool '{}' with unified registry: {}", extension_id, e);
            } else {
                info!("Successfully registered universal tool '{}' with unified registry", extension_id);
                // Verify registration
                let count = self.core.tool_count().await;
                info!("Unified registry now has {} tools", count);
            }
        }
        
        // For MCP servers, ensure config is loaded and register tools with unified registry
        if ext_type == "mcp" {
            use crate::extensions::adapters::mcp_adapter::McpAdapter;
            
            // Get config path from manifest and ensure server config is loaded
            if let Some(config_path) = manifest.get("config_path").and_then(|v| v.as_str()) {
                // Use MCP adapter with global shared manager
                let adapter = McpAdapter::with_default_manager();
                
                // First, ensure the server config is loaded into the MCP manager
                if let Err(e) = adapter.ensure_server_config(config_path).await {
                    warn!("Failed to load MCP server config for '{}': {}", extension_id, e);
                } else {
                    info!("Registering MCP server '{}' tools with unified registry...", extension_id);
                    if let Err(e) = adapter.register_server_tools(&self.core, &manifest.name).await {
                        warn!("Failed to register MCP server '{}' tools with unified registry: {}", extension_id, e);
                    } else {
                        info!("Successfully registered MCP server '{}' tools with unified registry", extension_id);
                        // Verify registration
                        let count = self.core.tool_count().await;
                        info!("Unified registry now has {} tools", count);
                    }
                }
            } else {
                warn!("MCP extension '{}' missing config_path in manifest", extension_id);
            }
        }

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
        let content = tokio::fs::read_to_string(path).await
            .with_context(|| format!("Failed to read manifest at {:?}", path))?;

        // Use the adapter's parse_manifest method to allow custom parsing
        adapter.parse_manifest(path, &content)
            .with_context(|| format!("Failed to parse manifest at {:?}", path))
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
                    candidates.into_iter()
                        .find(|p| p.exists())
                        .context(format!("Could not find manifest file in directory {:?}", path))?
                } else {
                    anyhow::bail!("Could not determine manifest path for {:?}", path)
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

    /// Enable hooks for an extension at runtime.
    /// 
    /// Note: This only affects hook registration state, not tool access.
    /// Tool access is controlled by AgentConfig.tools.enabled whitelist.
    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()> {
        let loaded_ext = self
            .extensions
            .get(id)
            .context(format!("Extension '{}' not found", id))?;

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
            .context(format!("Extension '{}' not found", id))?;

        for hook_id in &loaded_ext.hook_ids {
            self.core.disable_hook(hook_id).await?;
        }

        info!("Disabled extension hooks for '{}'", id);

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

    // ============================================================================
    // Built-in Tool Support
    // ============================================================================

    /// List all built-in tools registered with ExtensionCore
    ///
    /// Built-in tools have extension IDs starting with "builtin:"
    pub async fn list_builtin_tools(&self) -> Vec<BuiltinToolInfo> {
        let mut builtins = Vec::new();
        
        // Get all hooks from ExtensionCore
        let hooks = self.core.get_all_hooks().await;
        
        // Group hooks by extension ID
        let mut builtin_extensions: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
        
        for hook in hooks {
            if hook.extension_id.0.starts_with("builtin:") {
                builtin_extensions
                    .entry(hook.extension_id.0.clone())
                    .or_default()
                    .push(hook);
            }
        }
        
        // Create BuiltinToolInfo for each unique builtin
        for (ext_id, hooks) in builtin_extensions {
            let tool_name = ext_id.strip_prefix("builtin:").unwrap_or(&ext_id).to_string();
            let enabled = hooks.iter().any(|h| h.enabled);
            let has_execute_hook = hooks.iter().any(|h| {
                matches!(h.point, HookPoint::ToolExecute { .. })
            });
            
            if has_execute_hook {
                builtins.push(BuiltinToolInfo {
                    id: ext_id,
                    name: tool_name,
                    enabled,
                });
            }
        }
        
        builtins.sort_by(|a, b| a.name.cmp(&b.name));
        builtins
    }

    /// Enable a built-in tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The tool name (with or without "builtin:" prefix)
    pub async fn enable_builtin(&self, tool_name: &str) -> Result<()> {
        let ext_id = if tool_name.starts_with("builtin:") {
            tool_name.to_string()
        } else {
            format!("builtin:{}", tool_name)
        };
        
        let hooks = self.core.get_hooks_for_extension(&ExtensionId::new(&ext_id)).await;
        
        if hooks.is_empty() {
            anyhow::bail!("Built-in tool '{}' not found", tool_name);
        }
        
        for hook in hooks {
            self.core.enable_hook(&hook.id).await?;
        }
        
        info!("Enabled built-in tool '{}'", tool_name);
        Ok(())
    }

    /// Disable a built-in tool by name
    ///
    /// # Arguments
    /// * `tool_name` - The tool name (with or without "builtin:" prefix)
    pub async fn disable_builtin(&self, tool_name: &str) -> Result<()> {
        let ext_id = if tool_name.starts_with("builtin:") {
            tool_name.to_string()
        } else {
            format!("builtin:{}", tool_name)
        };
        
        let hooks = self.core.get_hooks_for_extension(&ExtensionId::new(&ext_id)).await;
        
        if hooks.is_empty() {
            anyhow::bail!("Built-in tool '{}' not found", tool_name);
        }
        
        for hook in hooks {
            self.core.disable_hook(&hook.id).await?;
        }
        
        info!("Disabled built-in tool '{}'", tool_name);
        Ok(())
    }

    /// Check if a built-in tool is enabled
    pub async fn is_builtin_enabled(&self, tool_name: &str) -> bool {
        let ext_id = if tool_name.starts_with("builtin:") {
            tool_name.to_string()
        } else {
            format!("builtin:{}", tool_name)
        };
        
        let hooks = self.core.get_hooks_for_extension(&ExtensionId::new(&ext_id)).await;
        hooks.iter().any(|h| h.enabled)
    }

    /// Scan a specific directory for extensions without loading them
    ///
    /// This is used for:
    /// - Legacy tools directory (~/.pekobot/tools/)
    /// - Workspace custom tools (./tools/)
    /// - CAP catalog discovery
    ///
    /// Unlike `load_all()`, this scans an arbitrary path, not just discovery_paths.
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
                tracing::info!("Detected extension type '{}' in {}", adapter.extension_type(), name);
                
                // Get manifest path - for Custom formats, we need to determine it manually
                let manifest_path = match format.manifest_path(&path) {
                    Some(p) => p,
                    None => {
                        // For Custom formats, check common manifest file names
                        let candidates = vec![
                            path.join("config.toml"),
                            path.join("config.json"),
                            path.join("manifest.json"),
                            path.join("manifest.toml"),
                        ];
                        candidates.into_iter()
                            .find(|p| p.exists())
                            .unwrap_or_else(|| path.join("config.toml"))
                    }
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
            
            let manifest = adapter
                .parse_manifest(&discovered_ext.manifest_path, &manifest_content)?;
            
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
                    warn!("Failed to load extension from {:?}: {}", discovered_ext.path, e);
                }
            }
        }

        Ok(loaded_ids)
    }

    /// Get loaded extensions as Arc<dyn Tool> trait objects
    ///
    /// This bridges the gap between ExtensionManager (tracks metadata)
    /// and Agent (needs executable Tool instances).
    ///
    /// Supports both "universal-tool" and "mcp" extension types.
    /// 
    /// Note: Tool access control is handled by the caller via `is_tool_allowed` predicate.
    /// ExtensionManager does not track enable/disable state; that is the responsibility
    /// of the agent/team configuration layer (AgentConfig.tools.enabled whitelist).
    ///
    /// # Arguments
    /// * `is_tool_allowed` - Function that takes a tool name and returns true if the tool should be included
    pub async fn get_tool_instances<F>(&self, is_tool_allowed: F) -> Vec<Arc<dyn crate::tools::Tool>>
    where
        F: Fn(&str) -> bool,
    {
        let mut tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();

        for (ext_id, loaded_ext) in &self.extensions {
            match loaded_ext.extension_type.as_str() {
                "universal-tool" => {
                    // For universal tools, the extension name is the tool name
                    let tool_name = &loaded_ext.manifest.name;
                    if is_tool_allowed(tool_name) {
                        if let Some(tool) = self.create_tool_instance(ext_id).await {
                            tools.push(tool);
                        }
                    }
                }
                "mcp" => {
                    // Create MCP tool instances and filter individually
                    let mcp_tools = self.create_mcp_tool_instances(ext_id).await;
                    for tool in mcp_tools {
                        if is_tool_allowed(tool.name()) {
                            tools.push(tool);
                        }
                    }
                }
                _ => {}
            }
        }

        tools
    }

    /// Create a Tool trait object from a loaded universal tool extension
    async fn create_tool_instance(&self, ext_id: &ExtensionId) -> Option<Arc<dyn crate::tools::Tool>> {
        let loaded = self.extensions.get(ext_id)?;

        // Find the manifest.json and executable paths
        let manifest_path = loaded.path.join("manifest.json");
        
        // Try to find executable with same name as tool
        let tool_name = &loaded.manifest.name;
        let executable_candidates = vec![
            loaded.path.join(format!("{}.py", tool_name)),
            loaded.path.join(format!("{}.js", tool_name)),
            loaded.path.join(format!("{}.sh", tool_name)),
            loaded.path.join(tool_name),
        ];

        let executable = executable_candidates
            .into_iter()
            .find(|p| p.exists())
            .or_else(|| {
                // Fallback: find any file that's not manifest.json
                std::fs::read_dir(&loaded.path)
                    .ok()?
                    .flatten()
                    .find(|e| {
                        let name = e.file_name();
                        name != "manifest.json" && e.path().is_file()
                    })
                    .map(|e| e.path())
            })?;

        // Create UniversalToolAdapter from manifest
        match crate::tools::universal::UniversalToolAdapter::from_manifest(&manifest_path, &executable).await {
            Ok(adapter) => Some(Arc::new(adapter)),
            Err(e) => {
                warn!("Failed to create tool instance for '{}': {}", ext_id, e);
                None
            }
        }
    }

    /// Create Tool trait objects from a loaded MCP extension
    async fn create_mcp_tool_instances(&self, ext_id: &ExtensionId) -> Vec<Arc<dyn crate::tools::Tool>> {
        let loaded = match self.extensions.get(ext_id) {
            Some(l) => l,
            None => return Vec::new(),
        };

        // Get server name from manifest
        let server_name = &loaded.manifest.name;

        // Create MCP adapter with the global shared manager
        let adapter = crate::extensions::adapters::McpAdapter::with_default_manager();
        
        // Start the MCP server if not already running
        // This is needed because list_all_tools only returns tools from running servers
        let manager = adapter.manager();
        {
            let mgr = manager.read().await;
            let server_state = mgr.get_server_state(server_name).await;
            drop(mgr);
            
            if server_state.is_err() || !server_state.unwrap().running {
                let mut mgr = manager.write().await;
                if let Err(e) = mgr.start_server(server_name).await {
                    warn!("Failed to start MCP server '{}': {}", server_name, e);
                    return Vec::new();
                }
            }
        }

        // Create tool instances
        match adapter.create_tool_instances(server_name).await {
            Ok(tools) => {
                info!("Created {} MCP tool instances for '{}'", tools.len(), ext_id);
                tools
            }
            Err(e) => {
                warn!("Failed to create MCP tool instances for '{}': {}", ext_id, e);
                Vec::new()
            }
        }
    }
}

/// Discovered extension before loading
#[derive(Debug, Clone)]
pub struct DiscoveredExtension {
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub extension_type: String,
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
