//! Trust store for principal package imports.
//!
//! Implements TOFU (trust-on-first-use) pinning of principal names to the
//! publisher DID they were first imported with. This prevents a replaced
//! package with a new self-attested identity from silently verifying.

use crate::common::paths::PathResolver;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use tracing::{info, warn};

/// Policy controlling how trust pinning conflicts are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPolicy {
    /// Trust-on-first-use: pin the first DID seen for a name, and reject
    /// later imports signed by a different DID unless explicitly overridden.
    #[default]
    Tofu,
    /// Allow importing a principal whose DID does not match the pinned entry.
    /// The pin is updated to the new DID.
    AllowUntrusted,
}

/// Result of checking whether a principal name is trusted for a given DID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// No prior import of this principal name.
    Unknown,
    /// The name is already pinned to this DID.
    Trusted,
    /// The name is pinned to a different DID.
    Mismatch {
        /// The currently pinned DID.
        expected: String,
        /// The DID in the package being imported.
        actual: String,
    },
}

/// A pinned publisher identity for a principal name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPublisher {
    /// Publisher DID.
    pub did: String,
    /// Public key used for package signing, encoded as multibase (`z{base58}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// When this pin was created or last updated.
    pub trusted_at: DateTime<Utc>,
}

/// Persistent trust store mapping principal names to publisher DIDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustStore {
    publishers: BTreeMap<String, TrustedPublisher>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TrustStore {
    /// Create an empty trust store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            publishers: BTreeMap::new(),
        }
    }

    /// Load from disk or create a new empty trust store.
    pub fn load_or_create(resolver: &PathResolver) -> Result<Self> {
        let store_path = resolver.data_dir().join("trusted_publishers.toml");

        if store_path.exists() {
            let content = fs::read_to_string(&store_path)
                .with_context(|| format!("Failed to read trust store: {store_path:?}"))?;
            let store: TrustStore = toml::from_str(&content)
                .with_context(|| "Failed to parse trusted_publishers.toml")?;
            info!("Loaded trust store from: {:?}", store_path);
            return Ok(store);
        }

        let store = Self::new();

        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create data directory: {parent:?}"))?;
        }

        let toml =
            toml::to_string_pretty(&store).with_context(|| "Failed to serialize trust store")?;
        fs::write(&store_path, toml)
            .with_context(|| format!("Failed to write trust store: {store_path:?}"))?;

        info!("Created empty trust store at: {:?}", store_path);
        Ok(store)
    }

    /// Save the trust store to disk.
    pub fn save(&self, resolver: &PathResolver) -> Result<()> {
        let store_path = resolver.data_dir().join("trusted_publishers.toml");

        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create data directory: {parent:?}"))?;
        }

        let toml =
            toml::to_string_pretty(self).with_context(|| "Failed to serialize trust store")?;
        fs::write(&store_path, toml)
            .with_context(|| format!("Failed to write trust store: {store_path:?}"))?;

        Ok(())
    }

    /// Check whether `name` is trusted for `did`.
    #[must_use]
    pub fn is_trusted(&self, name: &str, did: &str) -> TrustStatus {
        match self.publishers.get(name) {
            None => TrustStatus::Unknown,
            Some(entry) if entry.did == did => TrustStatus::Trusted,
            Some(entry) => TrustStatus::Mismatch {
                expected: entry.did.clone(),
                actual: did.to_string(),
            },
        }
    }

    /// Pin `name` to `did`. Optionally stores the signing public key.
    pub fn pin(
        &mut self,
        name: impl Into<String>,
        did: impl Into<String>,
        public_key: Option<String>,
    ) {
        let name = name.into();
        let did = did.into();
        self.publishers.insert(
            name.clone(),
            TrustedPublisher {
                did: did.clone(),
                public_key,
                trusted_at: Utc::now(),
            },
        );
        info!("Pinned principal '{}' to DID {}", name, did);
    }

    /// Remove a pin for `name`.
    pub fn unpin(&mut self, name: &str) -> Result<()> {
        if self.publishers.remove(name).is_some() {
            info!("Removed trust pin for principal '{}'", name);
            Ok(())
        } else {
            warn!("Tried to remove unknown trust pin for principal '{}'", name);
            anyhow::bail!("Principal '{}' is not pinned", name)
        }
    }

    /// Return the pinned publisher for `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TrustedPublisher> {
        self.publishers.get(name)
    }

    /// List all pinned principal names.
    pub fn list(&self) -> impl Iterator<Item = (&String, &TrustedPublisher)> {
        self.publishers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_then_pin() {
        let mut store = TrustStore::new();
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Unknown
        );
        store.pin("foo", "did:peko:public:abc", Some("zabc".to_string()));
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Trusted
        );
        assert!(store.get("foo").unwrap().public_key.is_some());
    }

    #[test]
    fn trusted() {
        let mut store = TrustStore::new();
        store.pin("foo", "did:peko:public:abc", None);
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Trusted
        );
        assert_eq!(
            store.is_trusted("bar", "did:peko:public:abc"),
            TrustStatus::Unknown
        );
    }

    #[test]
    fn mismatch() {
        let mut store = TrustStore::new();
        store.pin("foo", "did:peko:public:abc", None);
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:def"),
            TrustStatus::Mismatch {
                expected: "did:peko:public:abc".to_string(),
                actual: "did:peko:public:def".to_string(),
            }
        );
    }

    #[test]
    fn update_pin() {
        let mut store = TrustStore::new();
        store.pin("foo", "did:peko:public:abc", None);
        store.pin("foo", "did:peko:public:def", Some("zdef".to_string()));
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:def"),
            TrustStatus::Trusted
        );
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Mismatch {
                expected: "did:peko:public:def".to_string(),
                actual: "did:peko:public:abc".to_string(),
            }
        );
        assert_eq!(
            store.get("foo").unwrap().public_key.as_deref(),
            Some("zdef")
        );
    }

    #[test]
    fn unpin_existing() {
        let mut store = TrustStore::new();
        store.pin("foo", "did:peko:public:abc", None);
        store.unpin("foo").unwrap();
        assert_eq!(
            store.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Unknown
        );
    }

    #[test]
    fn unpin_unknown_fails() {
        let mut store = TrustStore::new();
        assert!(store.unpin("foo").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let mut store = TrustStore::new();
        store.pin("foo", "did:peko:public:abc", Some("zabc".to_string()));

        let toml_str = toml::to_string_pretty(&store).unwrap();
        let parsed: TrustStore = toml::from_str(&toml_str).unwrap();

        assert_eq!(
            parsed.is_trusted("foo", "did:peko:public:abc"),
            TrustStatus::Trusted
        );
        assert_eq!(
            parsed.get("foo").unwrap().public_key.as_deref(),
            Some("zabc")
        );
    }
}
