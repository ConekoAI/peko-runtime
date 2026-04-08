//! Extension Manager
//!
//! This module provides unified lifecycle management for extensions:
//! - Discovery from standard locations
//! - Installation and uninstallation
//! - Enable/disable control
//! - Bundling and packaging
//!
//! # Phase
//!
//! This module is implemented in **Phase 7** of the migration plan.
//! Currently it contains only placeholder types and traits.
//!
//! # Usage
//!
//! ```rust,ignore
//! use pekobot::extensions::manager::ExtensionManager;
//!
//! let mut manager = ExtensionManager::new();
//!
//! // Discover and load all extensions
//! manager.load_all().await?;
//!
//! // Install a new extension
//! manager.install(Path::new("./my-skill")).await?;
//!
//! // List extensions
//! for ext in manager.list_extensions() {
//!     println!("{}: {}", ext.id, ext.manifest.name);
//! }
//!
//! // Enable/disable
//! manager.disable(&ExtensionId::new("my-skill")).await?;
//! manager.enable(&ExtensionId::new("my-skill")).await?;
//!
//! // Uninstall
//! manager.uninstall(&ExtensionId::new("my-skill")).await?;
//! ```

use crate::extensions::adapters::ExtensionTypeAdapter;
use crate::extensions::types::{ExtensionId, ExtensionManifest};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extension Manager - Central management for all extensions
///
/// This struct provides the unified interface for managing extensions
/// regardless of their type (skill, MCP, tool, channel, etc.).
#[derive(Debug)]
pub struct ExtensionManager {
    /// Registered adapters by extension type
    adapters: HashMap<String, Box<dyn ExtensionTypeAdapter>>,

    /// Loaded extensions
    extensions: HashMap<ExtensionId, LoadedExtension>,

    /// Storage for extension metadata
    storage: ExtensionStorage,
}

/// An extension that has been loaded into the manager
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    /// Extension metadata
    pub manifest: ExtensionManifest,

    /// Extension type identifier
    pub extension_type: String,

    /// Hook registration IDs
    pub hook_ids: Vec<crate::extensions::HookId>,

    /// Whether the extension is enabled
    pub enabled: bool,

    /// Path to the extension
    pub path: PathBuf,
}

/// Storage backend for extension metadata
#[derive(Debug)]
pub struct ExtensionStorage {
    /// Root directory for extension storage
    storage_dir: Option<PathBuf>,
}

impl ExtensionStorage {
    /// Create new storage
    pub fn new() -> Self {
        Self { storage_dir: None }
    }

    /// Create new storage with specific directory
    pub fn with_dir(storage_dir: PathBuf) -> Self {
        Self {
            storage_dir: Some(storage_dir),
        }
    }

    /// Get the storage directory
    pub fn dir(&self) -> Option<&Path> {
        self.storage_dir.as_deref()
    }
}

impl Default for ExtensionStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a load operation
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Extensions that were loaded successfully
    pub loaded: Vec<ExtensionId>,

    /// Extensions that failed to load
    pub failed: Vec<(PathBuf, anyhow::Error)>,
}

/// Bundle of multiple extensions
#[derive(Debug)]
pub struct ExtensionBundle {
    /// Bundle name
    pub name: String,

    /// Extensions in the bundle
    pub extensions: Vec<ExtensionManifest>,

    /// Bundle metadata
    pub metadata: BundleMetadata,
}

/// Metadata for an extension bundle
#[derive(Debug, Default)]
pub struct BundleMetadata {
    /// Bundle version
    pub version: String,

    /// Bundle description
    pub description: String,

    /// Extension IDs that this bundle depends on
    pub dependencies: Vec<String>,

    /// Extension IDs that conflict with this bundle
    pub conflicts: Vec<String>,
}

impl ExtensionManager {
    /// Create a new extension manager
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            extensions: HashMap::new(),
            storage: ExtensionStorage::new(),
        }
    }

    /// Register an extension type adapter
    pub fn register_adapter(&mut self, adapter: Box<dyn ExtensionTypeAdapter>) {
        let ext_type = adapter.extension_type().to_string();
        self.adapters.insert(ext_type, adapter);
    }

    /// Discover and load all extensions from standard locations
    pub async fn load_all(&mut self) -> Result<LoadReport> {
        // Phase 7: Implementation
        // 1. Define standard discovery paths
        // 2. Scan each path for extension manifests
        // 3. Detect extension type for each
        // 4. Load using appropriate adapter
        Ok(LoadReport::default())
    }

    /// Install an extension from a path
    pub async fn install(&mut self, _path: &Path) -> Result<ExtensionId> {
        // Phase 7: Implementation
        // 1. Detect extension type
        // 2. Validate manifest
        // 3. Copy to storage
        // 4. Load using adapter
        // 5. Register hooks
        anyhow::bail!("Not implemented (Phase 7)")
    }

    /// Uninstall an extension
    pub async fn uninstall(&mut self, _id: &ExtensionId) -> Result<()> {
        // Phase 7: Implementation
        // 1. Unregister all hooks
        // 2. Shutdown if stateful
        // 3. Remove from storage
        anyhow::bail!("Not implemented (Phase 7)")
    }

    /// Enable an extension
    pub async fn enable(&mut self, _id: &ExtensionId) -> Result<()> {
        // Phase 7: Implementation
        // Enable all hook registrations for this extension
        anyhow::bail!("Not implemented (Phase 7)")
    }

    /// Disable an extension
    pub async fn disable(&mut self, _id: &ExtensionId) -> Result<()> {
        // Phase 7: Implementation
        // Disable all hook registrations for this extension
        anyhow::bail!("Not implemented (Phase 7)")
    }

    /// List all extensions
    pub fn list_extensions(&self) -> Vec<&LoadedExtension> {
        self.extensions.values().collect()
    }

    /// Get a specific extension
    pub fn get_extension(&self, id: &ExtensionId) -> Option<&LoadedExtension> {
        self.extensions.get(id)
    }

    /// Create a bundle from multiple extensions
    pub fn create_bundle(&self, _ids: Vec<ExtensionId>, _name: &str) -> Result<ExtensionBundle> {
        // Phase 7: Implementation
        anyhow::bail!("Not implemented (Phase 7)")
    }

    /// Install a bundle
    pub async fn install_bundle(&mut self, _bundle: ExtensionBundle) -> Result<Vec<ExtensionId>> {
        // Phase 7: Implementation
        anyhow::bail!("Not implemented (Phase 7)")
    }
}

impl Default for ExtensionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Standard discovery paths for extensions
pub mod discovery_paths {
    use std::path::PathBuf;

    /// User configuration directory
    pub fn user_config() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("pekobot/extensions"))
    }

    /// User data directory
    pub fn user_data() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("pekobot/extensions"))
    }

    /// Project-local extensions
    pub fn project_local() -> PathBuf {
        PathBuf::from(".pekobot/extensions")
    }

    /// System-wide extensions
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

    /// Get all standard discovery paths
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

        // Should include project-local
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
