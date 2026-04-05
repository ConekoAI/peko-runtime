//! Capability Framework
//!
//! Unified capability management consolidating built-in tools, MCP servers,
//! Universal Tools, and downloaded tools into a single system.
//!
//! # Architecture
//!
//! - **CapabilityCatalog** (trait): Read-only registry access - discovery, metadata, search
//! - **CapabilityLifecycle** (trait): Runtime management - start/stop/status of processes
//! - **CapabilityManager**: Coordinates Catalog + Lifecycle
//!
//! # Capability Types
//!
//! - **BuiltIn**: Compiled capabilities (shell, read_file, glob, etc.)
//! - **Mcp**: MCP server capabilities
//! - **Universal**: Universal Tool capabilities (local executables)
//! - **Downloaded**: Downloaded from Pekohub registry
//!
//! # Example
//!
//! ```ignore
//! let catalog = Arc::new(CapabilityCatalogImpl::new(paths));
//! let lifecycle = Arc::new(CapabilityLifecycleImpl::uninitialized());
//! let manager = CapabilityManager::new(catalog, lifecycle);
//!
//! let caps = manager.list_capabilities().await;
//! ```

pub mod types;
pub use types::*;

pub mod builtin;

pub mod commands;

mod catalog;
pub use catalog::CapabilityCatalogImpl;

mod lifecycle;
pub use lifecycle::CapabilityLifecycleImpl;

use crate::common::paths::PathResolver;
use async_trait::async_trait;
use std::sync::Arc;

/// Read-only registry access for capability discovery and metadata
///
/// Implementors provide read-only access to the capability registry,
/// aggregating capabilities from all sources (built-in, MCP, Universal, downloaded).
#[async_trait]
pub trait CapabilityCatalog: Send + Sync {
    /// List all installed capabilities across all sources
    async fn list_installed(&self) -> Vec<CapabilityInfo>;

    /// Get capability info by name
    async fn get(&self, name: &str) -> Option<CapabilityInfo>;

    /// Search remote registry for capabilities
    async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<CapabilitySearchResult>>;

    /// List available capabilities from remote registry
    async fn list_available(&self) -> anyhow::Result<Vec<CapabilitySearchResult>>;

    /// Get capabilities by type
    async fn list_by_type(&self, cap_type: CapabilityType) -> Vec<CapabilityInfo> {
        self.list_installed()
            .await
            .into_iter()
            .filter(|c| c.cap_type == cap_type)
            .collect()
    }
}

/// Runtime lifecycle management for capabilities
///
/// Implementors handle starting, stopping, and status checking
/// of running capability processes (MCP servers, Universal Tool subprocesses).
#[async_trait]
pub trait CapabilityLifecycle: Send + Sync {
    /// Start a capability by name
    async fn start(&self, name: &str) -> anyhow::Result<()>;

    /// Stop a capability by name
    async fn stop(&self, name: &str) -> anyhow::Result<()>;

    /// Restart a capability by name
    async fn restart(&self, name: &str) -> anyhow::Result<()>;

    /// Get status of a specific capability
    async fn status(&self, name: &str) -> anyhow::Result<CapabilityStatus>;

    /// List all capabilities with their current status
    async fn list_with_status(&self) -> Vec<CapabilityWithStatus>;
}

/// Coordinates Catalog and Lifecycle for unified capability management
pub struct CapabilityManager {
    catalog: Arc<dyn CapabilityCatalog>,
    lifecycle: Arc<dyn CapabilityLifecycle>,
}

impl CapabilityManager {
    /// Create a new CapabilityManager
    pub fn new(catalog: Arc<dyn CapabilityCatalog>, lifecycle: Arc<dyn CapabilityLifecycle>) -> Self {
        Self { catalog, lifecycle }
    }

    /// List all installed capabilities
    pub async fn list_capabilities(&self) -> Vec<CapabilityInfo> {
        self.catalog.list_installed().await
    }

    /// List installed capabilities of a specific type
    pub async fn list_by_type(&self, cap_type: CapabilityType) -> Vec<CapabilityInfo> {
        self.catalog.list_by_type(cap_type).await
    }

    /// Get capability info by name
    pub async fn get(&self, name: &str) -> Option<CapabilityInfo> {
        self.catalog.get(name).await
    }

    /// Search remote registry
    pub async fn search_registry(&self, query: &str) -> anyhow::Result<Vec<CapabilitySearchResult>> {
        self.catalog.search_registry(query).await
    }

    /// List available capabilities from remote registry
    pub async fn list_available(&self) -> anyhow::Result<Vec<CapabilitySearchResult>> {
        self.catalog.list_available().await
    }

    /// Start a capability
    pub async fn start(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.start(name).await
    }

    /// Stop a capability
    pub async fn stop(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.stop(name).await
    }

    /// Restart a capability
    pub async fn restart(&self, name: &str) -> anyhow::Result<()> {
        self.lifecycle.restart(name).await
    }

    /// Get capability status
    pub async fn status(&self, name: &str) -> anyhow::Result<CapabilityStatus> {
        self.lifecycle.status(name).await
    }

    /// List capabilities with runtime status
    pub async fn list_with_status(&self) -> Vec<CapabilityWithStatus> {
        self.lifecycle.list_with_status().await
    }
}

/// Builder for CapabilityManager with common path configurations
impl CapabilityManager {
    /// Create a CapabilityManager with default implementations
    pub fn with_defaults(path_resolver: PathResolver) -> Self {
        let catalog = Arc::new(CapabilityCatalogImpl::new(path_resolver.clone()));
        let lifecycle = Arc::new(CapabilityLifecycleImpl::uninitialized());
        Self::new(catalog, lifecycle)
    }
}
