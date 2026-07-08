//! Extension management service
//!
//! Provides registry push/pull operations for extensions, used by both the
//! CLI command layer and other services (e.g. agent pull auto-installs
//! declared extension dependencies).

use crate::common::paths::PathResolver;
use crate::common::types::extension::{
    ExtensionDependencyResult, ExtensionPullResult, ExtensionPushResult,
};
use crate::common::vault::Vault;
use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
use crate::extensions::framework::core::global_core;
use crate::extensions::framework::manager::packaging::ExtensionPackager;
use crate::extensions::framework::manager::{DependencyStatus, ExtensionManager};
use crate::extensions::framework::types::{ExtensionId, ExtensionManifest};
use crate::extensions::gateway::GatewayAdapter;
use crate::extensions::general::GeneralExtensionAdapter;
use crate::extensions::mcp::McpAdapter;
use crate::extensions::skill::SkillAdapter;
use crate::extensions::slash::SlashAdapter;
use crate::extensions::universal::UniversalToolAdapter;
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef, ResourceType};
use crate::registry::config::RegistryConfig;
use crate::registry::manifest::RegistryManifest;
use crate::registry::packaging::types::{compute_digest, ImageDigest, Layer, LayerType};
use crate::registry::AgentRegistry;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Service for extension registry operations
#[derive(Clone)]
pub struct ExtensionManagementService {
    resolver: PathResolver,
}

impl std::fmt::Debug for ExtensionManagementService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionManagementService")
            .field("resolver", &self.resolver)
            .finish_non_exhaustive()
    }
}

impl ExtensionManagementService {
    /// Create a new extension management service with the given path resolver
    #[must_use]
    pub fn new(resolver: PathResolver) -> Self {
        Self { resolver }
    }

    /// Create an `ExtensionManager` with all default adapters registered
    async fn create_manager_with_adapters(&self) -> Result<ExtensionManager> {
        let core = global_core().context("Global ExtensionCore not initialized")?;
        let mut manager = ExtensionManager::with_core(core.clone());
        manager = manager.with_storage_dir(self.extensions_dir());

        if let Err(e) =
            BuiltinToolAdapter::register_all(&core, &BuiltinToolRegistrarConfig::default()).await
        {
            tracing::warn!(
                "Failed to register built-in tools with ExtensionCore: {}",
                e
            );
        }

        manager.register_adapter(Box::new(SkillAdapter::new()));
        manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
        manager.register_adapter(Box::new(SlashAdapter::new()));
        manager.register_adapter(Box::new(UniversalToolAdapter::new()));
        manager.register_adapter(Box::new(GatewayAdapter::new(core.clone())));
        manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));

        manager.load_all().await?;

        Ok(manager)
    }

    /// Push an installed extension to a registry.
    pub async fn push_extension<F>(
        &self,
        id: &str,
        registry_ref: &str,
        cli_registry: Option<&str>,
        with_deps: bool,
        on_progress: F,
    ) -> Result<ExtensionPushResult>
    where
        F: FnMut(ProgressEvent) + 'static,
    {
        let manager = self.create_manager_with_adapters().await?;
        let ext_id = ExtensionId::new(id);

        let ext = manager
            .get_extension(&ext_id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?
            .clone();

        // Resolve dependencies if --with-deps
        let mut dep_ids = Vec::new();
        if with_deps {
            let resolution = manager.resolve_dependencies_root(&ext.manifest)?;
            if resolution.has_required_missing() {
                let missing: Vec<_> = resolution
                    .missing
                    .iter()
                    .filter(|m| matches!(m, DependencyStatus::Missing { required: true, .. }))
                    .map(|m| format!("{m:?}"))
                    .collect();
                anyhow::bail!(
                    "Cannot push with --with-deps: required dependencies are not installed: {}",
                    missing.join(", ")
                );
            }
            for dep in &resolution.satisfied {
                if let DependencyStatus::Satisfied { package, .. } = dep {
                    dep_ids.push(ExtensionId::new(package));
                }
            }
        }

        // Export to a temp .ext file
        let temp_dir = std::env::temp_dir().join("PEKO_ext_push");
        tokio::fs::create_dir_all(&temp_dir).await?;
        let temp_path = temp_dir.join(format!("{}.ext", ext.manifest.id.0));

        ExtensionPackager::export_with_deps(
            &manager,
            &ext_id,
            &dep_ids,
            temp_path.to_string_lossy().as_ref(),
        )?;

        // Read file bytes and compute digest
        let data = tokio::fs::read(&temp_path).await?;
        let layer_digest = compute_digest(&data);

        // Store as layer in AgentRegistry
        let registry = AgentRegistry::new(AgentRegistry::default_path());
        registry.init().await?;
        registry.store_layer(&layer_digest, &data).await?;

        // Build RegistryManifest with kind="extension", single layer.
        let mut manifest =
            RegistryManifest::new(ext.manifest.name.clone(), ext.manifest.version.clone())
                .with_kind("extension")
                .with_ref(registry_ref)
                .with_bundle_type("extension")
                .with_extension_type(&ext.extension_type)
                .with_description(&ext.manifest.description)
                .with_config(layer_digest.clone(), data.len() as u64, None::<String>);
        manifest.add_layer(Layer::new(
            layer_digest.clone(),
            LayerType::Config,
            data.len() as u64,
        ));

        // Compute manifest digest
        let manifest_json = manifest.to_json()?;
        let manifest_digest = ImageDigest::from_bytes(manifest_json.as_bytes());
        manifest.digest = manifest_digest.as_str().to_string();

        // Store manifest for RegistryClient
        self.store_registry_manifest_for_client(&registry, &manifest)
            .await?;

        // Parse registry ref and configure client
        let reg_ref = RegistryRef::parse_with_default(
            registry_ref,
            cli_registry.or(Some(&self.registry_config().default)),
            Some(ResourceType::Extension),
        )?;
        let config = self
            .resolve_registry_config(cli_registry, &reg_ref.host)
            .await?;

        let client = RegistryClient::new(config, registry);
        let resolved_ref = reg_ref.full_ref();

        let result = client
            .push(&manifest_digest, &resolved_ref, on_progress)
            .await?;
        let total_size = result.total_size_bytes();

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok(ExtensionPushResult {
            id: id.to_string(),
            registry_ref: resolved_ref,
            name: result.name,
            version: result.version,
            digest: result.digest,
            kind: result.kind,
            layers: result.layers.len(),
            total_size,
        })
    }

    /// Pull an extension from a registry and install it.
    ///
    /// Resolves and pulls transitive dependencies unless `no_deps` is true.
    pub async fn pull_extension<F>(
        &self,
        registry_ref: &str,
        cli_registry: Option<&str>,
        no_deps: bool,
        on_progress: F,
    ) -> Result<ExtensionPullResult>
    where
        F: FnMut(ProgressEvent) + 'static,
    {
        let mut manager = self.create_manager_with_adapters().await?;
        let mut already_pulled = HashSet::new();
        let on_progress: Box<dyn FnMut(ProgressEvent)> = Box::new(on_progress);
        self.pull_extension_inner(
            &mut manager,
            registry_ref,
            cli_registry,
            no_deps,
            on_progress,
            &mut already_pulled,
        )
        .await
    }

    async fn pull_extension_inner(
        &self,
        manager: &mut ExtensionManager,
        registry_ref: &str,
        cli_registry: Option<&str>,
        no_deps: bool,
        mut on_progress: Box<dyn FnMut(ProgressEvent)>,
        already_pulled: &mut HashSet<String>,
    ) -> Result<ExtensionPullResult> {
        // Prevent infinite recursion
        if !already_pulled.insert(registry_ref.to_string()) {
            return Err(anyhow::anyhow!(
                "Skipping {} (already pulled in this dependency tree)",
                registry_ref
            ));
        }

        let (temp_path, manifest) = self
            .pull_extension_to_temp(manager, registry_ref, cli_registry, &mut on_progress)
            .await?;

        // Install the main extension
        let install_result = self
            .install_pulled_extension(manager, registry_ref, &temp_path)
            .await;

        let ext_manifest = match install_result {
            Ok(m) => m,
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(e);
            }
        };

        // Resolve dependencies
        let dep_resolution = manager.resolve_dependencies_root(&ext_manifest)?;
        let mut dep_results = Vec::new();

        if !no_deps && !dep_resolution.missing.is_empty() {
            for dep in &dep_resolution.missing {
                if let DependencyStatus::Missing { package, .. } = dep {
                    let dep_progress: Box<dyn FnMut(ProgressEvent)> = Box::new(|_| {});
                    let result = Box::pin(self.pull_extension_inner(
                        manager,
                        package,
                        cli_registry,
                        false,
                        dep_progress,
                        already_pulled,
                    ))
                    .await;
                    dep_results.push(ExtensionDependencyResult {
                        registry_ref: package.clone(),
                        success: result.is_ok(),
                        error: result.err().map(|e| e.to_string()),
                    });
                }
            }
        }

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        Ok(ExtensionPullResult {
            registry_ref: registry_ref.to_string(),
            output_path: temp_path,
            manifest_name: manifest.name.clone(),
            manifest_version: manifest.version.clone(),
            manifest_digest: manifest.digest.clone(),
            manifest_kind: manifest.kind.clone(),
            manifest_layers: manifest.layers.len(),
            manifest_total_size: manifest.total_size_bytes(),
            dependencies: dep_results,
        })
    }

    async fn install_pulled_extension(
        &self,
        manager: &mut ExtensionManager,
        registry_ref: &str,
        temp_path: &Path,
    ) -> Result<ExtensionManifest> {
        // .ext packages are archives; the manager's type detection only works
        // on an extracted directory. Unpack to a temp dir before installing.
        let install_dir = if temp_path.extension().map_or(false, |e| e == "ext") {
            let temp_dir = std::env::temp_dir().join("PEKO_ext_install").join(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .to_string(),
            );
            tokio::fs::create_dir_all(&temp_dir).await?;
            crate::extensions::framework::manager::packaging::ExtensionUnpackager::install(
                temp_path, &temp_dir,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to extract .ext package '{}': {e}",
                    temp_path.display()
                )
            })?
        } else {
            temp_path.to_path_buf()
        };

        let install_result = manager.install(&install_dir).await;

        if let Err(ref e) = install_result {
            return Err(anyhow::anyhow!("{e}"));
        }

        let ext_id = install_result.unwrap();

        if manager.storage_dir().is_some() {
            let _ = manager.storage().write_source(&ext_id, registry_ref);
        }
        if let Some(loaded) = manager.get_extension_mut(&ext_id) {
            loaded.manifest.source = Some(registry_ref.to_string());
        }

        manager
            .get_extension(&ext_id)
            .map(|e| e.manifest.clone())
            .context("Installed extension not found in manager")
    }

    async fn pull_extension_to_temp(
        &self,
        _manager: &mut ExtensionManager,
        registry_ref: &str,
        cli_registry: Option<&str>,
        on_progress: &mut dyn FnMut(ProgressEvent),
    ) -> Result<(PathBuf, RegistryManifest)> {
        let agent_registry = AgentRegistry::new(AgentRegistry::default_path());
        agent_registry.init().await?;

        let reg_ref = RegistryRef::parse_with_default(
            registry_ref,
            cli_registry.or(Some(&self.registry_config().default)),
            Some(ResourceType::Extension),
        )?;
        let config = self
            .resolve_registry_config(cli_registry, &reg_ref.host)
            .await?;

        let client = RegistryClient::new(config, agent_registry.clone());
        let resolved_ref = reg_ref.full_ref();

        let manifest = client.pull(&resolved_ref, on_progress).await?;

        let layer = manifest
            .layers
            .first()
            .ok_or_else(|| anyhow::anyhow!("Manifest has no layers"))?;
        let data = agent_registry.get_layer(&layer.digest).await?;

        let temp_dir = std::env::temp_dir().join("PEKO_ext_pull");
        tokio::fs::create_dir_all(&temp_dir).await?;
        let temp_path = temp_dir.join(format!("{}.ext", manifest.name));
        tokio::fs::write(&temp_path, &data).await?;

        Ok((temp_path, manifest))
    }

    /// Store a `RegistryManifest` in the format expected by `RegistryClient`
    async fn store_registry_manifest_for_client(
        &self,
        registry: &AgentRegistry,
        manifest: &RegistryManifest,
    ) -> Result<ImageDigest> {
        let digest = ImageDigest::new(&manifest.digest)?;
        let image_dir = registry
            .root_path()
            .join("registry_manifests")
            .join(digest.dir_name());
        tokio::fs::create_dir_all(&image_dir).await?;
        let manifest_path = image_dir.join("manifest.json");
        let json = manifest.to_json()?;
        tokio::fs::write(&manifest_path, json).await?;
        Ok(digest)
    }

    /// Resolve registry configuration for push/pull operations
    async fn resolve_registry_config(
        &self,
        cli_registry: Option<&str>,
        host: &str,
    ) -> Result<RegistryConfig> {
        let config = crate::registry::config::load_from_config_dir(self.resolver.config_dir());
        let vault = Vault::load(self.resolver.vault())
            .with_context(|| "failed to load credential vault")?;
        let token = vault.get_registry_token().map(|t| t.token);
        crate::registry::config::resolve_registry_config(config, cli_registry, host, token)
    }

    fn registry_config(&self) -> RegistryConfig {
        crate::registry::config::load_from_config_dir(self.resolver.config_dir())
    }

    fn extensions_dir(&self) -> PathBuf {
        self.resolver.data_dir().join("extensions")
    }
}
