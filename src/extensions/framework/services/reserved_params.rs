//! Re-export shim (Phase 8c.1.D.1 + D.3 reserved_params portion).
//!
//! Implementation lives in `peko_extension_host::services::reserved_params`,
//! which already adopts the `&dyn VaultAccess` trait object form (the
//! host owns the trait; root's `Vault` impls `VaultAccess` at
//! `src/common/vault.rs:2274`). This root-side file is kept so the
//! historical `crate::extensions::framework::services::reserved_params::*`
//! import paths keep compiling until the framework shim tree is fully
//! deleted in Phase 8c.2.

pub use peko_extension_host::services::reserved_params::*;
