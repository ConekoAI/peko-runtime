//! `ExtensionStore` trait port + data types.
//!
//! Phase 8c.1.D.2: lift the trait contract + the data types the trait
//! methods return into the host crate so the framework's extension
//! lifecycle surface can be referenced through a trait object rather
//! than a concrete root-only type. Patterned on [`VaultAccess`] (see
//! `crates/extension-host/src/vault.rs`).
//!
//! Root's concrete [`ExtensionStore`] (in
//! `src/extensions/framework/store.rs`) keeps the full impl and adds
//! a blanket impl of this trait. Method coverage grows incrementally:
//! v1 only declares the methods the packaging layer needs
//! (`get_extension`, `resolve_tool_name`, `install`); future phases
//! will add more as the framework is split further.
//!
//! [`ExtensionStore`]: crate::extensions::framework::store::ExtensionStore (root)
//! [`VaultAccess`]: crate::vault::VaultAccess

use anyhow::Result;
use peko_extension_api::{ExtensionId, ExtensionManifest, HookId};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Trait port for the runtime-wide extension store.
///
/// Implementors MUST be `Send + Sync` so the trait object can flow
/// across `.await` points (e.g., inside packaging, IPC handlers,
/// registry flows). Mirrors the [`VaultAccess`] trait's bound.
///
/// [`VaultAccess`]: crate::vault::VaultAccess
#[async_trait::async_trait]
pub trait ExtensionStore: Send + Sync {
    /// Look up a loaded extension by its [`ExtensionId`]. Returns
    /// `None` if no extension with that id is currently registered.
    async fn get_extension(&self, id: &ExtensionId) -> Option<LoadedExtension>;

    /// Resolve a tool name (as it appears in the LLM catalog) back to
    /// the extension that owns it. Returns `None` if the tool name is
    /// not known to this store.
    async fn resolve_tool_name(&self, name: &str) -> Option<ToolResolution>;

    /// Install an extension from a directory path. Returns the
    /// newly-assigned [`ExtensionId`] on success.
    async fn install(&self, path: &Path) -> Result<ExtensionId>;
}

/// Blanket impl so `&Arc<T>` can coerce to `&dyn ExtensionStore`. The
/// trait lives in this crate, so this orphan-rule-safe impl stays here
/// rather than at the call site. Root's `ExtensionStore` (concrete
/// struct) gets the trait via its own blanket impl in
/// `src/extensions/framework/store.rs`; this blanket makes the
/// pointer indirection free for all such impls.
#[async_trait::async_trait]
impl<T: ExtensionStore + ?Sized> ExtensionStore for Arc<T> {
    async fn get_extension(&self, id: &ExtensionId) -> Option<LoadedExtension> {
        (**self).get_extension(id).await
    }

    async fn resolve_tool_name(&self, name: &str) -> Option<ToolResolution> {
        (**self).resolve_tool_name(name).await
    }

    async fn install(&self, path: &Path) -> Result<ExtensionId> {
        (**self).install(path).await
    }
}

/// An extension that has been loaded into the store.
///
/// Note: Enable/disable state is NOT stored here. It is managed by the
/// Principal's `capabilities` grant set. The store handles loading and
/// lifecycle; access control is determined by configuration.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub manifest: ExtensionManifest,
    pub extension_type: String,
    pub hook_ids: Vec<HookId>,
    pub path: PathBuf,
}

/// Result of resolving a tool name to an extension.
#[derive(Debug, Clone)]
pub struct ToolResolution {
    pub id: String,
    pub registry_ref: Option<String>,
}

/// Plain data snapshot of a globally loaded extension, used by the Principal
/// layer to build a per-Principal view without holding a reference to the store.
#[derive(Debug, Clone)]
pub struct GlobalExtensionItem {
    pub id: String,
    pub name: String,
    pub ext_type: String,
    pub source: Option<String>,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
}

/// Result of a load operation.
#[derive(Debug, Default)]
pub struct LoadReport {
    pub loaded: Vec<ExtensionId>,
    pub failed: Vec<(PathBuf, anyhow::Error)>,
}

/// Bundle of multiple extensions.
#[derive(Debug, Clone)]
pub struct ExtensionBundle {
    pub name: String,
    pub extensions: Vec<ExtensionManifest>,
    pub metadata: BundleMetadata,
}

/// Metadata for an extension bundle.
#[derive(Debug, Default, Clone)]
pub struct BundleMetadata {
    pub version: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub conflicts: Vec<String>,
}

/// Status of a single dependency after resolution.
#[derive(Debug, Clone)]
pub enum DependencyStatus {
    /// Already installed and version satisfies constraint.
    Satisfied {
        package: String,
        installed_version: String,
    },
    /// Not installed, needs pull.
    Missing { package: String, required: bool },
    /// Installed but version doesn't satisfy constraint (informational only for v1).
    VersionMismatch {
        package: String,
        have: String,
        need: Option<String>,
    },
}

/// Result of resolving dependencies for an extension.
#[derive(Debug, Clone, Default)]
pub struct DependencyResolution {
    /// Dependencies that are already satisfied.
    pub satisfied: Vec<DependencyStatus>,
    /// Dependencies that need to be pulled.
    pub missing: Vec<DependencyStatus>,
    /// Dependencies with version mismatches (informational).
    pub version_mismatches: Vec<DependencyStatus>,
    /// Circular dependency chains detected (if any).
    pub circular: Vec<Vec<String>>,
}

impl DependencyResolution {
    /// Check if there are any required missing dependencies.
    #[must_use]
    pub fn has_required_missing(&self) -> bool {
        self.missing
            .iter()
            .any(|m| matches!(m, DependencyStatus::Missing { required: true, .. }))
    }

    /// Get only the optional missing dependencies.
    #[must_use]
    pub fn optional_missing(&self) -> Vec<&DependencyStatus> {
        self.missing
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    DependencyStatus::Missing {
                        required: false,
                        ..
                    }
                )
            })
            .collect()
    }
}
