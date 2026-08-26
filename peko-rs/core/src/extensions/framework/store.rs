//! Global Extension Store
//!
//! Owns the canonical runtime extension state for the whole process:
//! registered adapters, loaded extensions, extension states, and the shared
//! [`ExtensionCore`]. Callers hold an `Arc<ExtensionStore>`; locking is
//! internal, so read-heavy paths do not force every consumer to manage an
//! `RwLock` guard.
//!
//! The per-Principal snapshot is built by the Principal layer from
//! [`GlobalExtensionItem`] data returned by this store, merged with workspace
//! agents and built-ins.

use crate::extensions::framework::adapters::{ExtensionState, ExtensionTypeAdapter};
use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::extension_storage::ExtensionStorage;

use crate::extensions::framework::discovery::{discovery_paths, DiscoveredExtension};
use crate::extensions::framework::types::HookId;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// Phase 8c.1.D.2: data types now live in `peko_extension_host::store`
// (the trait port's home). Internal-only — callers reach for these
// directly via the host crate's path.
//
// PR-B (extension removal): `BundleMetadata`, `ExtensionBundle`,
// `DependencyResolution`, `DependencyStatus` dropped along with
// `create_bundle` / `install_bundle` / `resolve_dependencies*`. Zero
// production callers; with `peko ext *` retired in Phase 5
// (ADR-047 §2.1) there is no bundle install / dependency-resolution
// flow left.
use crate::extensions::framework::store_trait::{
    ExtensionStore as ExtensionStoreTrait, GlobalExtensionItem, LoadReport, LoadedExtension,
    ToolResolution,
};
use peko_extension_api::{ExtensionId, ExtensionManifest};

/// Extension Store - Central, process-wide owner of extension runtime state.
#[derive(Debug, Clone)]
pub struct ExtensionStore {
    inner: Arc<RwLock<ExtensionStoreInner>>,
    storage: ExtensionStorage,
    core: Arc<ExtensionCore>,
}

#[derive(Debug)]
struct ExtensionStoreInner {
    adapters: HashMap<String, Box<dyn ExtensionTypeAdapter>>,
    extensions: HashMap<ExtensionId, LoadedExtension>,
    extension_states: HashMap<ExtensionId, ExtensionState>,
}

// Phase 8c.1.D.2: blanket impl of the host trait port. Lets the
// concrete `ExtensionStore` flow through `Arc<dyn ExtensionStore>` for
// the packaging layer (which can no longer hold the root-concrete
// type across crate boundaries). Mirrors the `VaultAccess` impl at
// `src/common/vault.rs:2274`.
#[async_trait::async_trait]
impl ExtensionStoreTrait for ExtensionStore {
    async fn get_extension(&self, id: &ExtensionId) -> Option<LoadedExtension> {
        ExtensionStore::get_extension(self, id).await
    }

    async fn resolve_tool_name(&self, name: &str) -> Option<ToolResolution> {
        ExtensionStore::resolve_tool_name(self, name).await
    }

    async fn install(&self, path: &Path) -> Result<ExtensionId> {
        ExtensionStore::install(self, path).await
    }
}

impl ExtensionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ExtensionStoreInner {
                adapters: HashMap::new(),
                extensions: HashMap::new(),
                extension_states: HashMap::new(),
            })),
            storage: ExtensionStorage::new(),
            core: Arc::new(ExtensionCore::new()),
        }
    }

    pub fn with_core(core: Arc<ExtensionCore>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ExtensionStoreInner {
                adapters: HashMap::new(),
                extensions: HashMap::new(),
                extension_states: HashMap::new(),
            })),
            storage: ExtensionStorage::new(),
            core,
        }
    }

    #[must_use]
    pub fn with_storage_dir(mut self, storage_dir: PathBuf) -> Self {
        self.storage = ExtensionStorage::with_dir(storage_dir);
        self
    }

    /// Get the storage directory if configured.
    #[must_use]
    pub fn storage_dir(&self) -> Option<&Path> {
        self.storage.dir()
    }

    /// Get a reference to the storage backend.
    #[must_use]
    pub fn storage(&self) -> &ExtensionStorage {
        &self.storage
    }

    /// Persist and set the registry source reference for an installed
    /// extension.
    pub async fn set_source(&self, id: &ExtensionId, registry_ref: &str) -> Result<()> {
        if self.storage.dir().is_some() {
            self.storage().write_source(id, registry_ref)?;
        }
        let mut inner = self.inner.write().await;
        if let Some(loaded) = inner.extensions.get_mut(id) {
            loaded.manifest.source = Some(registry_ref.to_string());
        }
        Ok(())
    }

    #[must_use]
    pub fn core(&self) -> &ExtensionCore {
        &self.core
    }

    #[must_use]
    pub fn core_arc(&self) -> Arc<ExtensionCore> {
        self.core.clone()
    }

    pub async fn register_adapter(&self, adapter: Box<dyn ExtensionTypeAdapter>) {
        let ext_type = adapter.extension_type().to_string();
        debug!("Registering adapter for extension type: {}", ext_type);
        let mut inner = self.inner.write().await;
        inner.adapters.insert(ext_type, adapter);
    }

    /// Detect extension type using the two-tier hierarchy (ADR-024).
    ///
    /// Tier 1: Ecosystem standards (SKILL.md, AGENT.md, server.json)
    /// Tier 2: Unified manifest (manifest.yaml with `extension_type`)
    pub(crate) fn detect_extension_type_string(&self, path: &Path) -> Option<String> {
        use crate::extensions::framework::adapters::extract_extension_type_from_yaml;

        // ─── TIER 1: Ecosystem Standards ─────────────────────────────────────

        if path.join("SKILL.md").exists() {
            tracing::debug!("Detected Tier 1 ecosystem standard: SKILL.md -> skill");
            return Some("skill".to_string());
        }

        if path.join("AGENT.md").exists() {
            tracing::debug!("Detected Tier 1 ecosystem standard: AGENT.md -> agent");
            return Some("agent".to_string());
        }

        if path.join("COMMAND.md").exists() {
            tracing::debug!("Detected Tier 1 ecosystem standard: COMMAND.md -> slash");
            return Some("slash".to_string());
        }

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
                    Some(ext_type)
                }
                Ok(None) => {
                    tracing::debug!("manifest.yaml exists but has no extension_type");
                    None
                }
                Err(e) => {
                    tracing::warn!("Failed to parse manifest.yaml: {}", e);
                    None
                }
            }
        } else {
            tracing::debug!("No extension manifest detected at {}", path.display());
            None
        }
    }

    async fn load_extension_internal(
        &self,
        path: &Path,
        adapter: &dyn ExtensionTypeAdapter,
    ) -> Result<(ExtensionId, Vec<HookId>, ExtensionManifest)> {
        let format = adapter.manifest_format();

        let manifest_path = match format.manifest_path(path) {
            Some(p) => p,
            None => {
                if path.is_file() {
                    path.to_path_buf()
                } else if path.is_dir() {
                    let candidates = [path.join("manifest.yaml"), path.join("server.json")];
                    candidates
                        .into_iter()
                        .find(|p| p.exists())
                        .context(format!("Could not determine manifest path for {path:?}"))?
                } else {
                    anyhow::bail!("Could not determine manifest path for {path:?}")
                }
            }
        };

        let manifest = self.parse_manifest(&manifest_path, adapter).await?;
        let extension_id = manifest.id.clone();
        let ext_type = adapter.extension_type().to_string();

        let state = adapter.initialize(&manifest).await?;

        let bindings = adapter.resolve_hooks(&manifest);
        let mut hook_ids = Vec::new();

        for binding in bindings {
            let handler = binding.handler_factory.create(manifest.clone());
            let handler_arc: Arc<dyn crate::extensions::framework::core::HookHandler> =
                handler.into();
            let registration = self
                .core
                .register_hook(binding.point, handler_arc, &extension_id)
                .await?;
            hook_ids.push(registration.id);
        }

        let tool_count = adapter
            .register_tools(&self.core, &manifest, peko_subject::PrincipalId::system())
            .await
            .unwrap_or(0);

        info!(
            "Loaded extension '{}' ({}) with {} hooks and {} tools",
            extension_id,
            ext_type,
            hook_ids.len(),
            tool_count,
        );

        if !state.is_unit() {
            // Stateful extensions are shut down via `extension_states` on uninstall.
            // Storing the state here would require mutable access; for now the
            // lifecycle is hook-based, matching the previous ExtensionManager.
            let _ = state;
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

        adapter
            .parse_manifest(path, &content)
            .with_context(|| format!("Failed to parse manifest at {path:?}"))
    }

    fn populate_source_from_storage(
        storage: &ExtensionStorage,
        extensions: &mut HashMap<ExtensionId, LoadedExtension>,
        id: &ExtensionId,
    ) {
        if let Some(loaded) = extensions.get_mut(id) {
            if loaded.manifest.source.is_none() {
                if let Some(source) = storage.read_source(id) {
                    loaded.manifest.source = Some(source);
                }
            }
        }
    }

    pub async fn load_all(&self) -> Result<LoadReport> {
        let mut report = LoadReport::default();
        let mut scanned_paths = HashSet::new();

        crate::extensions::framework::skill_catalog::SkillCatalog::global().clear();

        let mut all_paths = discovery_paths::all();
        if let Some(storage_dir) = self.storage.dir() {
            all_paths.push(storage_dir.to_path_buf());
        }
        let path_resolver = crate::common::paths::PathResolver::new();
        all_paths.push(path_resolver.skills_dir());
        all_paths.push(path_resolver.agents_dir());
        all_paths.push(path_resolver.commands_dir());

        for base_path in all_paths {
            if !base_path.exists() {
                continue;
            }

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

    async fn try_load_extension(&self, path: &Path) -> Result<ExtensionId> {
        let ext_type = self
            .detect_extension_type_string(path)
            .context("No adapter found for extension")?;

        let mut inner = self.inner.write().await;
        let adapter = inner.adapters.get(&ext_type).context("Adapter not found")?;

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

        inner.extensions.insert(extension_id.clone(), loaded_ext);
        let loaded_ref = inner.extensions.get(&extension_id).expect("just inserted");
        Self::register_skill_catalog(self.core.clone(), loaded_ref);
        Self::populate_source_from_storage(&self.storage, &mut inner.extensions, &extension_id);

        Ok(extension_id)
    }

    pub async fn install(&self, path: &Path) -> Result<ExtensionId> {
        if !path.exists() {
            anyhow::bail!("Extension path does not exist: {path:?}");
        }

        let ext_type = self
            .detect_extension_type_string(path)
            .context(format!("No adapter found for extension at {path:?}"))?;

        let mut inner = self.inner.write().await;
        let adapter = inner.adapters.get(&ext_type).context("Adapter not found")?;

        let adapter_ref = adapter.as_ref();
        let ext_type_name = adapter_ref.extension_type().to_string();
        let format = adapter_ref.manifest_format();

        let manifest_path = match format.manifest_path(path) {
            Some(p) => p,
            None => {
                if path.is_file() {
                    path.to_path_buf()
                } else if path.is_dir() {
                    let candidates = vec![path.join("manifest.yaml"), path.join("server.json")];
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

        let manifest = self.parse_manifest(&manifest_path, adapter_ref).await?;
        let extension_id = manifest.id.clone();

        let target_path = if self.storage.dir().is_some() {
            self.storage.copy_to_storage(path, &extension_id)?
        } else {
            path.to_path_buf()
        };

        let adapter = inner.adapters.get(&ext_type).context("Adapter not found")?;
        let adapter_ref = adapter.as_ref();

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

        inner.extensions.insert(extension_id.clone(), loaded_ext);
        let loaded_ref = inner.extensions.get(&extension_id).expect("just inserted");
        Self::register_skill_catalog(self.core.clone(), loaded_ref);
        Self::populate_source_from_storage(&self.storage, &mut inner.extensions, &extension_id);

        info!("Installed extension '{}' ({})", extension_id, ext_type_name);

        Ok(extension_id)
    }

    pub async fn uninstall(&self, id: &ExtensionId) -> Result<()> {
        let mut inner = self.inner.write().await;
        let loaded_ext = inner
            .extensions
            .remove(id)
            .context(format!("Extension '{id}' not found"))?;

        crate::extensions::framework::skill_catalog::SkillCatalog::global()
            .unregister_by_extension(id);

        for hook_id in &loaded_ext.hook_ids {
            if let Err(e) = self.core.unregister_hook(hook_id).await {
                warn!(
                    "Failed to unregister hook {} for extension {}: {}",
                    hook_id, id, e
                );
            }
        }

        if let Some(state) = inner.extension_states.remove(id) {
            if let Some(adapter) = inner.adapters.get(&loaded_ext.extension_type) {
                if let Err(e) = adapter.shutdown(state).await {
                    warn!("Error shutting down extension {}: {}", id, e);
                }
            }
        }

        if let Err(e) = self.storage.remove_from_storage(id) {
            warn!("Failed to remove extension from storage: {}", e);
        }

        info!("Uninstalled extension '{}'", id);

        Ok(())
    }

    fn register_skill_catalog(_core: Arc<ExtensionCore>, loaded: &LoadedExtension) {
        if loaded.extension_type != "skill" {
            return;
        }
        let Some(skill_file) = loaded
            .manifest
            .metadata
            .get("skill_file")
            .and_then(|v| v.as_str())
        else {
            return;
        };
        let name = loaded.manifest.id.0.clone();
        crate::extensions::framework::skill_catalog::SkillCatalog::global().register(
            name,
            PathBuf::from(skill_file),
            Some(loaded.manifest.id.clone()),
        );
    }

    pub async fn list_extensions(&self) -> Vec<LoadedExtension> {
        let inner = self.inner.read().await;
        inner.extensions.values().cloned().collect()
    }

    pub async fn get_extension(&self, id: &ExtensionId) -> Option<LoadedExtension> {
        let inner = self.inner.read().await;
        inner.extensions.get(id).cloned()
    }

    // PR-B (extension removal): `create_bundle` deleted — zero
    // production callers; `peko ext bundle` retired in Phase 5
    // (ADR-047 §2.1).
    //
    // PR-B: `install_bundle` deleted — same reason. The
    // `ExtensionBundle` / `BundleMetadata` types are gone from
    // `store_trait.rs` alongside.
    //
    // PR-B: the entire `Dependency Resolution` section deleted —
    // `resolve_dependencies_with_map`, `resolve_dependencies`, and
    // `resolve_dependencies_root`. Exercised only by the 6 dependency
    // tests (also deleted). No production call site exists for "is
    // this extension's dependency set satisfiable" — workspace-
    // resident tooling has no installation step.

    // ============================================================================
    // Discovery
    // ============================================================================

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

            if let Some(ext_type) = self.detect_extension_type_string(&path) {
                let (ext_type, manifest_path) = {
                    let inner = self.inner.read().await;
                    let adapter = inner.adapters.get(&ext_type);
                    match adapter {
                        Some(adapter) => {
                            let format = adapter.manifest_format();
                            let manifest_path = if let Some(p) = format.manifest_path(&path) {
                                p
                            } else {
                                let candidates =
                                    vec![path.join("manifest.yaml"), path.join("server.json")];
                                candidates
                                    .into_iter()
                                    .find(|p| p.exists())
                                    .unwrap_or_else(|| path.join("manifest.yaml"))
                            };
                            (adapter.extension_type().to_string(), manifest_path)
                        }
                        None => {
                            tracing::debug!(
                                "No adapter registered for detected type: {}",
                                ext_type
                            );
                            continue;
                        }
                    }
                };

                tracing::info!("Detected extension type '{}' in {}", ext_type, name);

                discovered.push(DiscoveredExtension {
                    path,
                    manifest_path,
                    extension_type: ext_type,
                });
            } else {
                tracing::debug!("No adapter detected extension type for: {}", name);
            }
        }

        Ok(discovered)
    }

    pub async fn load_from_directory(&self, path: &Path) -> Result<Vec<ExtensionId>> {
        tracing::info!("Scanning directory for extensions: {}", path.display());
        let discovered = self
            .scan_directory(path)
            .await
            .with_context(|| format!("Failed to scan directory: {}", path.display()))?;
        tracing::info!("Discovered {} extensions", discovered.len());
        let mut loaded_ids = Vec::new();

        for discovered_ext in discovered {
            tracing::info!(
                "Loading extension from {} (manifest: {}, type: {})",
                discovered_ext.path.display(),
                discovered_ext.manifest_path.display(),
                discovered_ext.extension_type
            );

            let manifest_content = tokio::fs::read_to_string(&discovered_ext.manifest_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to read manifest: {}",
                        discovered_ext.manifest_path.display()
                    )
                })?;

            let inner = self.inner.read().await;
            let adapter = inner
                .adapters
                .get(&discovered_ext.extension_type)
                .with_context(|| {
                    format!(
                        "Adapter not found for extension type: {}",
                        discovered_ext.extension_type
                    )
                })?;

            let manifest = adapter
                .parse_manifest(&discovered_ext.manifest_path, &manifest_content)
                .with_context(|| {
                    format!(
                        "Failed to parse manifest: {}",
                        discovered_ext.manifest_path.display()
                    )
                })?;
            drop(inner);

            if self.get_extension(&manifest.id).await.is_some() {
                debug!("Extension '{}' already loaded, skipping", manifest.id);
                continue;
            }

            match self.try_load_extension(&discovered_ext.path).await {
                Ok(id) => {
                    loaded_ids.push(id);
                }
                Err(e) => {
                    warn!(
                        "Failed to load extension from {:?}: {:#}",
                        discovered_ext.path, e
                    );
                }
            }
        }

        Ok(loaded_ids)
    }

    pub async fn resolve_tool_name(&self, name: &str) -> Option<ToolResolution> {
        if crate::principal::runtime::builtin_tools::is_builtin_tool(name)
            || name.starts_with("builtin:")
        {
            return None;
        }

        let inner = self.inner.read().await;
        for ext in inner.extensions.values() {
            if ext.manifest.name.eq_ignore_ascii_case(name) {
                return Some(ToolResolution {
                    id: ext.manifest.id.0.clone(),
                    registry_ref: ext.manifest.source.clone(),
                });
            }
        }
        None
    }

    pub async fn global_items(&self) -> Vec<GlobalExtensionItem> {
        let inner = self.inner.read().await;
        inner
            .extensions
            .values()
            .map(|ext| GlobalExtensionItem {
                id: ext.manifest.id.0.clone(),
                name: ext.manifest.name.clone(),
                ext_type: ext.extension_type.clone(),
                source: ext.manifest.source.clone(),
                provides: ext.manifest.provides.clone(),
                requires: ext.manifest.requires.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub async fn insert_test_extension(&self, loaded: LoadedExtension) {
        let mut inner = self.inner.write().await;
        inner.extensions.insert(loaded.manifest.id.clone(), loaded);
    }
}

impl Default for ExtensionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // PR-B: `ExtensionDependency` import dropped — the 6 dependency
    // resolution tests that used it were deleted alongside
    // `resolve_dependencies_with_map` and friends.
    use tempfile::TempDir;

    #[test]
    fn test_extension_store_creation() {
        let store = ExtensionStore::new();
        assert!(store.inner.blocking_read().extensions.is_empty());
    }

    // PR-B (extension removal): `test_extension_bundle` deleted —
    // exercised the now-removed `ExtensionBundle` / `BundleMetadata`
    // types.

    // ─── ADR-024: Two-tier detection hierarchy tests ─────────────────────

    #[test]
    fn test_detect_tier1_skill_md() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-skill");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("SKILL.md"), "---\nname: My Skill\n---\n").unwrap();

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
            Some("skill".to_string())
        );
    }

    #[test]
    fn test_detect_tier1_agent_md() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-agent");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("AGENT.md"), "---\nname: My Agent\n---\n").unwrap();

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
            Some("agent".to_string())
        );
    }

    #[test]
    fn test_detect_tier1_server_json() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("my-mcp");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("server.json"), r#"{"name": "test"}"#).unwrap();

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
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

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
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

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
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

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
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

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
            Some("custom:my-org/type".to_string())
        );
    }

    #[test]
    fn test_detect_tier1_skill_takes_precedence_over_tier2() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("hybrid");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("SKILL.md"), "---\nname: Skill\n---\n").unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: hybrid\nname: Hybrid\nextension_type: general\n",
        )
        .unwrap();

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
            Some("skill".to_string())
        );
    }

    #[test]
    fn test_detect_tier2_manifest_yaml_typed() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("hybrid2");
        std::fs::create_dir(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            "id: hybrid\nname: Hybrid\nextension_type: gateway\ngateway_type: http\n",
        )
        .unwrap();

        let store = ExtensionStore::new();
        assert_eq!(
            store.detect_extension_type_string(&ext_dir),
            Some("gateway".to_string())
        );
    }

    #[test]
    fn test_detect_nothing_found() {
        let temp = TempDir::new().unwrap();
        let ext_dir = temp.path().join("empty");
        std::fs::create_dir(&ext_dir).unwrap();

        let store = ExtensionStore::new();
        assert_eq!(store.detect_extension_type_string(&ext_dir), None);
    }

    // ─── Dependency Resolution Tests ─────────────────────────────────────
    //
    // PR-B (extension removal): the 6 tests below deleted — they
    // exercised `resolve_dependencies_with_map` /
    // `resolve_dependencies` / `resolve_dependencies_root`, which are
    // gone. `test_resolve_dependencies_no_deps`,
    // `test_resolve_dependencies_with_missing_required`,
    // `test_resolve_dependencies_with_missing_optional`,
    // `test_resolve_dependencies_satisfied`,
    // `test_resolve_dependencies_version_mismatch_informational`,
    // `test_resolve_dependencies_circular_detection` all gone.

    // ─── Tool Name Resolution Tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_resolve_tool_name_builtin_returns_none() {
        let store = ExtensionStore::new();
        assert!(store.resolve_tool_name("shell").await.is_none());
        assert!(store.resolve_tool_name("Read").await.is_none());
    }

    #[tokio::test]
    async fn test_resolve_tool_name_unknown_returns_none() {
        let store = ExtensionStore::new();
        assert!(store.resolve_tool_name("unknown-tool").await.is_none());
    }

    #[tokio::test]
    async fn test_resolve_tool_name_matches_extension() {
        let store = ExtensionStore::new();

        let mut manifest = ExtensionManifest::new(
            "calc-ext",
            "universal-tool",
            "Calculator",
            "A calculator tool",
            "1.0.0",
            PathBuf::from("/tmp/calc"),
        );
        manifest.source = Some("pekohub.com/extensions/calculator:latest".to_string());

        store
            .insert_test_extension(LoadedExtension {
                manifest,
                extension_type: "universal-tool".to_string(),
                hook_ids: vec![],
                path: PathBuf::from("/tmp/calc"),
            })
            .await;

        let resolution = store.resolve_tool_name("Calculator").await.unwrap();
        assert_eq!(resolution.id, "calc-ext");
        assert_eq!(
            resolution.registry_ref,
            Some("pekohub.com/extensions/calculator:latest".to_string())
        );
    }

    #[tokio::test]
    async fn test_resolve_tool_name_case_insensitive() {
        let store = ExtensionStore::new();

        let manifest = ExtensionManifest::new(
            "my-skill",
            "skill",
            "MySkill",
            "Desc",
            "1.0.0",
            PathBuf::from("/tmp/myskill"),
        );

        store
            .insert_test_extension(LoadedExtension {
                manifest,
                extension_type: "skill".to_string(),
                hook_ids: vec![],
                path: PathBuf::from("/tmp/myskill"),
            })
            .await;

        assert!(store.resolve_tool_name("myskill").await.is_some());
        assert!(store.resolve_tool_name("MYSKILL").await.is_some());
        assert!(store.resolve_tool_name("MySkill").await.is_some());
    }

    #[tokio::test]
    async fn test_resolve_tool_name_no_source() {
        let store = ExtensionStore::new();

        let manifest = ExtensionManifest::new(
            "local-ext",
            "general",
            "LocalExt",
            "Desc",
            "1.0.0",
            PathBuf::from("/tmp/local"),
        );

        store
            .insert_test_extension(LoadedExtension {
                manifest,
                extension_type: "general".to_string(),
                hook_ids: vec![],
                path: PathBuf::from("/tmp/local"),
            })
            .await;

        let resolution = store.resolve_tool_name("LocalExt").await.unwrap();
        assert_eq!(resolution.id, "local-ext");
        assert!(resolution.registry_ref.is_none());
    }
}
