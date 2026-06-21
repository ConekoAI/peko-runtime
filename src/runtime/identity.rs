//! Runtime Identity and Multi-Host Awareness
//!
//! This module provides runtime identity generation and management using did:key.
//!
//! DID format: `did:key:z6Mk{base58-btc-multicodec-ed25519-pubkey}`
//! The multicodec prefix for ed25519-pub is `0xed01` (two bytes: `[0xed, 0x01]`).
//!
//! The private signing key is stored in the encrypted vault; only public
//! identity metadata is kept in `identity.toml`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::RngCore;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use thiserror::Error;
use tracing::info;

use crate::common::paths::PathResolver;
use crate::common::vault::Vault;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

/// Prefix for did:key method
const DID_KEY_PREFIX: &str = "did:key:";

/// Multicodec prefix for ed25519-pub (varint encoded: 0xed01)
const ED25519_PUB_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Errors that can occur when working with DIDs
#[derive(Debug, Error)]
pub enum DidError {
    #[error("Invalid DID format: {0}")]
    InvalidFormat(String),
    #[error("Unsupported DID method: {0}")]
    UnsupportedMethod(String),
    #[error("Base58 decode error: {0}")]
    Base58Decode(String),
    #[error("Invalid public key length: expected 32, got {0}")]
    InvalidKeyLength(usize),
}

/// Runtime identity information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    /// The DID of this runtime (did:key:...)
    pub runtime_did: String,
    /// Key identifier (derived from DID)
    pub key_id: String,
    /// When the identity was created
    pub created_at: DateTime<Utc>,
}

impl RuntimeIdentity {
    /// Generate a new runtime identity with a fresh ed25519 keypair
    pub fn generate() -> Result<(Self, [u8; 32])> {
        let mut secret_key_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_key_bytes);

        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        let public_key_bytes = verifying_key.to_bytes();

        let did = public_key_to_did_key(&public_key_bytes);
        let key_id = format!("{did}#keys-1");
        let created_at = Utc::now();

        info!("Generated new runtime identity: {}", did);

        Ok((
            Self {
                runtime_did: did,
                key_id,
                created_at,
            },
            secret_key_bytes,
        ))
    }

    /// Load identity from a file, or generate a new one if it doesn't exist.
    ///
    /// The private key is stored in the encrypted vault; `identity.toml` only
    /// holds public metadata.
    pub fn generate_or_load(resolver: &PathResolver, vault: &Vault) -> Result<Self> {
        let identity_path = resolver.runtime_dir().join("identity.toml");

        if identity_path.exists() {
            let content = fs::read_to_string(&identity_path)
                .with_context(|| format!("Failed to read identity file: {identity_path:?}"))?;
            // Reject legacy files that contain a `keys` map with plaintext keys.
            if content.contains("[keys]") || content.contains("encrypted_private_key") {
                anyhow::bail!(
                    "Legacy identity.toml format detected at {identity_path:?}. \
                     It contains a plaintext/private key map. Run `peko runtime reset-identity` \
                     or delete the file to regenerate a secure identity."
                );
            }
            let identity: RuntimeIdentity =
                toml::from_str(&content).with_context(|| "Failed to parse identity.toml")?;
            info!("Loaded runtime identity from: {:?}", identity_path);
            return Ok(identity);
        }

        let (identity, private_key_bytes) = Self::generate()?;

        // Store private key in the vault.
        vault
            .set_identity_private_key(
                &identity.key_id,
                "ed25519-raw-base64",
                &BASE64.encode(private_key_bytes),
            )
            .with_context(|| "Failed to store runtime identity private key in vault")?;

        // Ensure parent directory exists
        if let Some(parent) = identity_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create runtime directory: {parent:?}"))?;
        }

        let toml = toml::to_string_pretty(&identity)
            .with_context(|| "Failed to serialize identity to TOML")?;
        fs::write(&identity_path, toml)
            .with_context(|| format!("Failed to write identity file: {identity_path:?}"))?;

        info!("Saved new runtime identity to: {:?}", identity_path);
        Ok(identity)
    }

    /// Load the private signing key for this identity from the vault.
    pub fn load_private_key(&self, vault: &Vault) -> Result<Option<String>> {
        Ok(vault.get_identity_private_key(&self.key_id).map(|s| s.expose_secret().to_string()))
    }

    /// Get the runtime DID
    #[must_use]
    pub fn did(&self) -> &str {
        &self.runtime_did
    }
}

/// Convert a 32-byte ed25519 public key to a did:key string
pub fn public_key_to_did_key(public_key: &[u8; 32]) -> String {
    let mut prefixed = Vec::with_capacity(34);
    prefixed.extend_from_slice(&ED25519_PUB_MULTICODEC);
    prefixed.extend_from_slice(public_key);
    format!("{DID_KEY_PREFIX}z{}", bs58::encode(&prefixed).into_string())
}

/// Convert a did:key string back to a 32-byte ed25519 public key
///
/// Strips the `did:key:` prefix, base58-decodes, and strips the multicodec prefix.
pub fn did_key_to_public_key(did: &str) -> Result<[u8; 32], DidError> {
    let without_prefix = did
        .strip_prefix(DID_KEY_PREFIX)
        .ok_or_else(|| DidError::InvalidFormat(did.to_string()))?;

    if !without_prefix.starts_with('z') {
        return Err(DidError::InvalidFormat(
            "did:key must start with 'z' prefix for base58-btc".to_string(),
        ));
    }

    let base58_part = &without_prefix[1..];
    let decoded = bs58::decode(base58_part)
        .into_vec()
        .map_err(|e| DidError::Base58Decode(e.to_string()))?;

    if decoded.len() < 2 {
        return Err(DidError::InvalidFormat(
            "decoded data too short".to_string(),
        ));
    }

    if decoded[0..2] != ED25519_PUB_MULTICODEC {
        return Err(DidError::UnsupportedMethod(
            "unexpected multicodec prefix".to_string(),
        ));
    }

    let key_bytes = &decoded[2..];
    if key_bytes.len() != 32 {
        return Err(DidError::InvalidKeyLength(key_bytes.len()));
    }

    let mut result = [0u8; 32];
    result.copy_from_slice(key_bytes);
    Ok(result)
}

/// Get the path to the identity file
#[must_use]
pub fn identity_file_path(resolver: &PathResolver) -> PathBuf {
    resolver.runtime_dir().join("identity.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_public_key_to_did_key_roundtrip() {
        let public_key: [u8; 32] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];

        let did = public_key_to_did_key(&public_key);
        assert!(did.starts_with("did:key:z6Mk"));

        let recovered = did_key_to_public_key(&did).unwrap();
        assert_eq!(recovered, public_key);
    }

    #[test]
    fn test_did_key_to_public_key_invalid_did() {
        let result = did_key_to_public_key("invalid");
        assert!(matches!(result, Err(DidError::InvalidFormat(_))));
    }

    #[test]
    fn test_did_key_to_public_key_wrong_method() {
        let result = did_key_to_public_key("did:web:example.com");
        assert!(matches!(result, Err(DidError::InvalidFormat(_))));
    }

    #[test]
    fn test_runtime_identity_generate() {
        let (identity, private_key) = RuntimeIdentity::generate().unwrap();
        assert!(identity.runtime_did.starts_with("did:key:z6Mk"));
        assert_eq!(private_key.len(), 32);
        assert!(identity.key_id.starts_with(&identity.runtime_did));
    }

    #[test]
    fn test_runtime_identity_generate_or_load_stores_key_in_vault() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "identity-test");
        let resolver = PathResolver::with_dirs(
            dir.path().to_path_buf(),
            dir.path().join("data"),
            dir.path().join("cache"),
        );

        let identity = RuntimeIdentity::generate_or_load(&resolver, &vault).unwrap();
        let loaded = RuntimeIdentity::generate_or_load(&resolver, &vault).unwrap();
        assert_eq!(loaded.runtime_did, identity.runtime_did);

        let private_key = loaded.load_private_key(&vault).unwrap();
        assert!(private_key.is_some());

        // identity.toml should not contain private key material.
        let content = fs::read_to_string(identity_file_path(&resolver)).unwrap();
        assert!(!content.contains("encrypted_private_key"));
        assert!(!content.contains("[keys]"));
    }

    #[test]
    fn test_legacy_identity_file_rejected() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "identity-test");
        let resolver = PathResolver::with_dirs(
            dir.path().to_path_buf(),
            dir.path().join("data"),
            dir.path().join("cache"),
        );

        fs::create_dir_all(resolver.runtime_dir()).unwrap();
        let legacy = r#"
runtime_did = "did:key:z6MkTest"
key_id = "did:key:z6MkTest#keys-1"
created_at = "2024-01-01T00:00:00Z"

[keys]
"did:key:z6MkTest#keys-1" = { encrypted_private_key = "c2VjcmV0", algorithm = "ed25519-raw-base64" }
"#;
        fs::write(identity_file_path(&resolver), legacy).unwrap();

        assert!(RuntimeIdentity::generate_or_load(&resolver, &vault).is_err());
    }
}
