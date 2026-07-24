//! Host-side adapters for the [`peko_auth::host`] trait ports.
//!
//! [`peko_auth::host`]: ../../crates/auth/src/host.rs
//!
//! `peko-auth` is a leaf crate. It defines two narrow trait
//! ports ([`RuntimePaths`], [`PrincipalResourceView`]) that
//! abstract the root-only `PathResolver` and the
//! `peko_principal::config::PrincipalConfig` type respectively.
//!
//! - `RuntimePaths` is implemented here in root for `PathResolver`
//!   (the type is local to root).
//! - `PrincipalResourceView` is implemented in `peko-principal`
//!   itself for `PrincipalConfig` (the type is local to that
//!   crate). See [`peko_principal::config::PrincipalConfig`] and the
//!   orphan rule: at least one of `{Trait, Type}` must be local to
//!   the impl's crate.
//!
//! This file only wires `PathResolver` into the trait port and
//! exposes the `Arc<dyn ...>` convenience constructors.

use std::sync::Arc;

use peko_auth::host::RuntimePaths;

use crate::common::paths::PathResolver;

// ---------------------------------------------------------------------------
// `RuntimePaths` impl for `PathResolver`
// ---------------------------------------------------------------------------

impl RuntimePaths for PathResolver {
    fn runtime_dir(&self) -> std::path::PathBuf {
        PathResolver::runtime_dir(self)
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

#[cfg(test)]
mod tests {
    use super::*;
    use peko_auth::host::RuntimePaths;
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
}