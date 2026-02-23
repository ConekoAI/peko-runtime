//! Gateway plugin loader
//!
//! This module handles dynamic loading of gateway plugins from `.gateway` files.
//! It uses `libloading` for cross-platform dynamic library loading.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use tracing::{debug, error, info, warn};

use crate::gateway::config::PluginManifest;
use crate::gateway::error::{GatewayError, GatewayResult, RegistryError, RegistryResult};
use crate::gateway::interface::{GatewayFactory, GATEWAY_API_VERSION};

/// Handle to a loaded gateway plugin
pub struct PluginHandle {
    /// The loaded dynamic library
    #[allow(dead_code)]
    library: Library,
    /// The factory instance
    factory: Box<dyn GatewayFactory>,
    /// Path to the plugin file
    path: PathBuf,
    /// Manifest metadata
    manifest: PluginManifest,
}

impl PluginHandle {
    /// Get the factory
    pub fn factory(&self) -> &dyn GatewayFactory {
        self.factory.as_ref()
    }

    /// Get the manifest
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Get the plugin path
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Loader for gateway plugins
pub struct PluginLoader {
    /// Cache directory for downloaded plugins
    cache_dir: PathBuf,
}

impl PluginLoader {
    /// Create a new plugin loader
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    /// Load a plugin from a path
    ///
    /// # Safety
    /// This uses `libloading` which is unsafe because we're loading arbitrary code.
    /// We trust plugins from Pekohub (verified by signatures).
    pub fn load_from_path(&self,
        path: &Path,
        manifest: PluginManifest,
    ) -> RegistryResult<PluginHandle> {
        info!("Loading gateway plugin from: {}", path.display());

        // Check file exists
        if !path.exists() {
            return Err(RegistryError::NotFound(
                path.display().to_string()
            ));
        }

        // Load the dynamic library
        // SAFETY: We assume plugins from Pekohub are trustworthy
        let library = unsafe {
            Library::new(path).map_err(|e| RegistryError::CacheError {
                name: manifest.plugin.name.clone(),
                message: format!("Failed to load library: {}", e),
            })?
        };

        // Get the factory creation function
        type CreateFactoryFn = unsafe fn() -> *mut dyn GatewayFactory;
        
        let create_factory: Symbol<CreateFactoryFn> = unsafe {
            library
                .get(b"create_gateway_factory")
                .map_err(|e| RegistryError::InvalidManifest {
                    name: manifest.plugin.name.clone(),
                    message: format!("Missing create_gateway_factory symbol: {}", e),
                })?
        };

        // Create the factory
        // SAFETY: The symbol is valid as long as library is loaded
        let factory = unsafe {
            let raw = create_factory();
            if raw.is_null() {
                return Err(RegistryError::CacheError {
                    name: manifest.plugin.name.clone(),
                    message: "Factory creation returned null".to_string(),
                });
            }
            Box::from_raw(raw)
        };

        // Verify API version compatibility
        let api_version = factory.api_version();
        if api_version != GATEWAY_API_VERSION {
            return Err(RegistryError::InvalidManifest {
                name: manifest.plugin.name.clone(),
                message: format!(
                    "API version mismatch: expected {}, got {}",
                    GATEWAY_API_VERSION, api_version
                ),
            });
        }

        info!(
            "Successfully loaded gateway plugin: {} v{}",
            manifest.plugin.name, manifest.plugin.version
        );

        Ok(PluginHandle {
            library,
            factory,
            path: path.to_path_buf(),
            manifest,
        })
    }

    /// Get the cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Get the path where a plugin would be cached
    pub fn get_cache_path(&self,
        name: &str,
        version: &str,
    ) -> PathBuf {
        let filename = format!(
            "{}_{}_{}.gateway{}",
            name,
            version,
            std::env::consts::OS,
            Self::get_platform_extension()
        );
        self.cache_dir.join(filename)
    }

    /// Check if a plugin is cached
    pub fn is_cached(&self,
        name: &str,
        version: &str,
    ) -> bool {
        self.get_cache_path(name, version).exists()
    }

    /// Get the platform-specific extension for dynamic libraries
    fn get_platform_extension() -> &'static str {
        if cfg!(target_os = "windows") {
            ".dll"
        } else if cfg!(target_os = "macos") {
            ".dylib"
        } else {
            ".so"
        }
    }
}

impl Drop for PluginLoader {
    fn drop(&mut self) {
        // Plugin handles must be dropped before the loader
        // (they hold references to loaded libraries)
        debug!("PluginLoader dropped");
    }
}

/// Platform detection utilities
pub mod platform {
    /// Get the current platform identifier
    pub fn current() -> String {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        format!("{}_{}", os, arch)
    }

    /// Check if a platform is supported
    pub fn is_supported(platform: &str) -> bool {
        matches!(
            platform,
            "linux_x86_64"
                | "linux_aarch64"
                | "macos_x86_64"
                | "macos_aarch64"
                | "windows_x86_64"
        )
    }

    /// Map Rust target triple to Pekohub platform string
    pub fn map_target(target: &str) -> Option<&'static str> {
        match target {
            "x86_64-unknown-linux-gnu" => Some("linux_x64"),
            "aarch64-unknown-linux-gnu" => Some("linux_arm64"),
            "x86_64-apple-darwin" => Some("macos_x64"),
            "aarch64-apple-darwin" => Some("macos_arm"),
            "x86_64-pc-windows-msvc" => Some("windows_x64"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = platform::current();
        assert!(!platform.is_empty());
        println!("Current platform: {}", platform);
    }

    #[test]
    fn test_cache_path_generation() {
        let loader = PluginLoader::new("/tmp/cache");
        let path = loader.get_cache_path("discord", "1.0.0");
        
        assert!(path.to_string_lossy().contains("discord"));
        assert!(path.to_string_lossy().contains("1.0.0"));
    }
}
