//! Trait ports that abstract root-only deps consumed by identity
//! types. `peko-identity` is a leaf crate; it must not depend on root
//! or on any other workspace crate that has root in its dep tree
//! (e.g. `peko-quota`, `peko-providers`, `peko-session`,
//! `peko-extension-host`).
//!
//! Three narrow traits cover everything `RuntimeIdentity`,
//! `RuntimeMetadata`, and `KeyStorage` need from the host:
//!
//! - [`RuntimePaths`] — abstracts `crate::common::paths::PathResolver`.
//!   Implementors provide `runtime_dir()` so the runtime identity + metadata
//!   TOML files can live in the canonical `{config_dir}/runtime` location.
//! - [`IdentityDataDir`] — abstracts
//!   `crate::common::paths::default_data_dir()` (a free function, but
//!   lifted to a trait so `peko-identity` doesn't need the function).
//!   Implementors provide the platform-default data dir used for
//!   `KeyStorage::new()`.
//! - [`IdentityVault`] — abstracts the narrow vault surface used by
//!   `RuntimeIdentity::generate_or_load` /
//!   `RuntimeIdentity::reconstruct_from_credential`. Implementors
//!   provide identity-credential lookup + private-key get/set; they
//!   do NOT need to expose the full `Vault` API.
//!
//! All three traits are `Send + Sync + 'static` so they can flow
//! through `Arc<dyn ...>` ownership shared between daemon and
//! identity-crate threads.
//!
//! ## Orphan rule + impl location
//!
//! Rust's orphan rule forbids `impl ForeignTrait for ForeignType` in a
//! third crate. These traits live in `peko-identity`, but the
//! implementing types (`crate::common::paths::PathResolver`,
//! `crate::common::vault::Vault`) live in root. The implementations
//! live in root (e.g. `src/daemon/state.rs` or a new
//! `src/identity_compat.rs`), with the local-type side satisfying the
//! orphan rule.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde_json::Value;

/// Minimal path-resolver surface needed by runtime-identity /
/// runtime-metadata code.
///
/// `crate::common::paths::PathResolver` implements this trait in root
/// (`src/daemon/state.rs` or a new compat module). `peko-identity`
/// never touches the concrete `PathResolver` type.
pub trait RuntimePaths: Send + Sync + 'static {
    /// The `{config_dir}/runtime` directory used to persist
    /// `identity.toml` + `runtime.toml`.
    fn runtime_dir(&self) -> PathBuf;
}

/// Provides the platform-default data directory used to locate the
/// identity store (`KeyStorage::new()`).
///
/// `crate::common::paths::default_data_dir()` is a free function; the
/// trait wraps it so callers go through `Arc<dyn IdentityDataDir>`
/// instead of pulling in the full paths module.
pub trait IdentityDataDir: Send + Sync + 'static {
    /// Default data dir for the current platform + `PEKO_HOME` env.
    /// Mirrors `crate::common::paths::default_data_dir()`.
    fn default_data_dir(&self) -> PathBuf;
}

/// Identity-namespace vault credential, returned by [`IdentityVault::list_identity_credentials`]
/// and [`IdentityVault::get_identity_credential`].
///
/// Subset of `crate::common::vault::Credential` — only the fields the
/// runtime-identity reconstruction needs. The trait port avoids
/// pulling the full `Vault` API surface into `peko-identity`.
#[derive(Debug, Clone)]
pub struct IdentityCredential {
    /// Credential id (used as the lookup key).
    pub id: String,
    /// Namespace tag (`"identity"` for runtime-identity credentials).
    pub namespace: String,
    /// Kind tag (`"private_key"` for runtime-identity credentials).
    pub kind: String,
    /// Algorithm metadata field (e.g. `"ed25519-raw-base64"`).
    pub algorithm: String,
    /// When the credential was created.
    pub created_at: DateTime<Utc>,
    /// Secret material (base64-encoded ed25519 private key bytes).
    pub material: SecretString,
}

/// Narrow trait over the vault operations `RuntimeIdentity` needs.
///
/// The implementor is typically `crate::common::vault::Vault`; the
/// impl lives in root. `peko-identity` never imports the full `Vault`
/// type, only this narrow port.
pub trait IdentityVault: Send + Sync + 'static {
    /// List identity-namespace credentials (`namespace == "identity"`).
    /// Filters out non-identity credentials so callers don't have to.
    fn list_identity_credentials(&self) -> Vec<IdentityCredential>;

    /// Look up a single credential by id. Returns `None` for unknown ids.
    fn get_identity_credential(&self, id: &str) -> Option<IdentityCredential>;

    /// Store the runtime-identity private key under `key_id` with the
    /// given `algorithm` tag (e.g. `"ed25519-raw-base64"`). The key
    /// material is a base64-encoded 32-byte secret seed.
    fn set_identity_private_key(&self, key_id: &str, algorithm: &str, key_b64: &str) -> Result<()>;

    /// Retrieve the runtime-identity private key for `key_id`, if one
    /// is stored. Returns `None` for unknown ids.
    fn get_identity_private_key(&self, key_id: &str) -> Option<SecretString>;
}

/// Convenience: build an [`IdentityCredential`] from a raw `(id, namespace,
/// kind, metadata, created_at, material)` tuple. Used by the root-side
/// `Vault` impl in [`crate::daemon::state::AppState`] (or a new
/// `src/identity_compat.rs`).
///
/// Defined here so the root-side impl can construct
/// `IdentityCredential` values without exposing `crate::common::vault`
/// types to `peko-identity`.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn identity_credential_from_raw(
    id: String,
    namespace: String,
    kind: String,
    metadata: &Value,
    created_at: DateTime<Utc>,
    material: SecretString,
) -> IdentityCredential {
    let algorithm = metadata
        .get("algorithm")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    IdentityCredential {
        id,
        namespace,
        kind,
        algorithm,
        created_at,
        material,
    }
}

/// Path helper used by `KeyStorage::default_storage_path` to mirror the
/// pre-Phase-3 behavior of `default_data_dir().join("identities")`.
///
/// Convenience free function so the trait method doesn't need to be
/// called separately for the `identities` subdirectory.
#[must_use]
pub fn identities_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("identities")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn identities_dir_appends_subdirectory() {
        let dir = Path::new("/var/data");
        assert_eq!(identities_dir(dir), PathBuf::from("/var/data/identities"));
    }

    #[test]
    fn identity_credential_from_raw_defaults_algorithm_to_empty() {
        let metadata = serde_json::json!({});
        let cred = identity_credential_from_raw(
            "id1".into(),
            "identity".into(),
            "private_key".into(),
            &metadata,
            chrono::Utc::now(),
            SecretString::new("secret".into()),
        );
        assert_eq!(cred.id, "id1");
        assert_eq!(cred.namespace, "identity");
        assert_eq!(cred.kind, "private_key");
        assert_eq!(cred.algorithm, "");
    }

    #[test]
    fn identity_credential_from_raw_extracts_algorithm_metadata() {
        let metadata = serde_json::json!({"algorithm": "ed25519-raw-base64"});
        let cred = identity_credential_from_raw(
            "id2".into(),
            "identity".into(),
            "private_key".into(),
            &metadata,
            chrono::Utc::now(),
            SecretString::new("secret".into()),
        );
        assert_eq!(cred.algorithm, "ed25519-raw-base64");
    }
}
