//! Host-side adapters for the [`peko_identity::host`] trait ports.
//!
//! [`peko_identity::host`]: ../../crates/identity/src/host.rs
//!
//! `peko-identity` is a leaf crate. It defines three narrow trait
//! ports ([`RuntimePaths`], [`IdentityDataDir`], [`IdentityVault`]) that
//! abstract the root-only `PathResolver` and `Vault` types. The
//! implementations live here in root so the orphan rule is satisfied
//! (one of `{Trait, Type}` is local to each impl's crate — the trait
//! lives in `peko_identity`, the type lives in root).
//!
//! ## What this module is NOT
//!
//! This is not a re-export shim of `peko-identity` types. Callers in
//! root import `peko_identity::*` directly. This file only wires
//! `PathResolver` and `Vault` into the trait ports.
//!
//! ## Why the trait ports exist
//!
//! Pre-Phase-3, `peko-identity` was a root module that imported
//! `crate::common::vault::Vault` and `crate::common::paths::PathResolver`
//! directly. That made it impossible to extract into a workspace
//! crate: a workspace member cannot import from root. The trait ports
//! flip the direction — `peko-identity` declares the contracts, root
//! implements them.

use std::sync::Arc;

use peko_identity::host::{
    identity_credential_from_raw, IdentityCredential, IdentityDataDir, IdentityVault, RuntimePaths,
};

use crate::common::paths::PathResolver;
use crate::common::vault::{CredentialFilter, CredentialKind, Vault};

// ---------------------------------------------------------------------------
// `RuntimePaths` impl for `PathResolver`
// ---------------------------------------------------------------------------

impl RuntimePaths for PathResolver {
    fn runtime_dir(&self) -> std::path::PathBuf {
        PathResolver::runtime_dir(self)
    }
}

// ---------------------------------------------------------------------------
// `IdentityDataDir` impl — wraps the `default_data_dir()` free function
// ---------------------------------------------------------------------------

/// Concrete `IdentityDataDir` adapter that always calls
/// `crate::common::paths::default_data_dir()`. Cheap to construct;
/// callers typically wrap it in an `Arc` and share with the daemon.
#[derive(Debug, Clone, Default)]
pub struct DefaultIdentityDataDir;

impl IdentityDataDir for DefaultIdentityDataDir {
    fn default_data_dir(&self) -> std::path::PathBuf {
        crate::common::paths::default_data_dir()
    }
}

// ---------------------------------------------------------------------------
// `IdentityVault` impl for `Vault`
// ---------------------------------------------------------------------------

/// Convert a root `Credential` (with all its `metadata` blob, etc.)
/// into the narrow `IdentityCredential` DTO that the identity crate
/// understands. The algorithm field is pulled out of the `metadata`
/// JSON for ergonomic matching in the identity crate.
#[must_use]
pub fn credential_to_identity_credential(
    c: &crate::common::vault::Credential,
) -> IdentityCredential {
    identity_credential_from_raw(
        c.id.clone(),
        c.namespace.clone(),
        kind_to_string(&c.kind),
        &c.metadata,
        c.created_at,
        c.material.clone(),
    )
}

fn kind_to_string(kind: &CredentialKind) -> String {
    kind.as_str().to_string()
}

impl IdentityVault for Vault {
    fn list_identity_credentials(&self) -> Vec<IdentityCredential> {
        let filter = CredentialFilter {
            namespace: Some("identity".to_string()),
            kind: Some(CredentialKind::PrivateKey),
            include_system: true,
        };
        Vault::list_credentials(self, &filter)
            .into_iter()
            .filter_map(|summary| {
                let c = Vault::get_credential(self, &summary.id)?;
                Some(credential_to_identity_credential(&c))
            })
            .collect()
    }

    fn get_identity_credential(&self, id: &str) -> Option<IdentityCredential> {
        Vault::get_credential(self, id).map(|c| credential_to_identity_credential(&c))
    }

    fn set_identity_private_key(
        &self,
        key_id: &str,
        algorithm: &str,
        key_b64: &str,
    ) -> anyhow::Result<()> {
        Vault::set_identity_private_key(self, key_id, algorithm, key_b64)
    }

    fn get_identity_private_key(&self, key_id: &str) -> Option<secrecy::SecretString> {
        Vault::get_identity_private_key(self, key_id)
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Build an `Arc<dyn IdentityDataDir>` pointing at the root's
/// `default_data_dir()` resolver. Used by `KeyStorage::new_with_arc`.
#[must_use]
pub fn default_identity_data_dir() -> Arc<dyn IdentityDataDir> {
    Arc::new(DefaultIdentityDataDir)
}

/// Build an `Arc<dyn RuntimePaths>` for the supplied resolver. The
/// `PathResolver` impl of `RuntimePaths` is in this file; this
/// helper hides the `as` coercion at the call site.
#[must_use]
pub fn runtime_paths_arc(resolver: &PathResolver) -> Arc<dyn RuntimePaths> {
    Arc::new(resolver.clone()) as Arc<dyn RuntimePaths>
}

/// Build an `Arc<dyn IdentityVault>` for the supplied vault. The
/// `Vault` is held by `Arc` in the daemon state, so callers pass
/// the existing `Arc<Vault>` and we just coerce to the trait object.
#[must_use]
pub fn identity_vault_arc(vault: Arc<Vault>) -> Arc<dyn IdentityVault> {
    vault as Arc<dyn IdentityVault>
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::vault::Vault;
    use secrecy::ExposeSecret;
    use tempfile::TempDir;

    #[test]
    fn default_identity_data_dir_resolves() {
        let d = DefaultIdentityDataDir;
        let path = d.default_data_dir();
        // Should be non-empty; the exact value depends on env / OS.
        assert!(!path.as_os_str().is_empty());
    }

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
    fn vault_set_get_private_key_round_trip() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "identity-test");
        // The test vault auto-loads via the env-controlled passphrase.
        Vault::set_identity_private_key(
            &vault,
            "did:key:z6Mk#keys-1",
            "ed25519-raw-base64",
            "AAAA",
        )
        .unwrap();
        let got = <Vault as IdentityVault>::get_identity_private_key(&vault, "did:key:z6Mk#keys-1")
            .expect("private key should round-trip");
        assert_eq!(got.expose_secret(), "AAAA");
    }
}
