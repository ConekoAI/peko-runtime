//! Secure secret store interface for provider API keys.
//!
//! This module defines the [`SecretStore`] trait and an in-memory test
//! implementation. Production provider API keys live in the unified
//! encrypted vault (`crate::common::vault`); the OS keychain is only used
//! for the vault data-encryption key (DEK), not for individual secrets.
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
/// The in-memory implementation (`InMemorySecretStore`) is provided for
/// tests. Production code should use `crate::common::vault::Vault`,
/// which implements this trait as a backward-compat shim over the
/// generic credential store.
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
        let guard = self
            .inner
            .read()
            .map_err(|e| SecretStoreError::Backend(format!("in-memory lock poisoned: {e}")))?;
        Ok(guard.get(account).map(|s| SecretString::from(s.clone())))
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretStoreError> {
        validate_account(account)?;
        let mut guard = self
            .inner
            .write()
            .map_err(|e| SecretStoreError::Backend(format!("in-memory lock poisoned: {e}")))?;
        guard.insert(account.to_string(), secret.expose_secret().to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, SecretStoreError> {
        let mut guard = self
            .inner
            .write()
            .map_err(|e| SecretStoreError::Backend(format!("in-memory lock poisoned: {e}")))?;
        Ok(guard.remove(account).is_some())
    }

    fn list_accounts(&self) -> Result<Vec<String>, SecretStoreError> {
        let guard = self
            .inner
            .read()
            .map_err(|e| SecretStoreError::Backend(format!("in-memory lock poisoned: {e}")))?;
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
        for bad in [
            "",
            "../escape",
            "with space",
            "with\nnewline",
            &"x".repeat(129),
        ] {
            assert!(validate_account(bad).is_err(), "should reject {bad:?}");
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
        let store =
            InMemorySecretStore::from_pairs(&[("zeta", "z"), ("alpha", "a"), ("beta", "b")]);
        assert_eq!(
            store.list_accounts().unwrap(),
            vec!["alpha", "beta", "zeta"]
        );
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
}
