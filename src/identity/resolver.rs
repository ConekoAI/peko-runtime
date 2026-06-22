//! DID Resolution service
//!
//! Resolves DIDs to their DID documents. Supports:
//! - Local storage resolution (for identities we control)
//! - Caching for performance

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::identity::did::DIDDocument;
use crate::identity::storage::KeyStorage;

/// Cache entry with expiration
#[derive(Clone)]
struct CacheEntry {
    document: DIDDocument,
    expires_at: Instant,
}

/// DID Resolver
pub struct DidResolver {
    /// Local key storage for owned identities
    local_storage: Option<KeyStorage>,
    /// Cache for resolved documents
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    /// Cache TTL (default: 1 hour)
    cache_ttl: Duration,
}

impl DidResolver {
    /// Create a new resolver with local storage only
    #[must_use]
    pub fn local(storage: KeyStorage) -> Self {
        Self {
            local_storage: Some(storage),
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_hours(1), // 1 hour
        }
    }

    /// Create a read-only resolver (for verifying others' DIDs)
    #[must_use]
    pub fn readonly() -> Self {
        Self {
            local_storage: None,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_hours(1),
        }
    }

    /// Resolve a DID to its document
    pub async fn resolve(&self, did: &str) -> Result<DIDDocument> {
        // Check cache first
        if let Some(doc) = self.get_cached(did).await {
            debug!("Cache hit for DID: {}", did);
            return Ok(doc);
        }

        // Try local storage
        if let Some(ref storage) = self.local_storage {
            if storage.exists(did) {
                let identity = storage.load(did)?;
                let doc = identity.document.clone();
                self.cache_document(did.to_string(), doc.clone()).await;
                return Ok(doc);
            }
        }

        anyhow::bail!("DID not found: {did}")
    }

    /// Resolve multiple DIDs in parallel
    pub async fn resolve_batch(&self, dids: &[String]) -> Vec<(String, Result<DIDDocument>)> {
        let futures = dids.iter().map(|did| {
            let did = did.clone();
            async move {
                let result = self.resolve(&did).await;
                (did, result)
            }
        });

        futures::future::join_all(futures).await
    }

    /// Get a cached document if valid
    async fn get_cached(&self, did: &str) -> Option<DIDDocument> {
        let cache = self.cache.read().await;
        cache.get(did).and_then(|entry| {
            if entry.expires_at > Instant::now() {
                Some(entry.document.clone())
            } else {
                None
            }
        })
    }

    /// Cache a document
    async fn cache_document(&self, did: String, document: DIDDocument) {
        let mut cache = self.cache.write().await;
        cache.insert(
            did,
            CacheEntry {
                document,
                expires_at: Instant::now() + self.cache_ttl,
            },
        );
    }

    /// Verify a DID is valid and resolvable
    pub async fn verify(&self, did: &str) -> bool {
        self.resolve(did).await.is_ok()
    }

    /// Get the public key for a DID
    pub async fn get_public_key(&self, did: &str) -> Result<Vec<u8>> {
        let document = self.resolve(did).await?;

        // Get the first verification method
        let vm = document
            .verification_method
            .first()
            .context("No verification methods in DID document")?;

        // Decode multibase public key
        let key_multibase = &vm.public_key_multibase;
        if !key_multibase.starts_with('z') {
            anyhow::bail!("Unsupported key encoding");
        }

        let key_bytes = bs58::decode(&key_multibase[1..])
            .into_vec()
            .context("Failed to decode public key")?;

        Ok(key_bytes)
    }

    /// Clear the cache
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        info!("DID resolver cache cleared");
    }

    /// Get cache stats
    pub async fn cache_stats(&self) -> (usize, usize) {
        let cache = self.cache.read().await;
        let total = cache.len();
        let valid = cache
            .values()
            .filter(|entry| entry.expires_at > Instant::now())
            .count();
        (valid, total)
    }

    /// Set cache TTL
    pub fn set_cache_ttl(&mut self, ttl: Duration) {
        self.cache_ttl = ttl;
    }
}

/// Quick synchronous resolution for local identities
pub fn resolve_local_sync(did: &str, storage: &KeyStorage) -> Result<DIDDocument> {
    let identity = storage.load(did)?;
    Ok(identity.document)
}

/// Verify a signature using DID resolution
pub async fn verify_signature(
    resolver: &DidResolver,
    did: &str,
    message: &[u8],
    signature: &[u8; 64],
) -> Result<()> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    // Get public key
    let key_bytes = resolver.get_public_key(did).await?;
    if key_bytes.len() != 32 {
        anyhow::bail!("Invalid public key length");
    }

    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&key_bytes);

    let verifying_key = VerifyingKey::from_bytes(&key_array)?;
    let sig = Signature::from_bytes(signature);

    verifying_key
        .verify(message, &sig)
        .map_err(|e| anyhow::anyhow!("Signature verification failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::did::DIDScope;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_resolve_local() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();
        let identity = storage
            .generate_identity(DIDScope::Local, Some("test"))
            .unwrap();

        let resolver = DidResolver::local(storage);
        let doc = resolver.resolve(&identity.did).await.unwrap();

        assert_eq!(doc.id, identity.did);
    }

    #[tokio::test]
    async fn test_cache() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();
        let identity = storage.generate_identity(DIDScope::Public, None).unwrap();

        let resolver = DidResolver::local(storage);

        // First resolve
        let doc1 = resolver.resolve(&identity.did).await.unwrap();

        // Second resolve (should hit cache)
        let doc2 = resolver.resolve(&identity.did).await.unwrap();

        assert_eq!(doc1.id, doc2.id);

        let (valid, total) = resolver.cache_stats().await;
        assert_eq!(valid, 1);
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn test_verify() {
        let temp_dir = TempDir::new().unwrap();
        let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();
        let identity = storage.generate_identity(DIDScope::Private, None).unwrap();

        let resolver = DidResolver::local(storage);

        assert!(resolver.verify(&identity.did).await);
        assert!(!resolver.verify("did:peko:public:nonexistent").await);
    }
}
