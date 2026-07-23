//! Host-side adapters for the [`peko_auth::host`] trait ports.
//!
//! [`peko_auth::host`]: ../../crates/auth/src/host.rs
//!
//! `peko-auth` is a leaf crate. It defines three narrow trait
//! ports ([`RuntimePaths`], [`PrincipalResourceView`], plus the
//! lifted [`Exposure`] type) that abstract the root-only
//! `PathResolver` and `PrincipalConfig` types. The implementations
//! live here in root so the orphan rule is satisfied (one of
//! `{Trait, Type}` is local to each impl's crate — the trait lives
//! in `peko_auth`, the type lives in root).
//!
//! ## What this module is NOT
//!
//! This is not a re-export shim of `peko-auth` types. Callers in
//! root import `peko_auth::*` directly. This file only wires
//! `PathResolver` and `PrincipalConfig` into the trait ports and
//! exposes the `Arc<dyn ...>` convenience constructors.

use std::sync::Arc;

use peko_auth::host::{Exposure, PrincipalResourceView, RuntimePaths};

use crate::common::paths::PathResolver;
use crate::principal::config::PrincipalConfig;

// ---------------------------------------------------------------------------
// `RuntimePaths` impl for `PathResolver`
// ---------------------------------------------------------------------------

impl RuntimePaths for PathResolver {
    fn runtime_dir(&self) -> std::path::PathBuf {
        PathResolver::runtime_dir(self)
    }
}

// ---------------------------------------------------------------------------
// `PrincipalResourceView` impl for `PrincipalConfig`
// ---------------------------------------------------------------------------

/// `PrincipalConfig` exposes the four fields
/// `auth::ownership::principal_resource` needs to build an
/// `auth::Resource::Principal` value.
///
/// ## Why a trait port and not a direct function in principal
///
/// The original code had `auth::ownership::principal_resource(name,
/// &PrincipalConfig)` taking the principal concrete type. That
/// creates a `peko-auth ↔ peko-principal` cycle when both become
/// workspace crates. The trait port in `peko-auth::host` flips the
/// direction — auth declares the contract, principal implements it
/// in root.
///
/// Note: this is the *opposite* direction from the `peko-auth →
/// peko-principal` import that used to live in
/// `auth::ownership::Resource::Principal`'s `exposure` field. The
/// `Exposure` enum used to live in `crate::principal::config` and
/// was imported by auth. To break the cycle, `Exposure` was lifted
/// into `peko-auth` (its natural home as part of `Resource`), and
/// `PrincipalConfig.exposure` is now typed as `peko_auth::Exposure`
/// (re-imported via `use crate::auth_compat::Exposure;` if needed,
/// or accessed directly as `peko_auth::Exposure`).
impl PrincipalResourceView for PrincipalConfig {
    fn name(&self) -> &str {
        &self.name
    }

    fn owner(&self) -> &peko_auth::Subject {
        &self.owner
    }

    fn permissions(&self) -> &[peko_auth::PermissionGrant] {
        &self.permissions
    }

    fn exposure(&self) -> Exposure {
        self.exposure
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Build an `Arc<dyn RuntimePaths>` for the supplied resolver. The
/// `PathResolver` impl of `RuntimePaths` is in this file; this
/// helper hides the `as` coercion at the call site.
#[must_use]
pub fn runtime_paths_arc(resolver: &PathResolver) -> Arc<dyn RuntimePaths> {
    Arc::new(resolver.clone()) as Arc<dyn RuntimePaths>
}

/// Re-export the lifted `Exposure` enum from `peko_auth` so callers
/// in root that previously imported `peko_auth::Exposure`
/// can update to `peko_auth::Exposure` (or stay on the new path
/// `crate::auth_compat::Exposure` if they prefer).
pub use peko_auth::host::Exposure as AuthExposure;

#[cfg(test)]
mod tests {
    use super::*;
    use peko_auth::host::{PrincipalResourceView, RuntimePaths};
    use tempfile::TempDir;

    #[test]
    fn path_resolver_runtime_dir_implements_trait() {
        let dir = TempDir::new().unwrap();
        let resolver = PathResolver::with_dirs(
            dir.path().to_path_buf(),
            dir.path().join("data"),
            dir.path().join("cache"),
        );
        // smoke: the trait method returns the same as the inherent method.
        let from_trait = RuntimePaths::runtime_dir(&resolver);
        let from_inherent = PathResolver::runtime_dir(&resolver);
        assert_eq!(from_trait, from_inherent);
    }

    #[test]
    fn runtime_paths_arc_round_trip() {
        let dir = TempDir::new().unwrap();
        let resolver = PathResolver::with_dirs(
            dir.path().to_path_buf(),
            dir.path().join("data"),
            dir.path().join("cache"),
        );
        let arc = runtime_paths_arc(&resolver);
        assert_eq!(
            RuntimePaths::runtime_dir(arc.as_ref()),
            PathResolver::runtime_dir(&resolver)
        );
    }

    #[test]
    fn exposure_re_exports_correctly() {
        // The re-export points at the same type as peko_auth::host::Exposure.
        let _: AuthExposure = AuthExposure::Private;
        let _: peko_auth::host::Exposure = AuthExposure::Public;
    }
}
