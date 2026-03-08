//! Gateway plugin registry
//!
//! The registry manages the lifecycle of gateway plugins:
//! - Downloading from Pekohub
//! - Caching locally
//! - Loading into memory
//! - Tracking installed plugins

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::gateway::config::{GatewayInfo, PluginManifest};
use crate::gateway::error::{GatewayError, GatewayResult};
use crate::gateway::interface::GatewayPlugin;
use crate::gateway::loader::{platform, PluginHandle, PluginLoader};
use crate::gateway::{RegistryError, RegistryResult};

/// Registry for gateway plugins
pub struct GatewayRegistry {
    /// Plugin loader
    loader: PluginLoader,
    /// Pekohub client for downloading
    pekohub_client: reqwest::Client,
    /// Pekohub base URL
    pekohub_url: String,
    /// Loaded plugins (name -> handle)
    loaded: RwLock<HashMap<String, PluginHandle>>,
    /// Active gateway instances
    instances: RwLock<HashMap<String, Box<dyn GatewayPlugin>>>,
    /// Cache directory
    cache_dir: PathBuf,
}

impl GatewayRegistry {
    /// Create a new registry
    pub fn new(cache_dir: impl Into<PathBuf>, pekohub_url: impl Into<String>) -> Self {
        let cache_dir = cache_dir.into();
        Self {
            loader: PluginLoader::new(cache_dir.clone()),
            pekohub_client: reqwest::Client::new(),
            pekohub_url: pekohub_url.into(),
            loaded: RwLock::new(HashMap::new()),
            instances: RwLock::new(HashMap::new()),
            cache_dir,
        }
    }

    /// Load a gateway plugin
    ///
    /// This will:
    /// 1. Check if already loaded
    /// 2. Check local cache
    /// 3. Download from Pekohub if needed
    /// 4. Load the plugin
    pub async fn load(&self, name: &str) -> RegistryResult<()> {
        // Check if already loaded
        {
            let loaded = self.loaded.read().await;
            if loaded.contains_key(name) {
                debug!("Gateway plugin '{}' already loaded", name);
                return Ok(());
            }
        }

        info!("Loading gateway plugin: {}", name);

        // Get manifest from Pekohub
        let manifest = self.fetch_manifest(name).await?;

        // Check if cached
        let cache_path = self.loader.get_cache_path(name, &manifest.plugin.version);

        if !self.loader.is_cached(name, &manifest.plugin.version) {
            // Download from Pekohub
            self.download_plugin(name, &manifest).await?;
        }

        // Load the plugin
        let handle = self.loader.load_from_path(&cache_path, manifest)?;

        // Store in registry
        {
            let mut loaded = self.loaded.write().await;
            loaded.insert(name.to_string(), handle);
        }

        info!("Gateway plugin '{}' loaded successfully", name);
        Ok(())
    }

    /// Load a plugin from a local path (for development)
    pub async fn load_from_path(
        &self,
        path: &Path,
        manifest: PluginManifest,
    ) -> RegistryResult<()> {
        let name = manifest.plugin.name.clone();

        // Check if already loaded
        {
            let loaded = self.loaded.read().await;
            if loaded.contains_key(&name) {
                return Err(RegistryError::AlreadyLoaded(name));
            }
        }

        // Load the plugin
        let handle = self.loader.load_from_path(path, manifest)?;

        // Store in registry
        {
            let mut loaded = self.loaded.write().await;
            loaded.insert(name, handle);
        }

        Ok(())
    }

    /// Unload a plugin
    pub async fn unload(&self, name: &str) -> RegistryResult<()> {
        info!("Unloading gateway plugin: {}", name);

        // Check if any instances are running
        {
            let instances = self.instances.read().await;
            if instances.contains_key(name) {
                return Err(RegistryError::CacheError {
                    name: name.to_string(),
                    message: "Cannot unload: active instances exist".to_string(),
                });
            }
        }

        // Remove from loaded
        {
            let mut loaded = self.loaded.write().await;
            if loaded.remove(name).is_none() {
                return Err(RegistryError::NotFound(name.to_string()));
            }
        }

        info!("Gateway plugin '{}' unloaded", name);
        Ok(())
    }

    /// Create a gateway instance
    pub async fn create_instance(
        &self,
        name: &str,
        config: HashMap<String, Value>,
    ) -> GatewayResult<Box<dyn GatewayPlugin>> {
        // Get the factory
        let factory = {
            let loaded = self.loaded.read().await;
            let handle = loaded
                .get(name)
                .ok_or_else(|| GatewayError::PluginNotFound(name.to_string()))?;

            // Create instance from factory
            handle.factory().create()
        };

        // Initialize with config
        let mut instance = factory;
        instance.initialize(config).await?;

        Ok(instance)
    }

    /// Store an active instance
    pub async fn register_instance(&self, instance_id: String, instance: Box<dyn GatewayPlugin>) {
        let mut instances = self.instances.write().await;
        instances.insert(instance_id, instance);
    }

    /// Remove an instance
    pub async fn unregister_instance(&self, instance_id: &str) -> Option<Box<dyn GatewayPlugin>> {
        let mut instances = self.instances.write().await;
        instances.remove(instance_id)
    }

    /// Get metadata for a loaded plugin
    pub async fn get_metadata(&self, name: &str) -> Option<GatewayInfo> {
        let loaded = self.loaded.read().await;
        loaded.get(name).map(|handle| {
            let manifest = handle.manifest();
            GatewayInfo {
                name: manifest.plugin.name.clone(),
                display_name: manifest.plugin.name.clone(),
                version: manifest.plugin.version.clone(),
                description: manifest.plugin.description.clone(),
                author: manifest.plugin.author.clone(),
                installed: true,
                latest_version: None,
            }
        })
    }

    /// List all loaded plugins
    pub async fn list_loaded(&self) -> Vec<GatewayInfo> {
        let loaded = self.loaded.read().await;
        loaded
            .values()
            .map(|handle| {
                let manifest = handle.manifest();
                GatewayInfo {
                    name: manifest.plugin.name.clone(),
                    display_name: manifest.plugin.name.clone(),
                    version: manifest.plugin.version.clone(),
                    description: manifest.plugin.description.clone(),
                    author: manifest.plugin.author.clone(),
                    installed: true,
                    latest_version: None,
                }
            })
            .collect()
    }

    /// List available plugins from Pekohub
    pub async fn list_available(&self) -> RegistryResult<Vec<GatewayInfo>> {
        let url = format!("{}/api/v1/gateways", self.pekohub_url);

        let response = self
            .pekohub_client
            .get(&url)
            .send()
            .await
            .map_err(|e| RegistryError::Pekohub(e.to_string()))?;

        if !response.status().is_success() {
            return Err(RegistryError::Pekohub(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let gateways: Vec<GatewayInfo> = response
            .json()
            .await
            .map_err(|e| RegistryError::Pekohub(e.to_string()))?;

        Ok(gateways)
    }

    /// Fetch manifest from Pekohub
    async fn fetch_manifest(&self, name: &str) -> RegistryResult<PluginManifest> {
        let url = format!("{}/api/v1/gateways/{}/manifest", self.pekohub_url, name);

        debug!("Fetching manifest from: {}", url);

        let response = self.pekohub_client.get(&url).send().await.map_err(|e| {
            RegistryError::DownloadFailed {
                name: name.to_string(),
                source: Box::new(e),
            }
        })?;

        if response.status().is_success() {
            let manifest = response
                .json()
                .await
                .map_err(|e| RegistryError::InvalidManifest {
                    name: name.to_string(),
                    message: e.to_string(),
                })?;
            Ok(manifest)
        } else if response.status().as_u16() == 404 {
            Err(RegistryError::NotFound(name.to_string()))
        } else {
            Err(RegistryError::Pekohub(format!(
                "HTTP {}",
                response.status()
            )))
        }
    }

    /// Download plugin from Pekohub
    async fn download_plugin(&self, name: &str, manifest: &PluginManifest) -> RegistryResult<()> {
        info!("Downloading gateway plugin: {}", name);

        // Determine platform
        let platform = platform::current();
        let platform_key = platform.replace('-', "_");

        // Get download URL from manifest
        let url = match platform_key.as_str() {
            "linux_x86_64" => &manifest.binary.linux_x64,
            "linux_aarch64" => manifest.binary.linux_arm64.as_ref().ok_or_else(|| {
                RegistryError::UnsupportedPlatform {
                    name: name.to_string(),
                    platform: platform.clone(),
                }
            })?,
            "macos_x86_64" => manifest.binary.macos_x64.as_ref().ok_or_else(|| {
                RegistryError::UnsupportedPlatform {
                    name: name.to_string(),
                    platform: platform.clone(),
                }
            })?,
            "macos_aarch64" => manifest.binary.macos_arm.as_ref().ok_or_else(|| {
                RegistryError::UnsupportedPlatform {
                    name: name.to_string(),
                    platform: platform.clone(),
                }
            })?,
            "windows_x86_64" => manifest.binary.windows_x64.as_ref().ok_or_else(|| {
                RegistryError::UnsupportedPlatform {
                    name: name.to_string(),
                    platform: platform.clone(),
                }
            })?,
            _ => {
                return Err(RegistryError::UnsupportedPlatform {
                    name: name.to_string(),
                    platform: platform.clone(),
                })
            }
        };

        // Download the plugin
        debug!("Downloading from: {}", url);

        let response = self.pekohub_client.get(url).send().await.map_err(|e| {
            RegistryError::DownloadFailed {
                name: name.to_string(),
                source: Box::new(e),
            }
        })?;

        if !response.status().is_success() {
            return Err(RegistryError::DownloadFailed {
                name: name.to_string(),
                source: Box::new(std::io::Error::other(format!("HTTP {}", response.status()))),
            });
        }

        // Ensure cache directory exists
        fs::create_dir_all(&self.cache_dir)
            .await
            .map_err(|e| RegistryError::CacheError {
                name: name.to_string(),
                message: e.to_string(),
            })?;

        // Save to cache
        let cache_path = self.loader.get_cache_path(name, &manifest.plugin.version);

        let bytes = response
            .bytes()
            .await
            .map_err(|e| RegistryError::DownloadFailed {
                name: name.to_string(),
                source: Box::new(e),
            })?;

        fs::write(&cache_path, bytes)
            .await
            .map_err(|e| RegistryError::CacheError {
                name: name.to_string(),
                message: e.to_string(),
            })?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&cache_path)
                .await
                .map_err(|e| RegistryError::CacheError {
                    name: name.to_string(),
                    message: e.to_string(),
                })?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&cache_path, perms).await.map_err(|e| {
                RegistryError::CacheError {
                    name: name.to_string(),
                    message: e.to_string(),
                }
            })?;
        }

        info!(
            "Downloaded gateway plugin '{}' to {}",
            name,
            cache_path.display()
        );

        Ok(())
    }

    /// Update a plugin to the latest version
    pub async fn update(&self, name: &str) -> RegistryResult<bool> {
        info!("Checking for updates: {}", name);

        // Get current version
        let current_version = {
            let loaded = self.loaded.read().await;
            loaded
                .get(name)
                .map(|h| h.manifest().plugin.version.clone())
        };

        // Fetch latest manifest
        let manifest = self.fetch_manifest(name).await?;
        let latest_version = &manifest.plugin.version;

        // Check if update needed
        if let Some(ref current) = current_version {
            if current == latest_version {
                debug!("Gateway plugin '{}' is up to date", name);
                return Ok(false);
            }

            // Unload current version
            self.unload(name).await?;
        }

        // Download and load new version
        self.download_plugin(name, &manifest).await?;
        self.load(name).await?;

        info!(
            "Updated gateway plugin '{}' from {} to {}",
            name,
            current_version.unwrap_or_else(|| "none".to_string()),
            latest_version
        );

        Ok(true)
    }

    /// Get the cache directory
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_registry_creation() {
        let temp_dir = TempDir::new().unwrap();
        let registry = GatewayRegistry::new(temp_dir.path(), "https://tools.coneko.ai");

        let loaded = registry.list_loaded().await;
        assert!(loaded.is_empty());
    }
}
