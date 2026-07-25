//! `VaultAccess` — narrow cross-boundary view of the root `Vault`.
//!
//! The framework's parameter-resolution path needs to look up
//! secrets by `(namespace, name)`. The framework can't depend on
//! `crate::common::vault::Vault` (a root-only concrete type), so it
//! defines this trait and root's `Vault` implements it.
//!
//! The method shape matches `Vault::get_material_for` so the impl
//! in root is a single delegation.

use secrecy::SecretString;

/// Narrow vault access trait used by the framework's
/// `services::reserved_params` module.
///
/// Frameworks only need the lookup-by-key API; vault rotation,
/// migration, encryption-at-rest, and the rest of the surface
/// stay in root. Implementors MUST be `Send + Sync` so the
/// resolved-params free functions can take `&dyn VaultAccess`
/// across `.await` points.
pub trait VaultAccess: Send + Sync {
    /// Look up a secret material by `(namespace, name)`. Returns
    /// `Ok(Some(_))` if present, `Ok(None)` if not in the vault,
    /// and `Err(_)` for I/O or decryption failures.
    ///
    /// Mirrors `crate::common::vault::Vault::get_material_for`.
    fn get_material_for(&self, namespace: &str, name: &str)
        -> anyhow::Result<Option<SecretString>>;
}
