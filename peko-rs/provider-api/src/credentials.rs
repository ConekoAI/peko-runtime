//! Read-only credential surface that provider construction requires.
//!
//! `peko-providers` is crate-agnostic about how credentials are
//! persisted; it only needs to look up an API key for a configured
//! model and load the rotation list for a `(namespace, name)` slot.
//! Both operations go through the [`CredentialProvider`] trait defined
//! here.
//!
//! The full credential vault (writes, OAuth token management, binding
//! mutation) lives in the root binary-composition layer because it
//! depends on `peko-identity` for encryption and on the daemon's
//! runtime-state lifecycle. `peko-providers` never sees the concrete
//! vault type — only this trait.
//!
//! Phase 6 of the post-migration cleanup. This trait replaces the
//! direct `crate::common::vault::Vault` field that
//! `LlmResolver`/`RotationState` held before the extraction.

use std::sync::Arc;

use secrecy::SecretString;
use thiserror::Error;

/// Material held by a credential entry, as seen by a provider.
///
/// Providers currently hand the raw material to `Provider::new` which
/// wraps it in an `AuthConfig::Bearer` regardless of the underlying
/// vault kind — no per-kind branching happens in the request path.
/// The struct is kept open in case a future provider needs to read
/// e.g. an OAuth `refresh_token` field, in which case the trait
/// surface can grow without an API break.
#[derive(Debug, Clone)]
pub struct CredentialMaterial {
    /// The secret material. Providers hold the `SecretString` through
    /// an `Arc` so the `Display`/`Debug` redacting semantics from the
    /// `secrecy` crate survive the trait boundary.
    pub material: SecretString,
}

/// One entry in a rotation binding, in rotation order.
#[derive(Debug, Clone)]
pub struct RotationEntry {
    /// Credential id used for test-outcome reporting and for `KeyProbeReport`
    /// diagnostics.
    pub credential_id: String,
    /// The secret material for this slot. Stored pre-resolved so
    /// `RotationState` can advance the cursor without re-querying the
    /// backend on every 401.
    pub material: SecretString,
}

/// Errors from credential lookup. Intentionally minimal — providers
/// only need to distinguish "not found" from "backend down" so the
/// resolver can fall back to the next precedence level
/// (`credential_id` → env-var → empty key).
#[derive(Debug, Error)]
pub enum CredentialError {
    /// Unrecoverable I/O or backend failure. The error message is
    /// surfaced through `anyhow::Context`; implementations should not
    /// include any credential material in it.
    #[error("credential backend error: {0}")]
    Backend(String),
}

/// Read-only view of the runtime credential store that provider
/// construction requires.
///
/// Implementations live next to the concrete vault type (root
/// composition layer); `peko-providers` only sees this trait. The
/// `Send + Sync` bound lets `LlmResolver` hold an
/// `Arc<dyn CredentialProvider>` and clone it into every `Provider`
/// instance it builds.
pub trait CredentialProvider: Send + Sync {
    /// Look up a single credential by id. Returns `Ok(None)` if no
    /// credential with that id exists in the backend; returns
    /// `Err(Backend(...))` for unrecoverable I/O failures.
    fn get_credential(&self, id: &str) -> Result<Option<Arc<CredentialMaterial>>, CredentialError>;

    /// Load the ordered credential list bound to `(namespace, name)`.
    /// Returns `Ok(vec![])` if no binding exists; returns
    /// `Err(Backend(...))` for unrecoverable I/O failures.
    fn load_rotation_credentials(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Vec<RotationEntry>, CredentialError>;

    /// Record the outcome of a key-probe test against the credential
    /// identified by `id`. Best-effort: implementations may log and
    /// swallow errors (e.g., if the backend has been torn down). Used
    /// by `RotationState::record_current_test` so future rotation
    /// cursors can skip dead credentials.
    fn record_test(&self, credential_id: &str, ok: bool);
}
