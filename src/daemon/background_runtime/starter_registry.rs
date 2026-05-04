//! Extension Runtime Starter Registry
//!
//! Central registry that maps extension types to their `ExtensionRuntimeStarter`
//! implementations. The IPC server and daemon use this registry to start, stop,
//! and restart extension background runtimes without knowing the extension type.

use super::starter::{ExtensionRuntimeStarter, StarterContext};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Registry of extension runtime starters.
///
/// Each starter is registered by its extension type string (e.g., "gateway", "mcp").
/// When the daemon receives an `ext_start` IPC request, it looks up the extension's
/// manifest, finds the starter for that type, and delegates.
#[derive(Debug)]
pub struct ExtensionRuntimeStarterRegistry {
    starters: HashMap<String, Box<dyn ExtensionRuntimeStarter>>,
}

impl ExtensionRuntimeStarterRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            starters: HashMap::new(),
        }
    }

    /// Register a starter for an extension type.
    ///
    /// If a starter for this type already exists, it is replaced.
    pub fn register(&mut self, starter: Box<dyn ExtensionRuntimeStarter>) {
        let ext_type = starter.extension_type().to_string();
        info!("Registering runtime starter for extension type: {}", ext_type);
        self.starters.insert(ext_type, starter);
    }

    /// Start the background runtime for an extension.
    ///
    /// 1. Reads the extension manifest from `data_dir/extensions/{extension_id}/manifest.yaml`
    /// 2. Extracts the `extension_type` field
    /// 3. Looks up the registered starter for that type
    /// 4. Delegates to `starter.start()`
    pub async fn start(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        let manifest = Self::read_manifest(extension_id, &ctx.data_dir).await?;
        let ext_type = manifest
            .get("extension_type")
            .and_then(|v| v.as_str())
            .unwrap_or("general");

        let starter = self
            .starters
            .get(ext_type)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Extension '{}' has type '{}' which does not support background runtimes. \
                     Use 'pekobot ext enable' instead.",
                    extension_id,
                    ext_type
                )
            })?;

        info!(
            "Starting background runtime for extension '{}' (type: {})",
            extension_id, ext_type
        );

        starter.start(extension_id, ctx).await
    }

    /// Stop the background runtime for an extension.
    ///
    /// This is type-agnostic — it simply calls `BackgroundRuntimeManager::stop()`.
    pub async fn stop(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        ctx.background_runtime_manager.stop(extension_id).await
    }

    /// Restart the background runtime for an extension.
    ///
    /// If the runtime is currently managed, calls `BackgroundRuntimeManager::restart()`.
    /// If not (e.g., was stopped), falls back to `start()` which re-reads the manifest.
    pub async fn restart(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> anyhow::Result<()> {
        let runtime_exists = ctx
            .background_runtime_manager
            .get_state(extension_id)
            .await
            .is_some();

        if runtime_exists {
            ctx.background_runtime_manager
                .restart(extension_id)
                .await
        } else {
            self.start(extension_id, ctx).await
        }
    }

    /// Get the runtime state for an extension.
    pub async fn get_state(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> Option<super::supervisor::RuntimeState> {
        ctx.background_runtime_manager.get_state(extension_id).await
    }

    /// Auto-start all extensions of registered types that declare `auto_start: true`.
    ///
    /// Called during daemon initialization.
    pub async fn auto_start_all(&self, ctx: &StarterContext) -> Vec<String> {
        let mut started = Vec::new();

        for starter in self.starters.values() {
            match starter.auto_start(ctx).await {
                Ok(ids) => {
                    for id in &ids {
                        info!("Auto-started extension '{}'", id);
                    }
                    started.extend(ids);
                }
                Err(e) => {
                    warn!(
                        "Auto-start failed for type '{}': {}",
                        starter.extension_type(),
                        e
                    );
                }
            }
        }

        started
    }

    /// Read and parse an extension's manifest.
    ///
    /// Tries `manifest.yaml` first. For Tier 1 MCP servers that only have
    /// `server.json`, synthesizes a minimal manifest with `extension_type: "mcp"`.
    async fn read_manifest(
        extension_id: &str,
        data_dir: &std::path::Path,
    ) -> anyhow::Result<serde_yaml::Value> {
        let ext_dir = data_dir.join("extensions").join(extension_id);
        let manifest_path = ext_dir.join("manifest.yaml");

        if manifest_path.exists() {
            let content = tokio::fs::read_to_string(&manifest_path)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read manifest for '{}': {}", extension_id, e))?;

            let manifest: serde_yaml::Value = serde_yaml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse manifest for '{}': {}", extension_id, e))?;

            return Ok(manifest);
        }

        // Tier 1 MCP: server.json without manifest.yaml
        let server_json_path = ext_dir.join("server.json");
        if server_json_path.exists() {
            return Ok(serde_yaml::Value::Mapping(
                serde_yaml::mapping::Mapping::from_iter([(
                    serde_yaml::Value::String("extension_type".to_string()),
                    serde_yaml::Value::String("mcp".to_string()),
                )]),
            ));
        }

        anyhow::bail!(
            "Extension '{}' not found (no manifest at {})",
            extension_id,
            manifest_path.display()
        )
    }
}

impl Default for ExtensionRuntimeStarterRegistry {
    fn default() -> Self {
        Self::new()
    }
}
