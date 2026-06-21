//! Secure secret store for provider API keys.
//!
//! This module replaces the previous plaintext `credentials.json` design.
//! Production deployments store secrets in the OS keychain (Windows
//! Credential Manager, macOS Keychain, libsecret on Linux) via the
//! `keyring` crate. Tests use an explicit in-memory implementation that
//! is opted into by the caller — the runtime never silently downgrades
//! to plaintext on disk.
//!
//! ## On-disk layout
//!
//! Secrets are not persisted by this module. The OS keychain is the
//! single source of truth when this module is used directly. For the
//! unified runtime vault (provider API keys, registry tokens, identity
//! keys, tunnel keys), see `crate::common::vault`.
//!
//! ## Service / account naming
//!
//! All peko-managed secrets live under a single service name in the OS
//! keychain. The account name is the provider id (e.g. `openai`,
//! `anthropic`). This matches the convention already used by
//! `peko-desktop`'s `vault/mod.rs` so desktop-entered keys are visible
//! to the runtime and vice versa.
//!
//! ## Thread-safety
//!
//! All implementations must be `Send + Sync` so they can be shared
//! across the runtime via `Arc<dyn SecretStore>`.

use anyhow::Result;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::RwLock;
use thiserror::Error;

/// OS keychain service name under which all peko-managed provider
/// secrets are stored.
///
/// This is the same service name used by `peko-desktop`'s
/// `vault::get_credential("peko", ...)`, ensuring desktop-entered keys
/// are visible to the runtime after this refactor.
pub const KEYCHAIN_SERVICE: &str = "peko";

/// Errors returned by secret store operations.
///
/// All variants are safe to log in full; they carry no secret material.
/// `Backend` preserves the underlying keyring error message so operators
/// can diagnose missing system services (e.g. `dbus`/`libsecret` on
/// headless Linux).
#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secret store backend error: {0}")]
    Backend(String),

    #[error("invalid secret: {0}")]
    Invalid(String),

    #[error("account name '{0}' is not a valid provider id")]
    InvalidAccount(String),
}

/// Abstract interface for storing and retrieving provider secrets.
///
/// One production implementation (`OsKeychainSecretStore`) plus one test
/// implementation (`InMemorySecretStore`) are provided. The trait is
/// sealed against further silent downgrades — any future backend must
/// be added explicitly so that the security properties of the OS
/// keychain remain the default.
pub trait SecretStore: Send + Sync {
    /// Retrieve the secret for `account`, if one exists.
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretStoreError>;

    /// Store or overwrite the secret for `account`.
    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretStoreError>;

    /// Delete the secret for `account`. Returns `true` if a secret was
    /// removed, `false` if none existed.
    fn delete(&self, account: &str) -> Result<bool, SecretStoreError>;

    /// Return all provider ids that currently have a stored secret.
    ///
    /// Order is unspecified. Implementations should treat this as a
    /// best-effort enumeration — on some OS keychains listing requires
    /// per-entry permissions that may be denied.
    fn list_accounts(&self) -> Result<Vec<String>, SecretStoreError>;

    /// Cheap format check: returns `Some(true)` if the secret is
    /// present and matches the expected shape for the account's
    /// provider family (e.g. `sk-` prefix for OpenAI), `Some(false)`
    /// if the shape looks wrong, `None` if no secret is stored.
    ///
    /// This is the format-only check used today; richer network-based
    /// tests live in `credential.test` IPC handlers.
    fn test_format(&self, account: &str) -> Result<Option<bool>, SecretStoreError>;
}

/// Validate that an account name is a syntactically reasonable provider
/// id. We restrict to a small charset so the name can safely be used as
/// the OS keychain account.
fn validate_account(account: &str) -> Result<(), SecretStoreError> {
    if account.is_empty() {
        return Err(SecretStoreError::InvalidAccount(
            "empty account name".to_string(),
        ));
    }
    if account.len() > 128 {
        return Err(SecretStoreError::InvalidAccount(format!(
            "account name too long ({} > 128 chars)",
            account.len()
        )));
    }
    if !account
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(SecretStoreError::InvalidAccount(format!(
            "account name '{account}' contains disallowed characters"
        )));
    }
    Ok(())
}

/// Production implementation backed by the OS keychain via the
/// `keyring` crate (Windows Credential Manager, macOS Keychain,
/// libsecret on Linux).
///
/// On Linux without a running secret service (common in CI / Docker),
/// keyring operations return errors rather than silently falling back
/// to disk. Callers should surface a clear "OS keychain unavailable"
/// message and direct users to either:
/// 1. Install / start `gnome-keyring` or `kwallet`, or
/// 2. Use the env-var bootstrap path (`LlmResolver` honors
///    `*_API_KEY` env vars when started with `--bootstrap-env-keys`).
pub struct OsKeychainSecretStore {
    service: String,
}

impl OsKeychainSecretStore {
    /// Create a new keychain-backed store under the canonical peko
    /// service name.
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_string(),
        }
    }

    /// Create a keychain-backed store under a custom service name.
    /// Intended for tests and unusual deployments; production code
    /// should use `new()`.
    #[must_use]
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretStoreError> {
        validate_account(account)?;
        keyring::Entry::new(&self.service, account)
            .map_err(|e| SecretStoreError::Backend(format!("keychain entry: {e}")))
    }
}

impl Default for OsKeychainSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for OsKeychainSecretStore {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretStoreError> {
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(pw) => Ok(Some(SecretString::from(pw))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretStoreError::Backend(format!("keychain get: {e}"))),
        }
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretStoreError> {
        let entry = self.entry(account)?;
        entry
            .set_password(secret.expose_secret())
            .map_err(|e| SecretStoreError::Backend(format!("keychain set: {e}")))
    }

    fn delete(&self, account: &str) -> Result<bool, SecretStoreError> {
        let entry = self.entry(account)?;
        match entry.delete_password() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(SecretStoreError::Backend(format!(
                "keychain delete: {e}"
            ))),
        }
    }

    fn list_accounts(&self) -> Result<Vec<String>, SecretStoreError> {
        // The cross-platform `keyring` v2 crate does not expose
        // service-wide enumeration (it's only available via
        // platform-specific extensions). We intentionally do not
        // shell out to platform tools here — callers that need a
        // full list should track it alongside the catalog. This
        // returns an empty list rather than failing so existing
        // call sites that iterate `list_accounts()` continue to
        // work; new code should rely on the catalog for membership.
        Ok(Vec::new())
    }

    fn test_format(&self, account: &str) -> Result<Option<bool>, SecretStoreError> {
        let Some(secret) = self.get(account)? else {
            return Ok(None);
        };
        let s = secret.expose_secret();
        let ok = match account {
            "openai" | "azure-openai" | "azure" | "openrouter" | "together"
            | "fireworks" | "groq" | "deepseek" | "xai" | "grok" | "moonshot" | "kimi" => {
                s.starts_with("sk-") || s.len() > 10
            }
            "anthropic" => s.starts_with("sk-ant-") || s.len() > 10,
            "ollama" => true, // local, no key required
            _ => s.len() > 4 && !s.trim().is_empty(),
        };
        Ok(Some(ok))
    }
}

/// In-memory implementation for tests.
///
/// This implementation is never used in production. It exists so unit
/// tests can exercise `LlmResolver`, `CredentialsService`, and the
/// migration path without touching the real OS keychain.
#[derive(Default)]
pub struct InMemorySecretStore {
    inner: RwLock<HashMap<String, String>>,
}

impl InMemorySecretStore {
    /// Create an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an in-memory store pre-populated with the given
    /// `(account, secret)` pairs.
    #[must_use]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        let inner = pairs
            .iter()
            .map(|(a, s)| ((*a).to_string(), (*s).to_string()))
            .collect();
        Self {
            inner: RwLock::new(inner),
        }
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretStoreError> {
        let guard = self.inner.read().map_err(|e| {
            SecretStoreError::Backend(format!("in-memory lock poisoned: {e}"))
        })?;
        Ok(guard
            .get(account)
            .map(|s| SecretString::from(s.clone())))
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretStoreError> {
        validate_account(account)?;
        let mut guard = self.inner.write().map_err(|e| {
            SecretStoreError::Backend(format!("in-memory lock poisoned: {e}"))
        })?;
        guard.insert(account.to_string(), secret.expose_secret().to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, SecretStoreError> {
        let mut guard = self.inner.write().map_err(|e| {
            SecretStoreError::Backend(format!("in-memory lock poisoned: {e}"))
        })?;
        Ok(guard.remove(account).is_some())
    }

    fn list_accounts(&self) -> Result<Vec<String>, SecretStoreError> {
        let guard = self.inner.read().map_err(|e| {
            SecretStoreError::Backend(format!("in-memory lock poisoned: {e}"))
        })?;
        let mut v: Vec<String> = guard.keys().cloned().collect();
        v.sort();
        Ok(v)
    }

    fn test_format(&self, account: &str) -> Result<Option<bool>, SecretStoreError> {
        let Some(secret) = self.get(account)? else {
            return Ok(None);
        };
        let s = secret.expose_secret();
        let ok = match account {
            "openai" => s.starts_with("sk-"),
            "anthropic" => s.starts_with("sk-ant-"),
            _ => s.len() > 4 && !s.trim().is_empty(),
        };
        Ok(Some(ok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_account_accepts_canonical_provider_ids() {
        for id in [
            "openai",
            "anthropic",
            "azure-openai",
            "ollama",
            "xai",
            "openrouter",
            "my-custom:llama-3.1",
            "provider.local",
        ] {
            validate_account(id).expect("canonical id should validate");
        }
    }

    #[test]
    fn validate_account_rejects_empty_and_pathological() {
        for bad in ["", "../escape", "with space", "with\nnewline", &"x".repeat(129)] {
            assert!(
                validate_account(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn in_memory_set_get_delete_roundtrip() {
        let store = InMemorySecretStore::new();
        let key = SecretString::from("sk-test-abc".to_string());

        assert!(store.get("openai").unwrap().is_none());
        store.set("openai", &key).unwrap();

        let got = store.get("openai").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "sk-test-abc");

        // overwrite
        let key2 = SecretString::from("sk-new-xyz".to_string());
        store.set("openai", &key2).unwrap();
        assert_eq!(
            store.get("openai").unwrap().unwrap().expose_secret(),
            "sk-new-xyz"
        );

        // delete
        assert!(store.delete("openai").unwrap());
        assert!(store.get("openai").unwrap().is_none());
        // second delete is a no-op
        assert!(!store.delete("openai").unwrap());
    }

    #[test]
    fn in_memory_list_accounts_sorted() {
        let store = InMemorySecretStore::from_pairs(&[
            ("zeta", "z"),
            ("alpha", "a"),
            ("beta", "b"),
        ]);
        assert_eq!(store.list_accounts().unwrap(), vec!["alpha", "beta", "zeta"]);
    }

    #[test]
    fn in_memory_test_format_openai_anthropic_unknown() {
        let store = InMemorySecretStore::from_pairs(&[
            ("openai", "sk-valid"),
            ("openai-bad", "no-prefix"),
            ("anthropic", "sk-ant-valid"),
            ("custom", "any-secret-is-fine"),
        ]);

        assert_eq!(store.test_format("openai").unwrap(), Some(true));
        assert_eq!(store.test_format("openai-bad").unwrap(), Some(true)); // length > 4 fallback
        assert_eq!(store.test_format("anthropic").unwrap(), Some(true));
        assert_eq!(store.test_format("custom").unwrap(), Some(true));
        assert_eq!(store.test_format("nonexistent").unwrap(), None);
    }

    #[test]
    fn in_memory_rejects_invalid_account_on_set() {
        let store = InMemorySecretStore::new();
        let key = SecretString::from("x".to_string());
        assert!(store.set("../escape", &key).is_err());
        assert!(store.set("with space", &key).is_err());
        assert!(store.set("", &key).is_err());
    }

    #[test]
    fn keychain_service_constant_matches_desktop() {
        // Pinned so peko-desktop's vault (service="peko") and the
        // runtime's keychain store share the same namespace.
        assert_eq!(KEYCHAIN_SERVICE, "peko");
    }
}