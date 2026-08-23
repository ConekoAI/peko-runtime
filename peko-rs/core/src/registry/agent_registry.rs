//! Local content-addressable registry for OCI-style layers and manifests
//!
//! Stores layers and manifests deduplicated by digest.
//! Previously lived at `crate::registry::packaging::registry` (issue #29).
//!
//! Storage layout:
//! ```text
//! ~/.peko/registry/
//! ├── layers/
//! │   └── sha256-abc123.../
//! │       └── layer.tar.gz
//! ├── manifests/
//! │   └── sha256-xyz789.../
//! │       └── manifest.toml
//! └── tags/
//!     └── my-agent_v1.0       # file contains manifest digest
//! ```

use crate::registry::packaging::manifest::AgentManifest;
use crate::registry::packaging::types::ImageDigest;
use std::path::PathBuf;

/// Maximum size of a single layer blob, in bytes (default: 4 GiB).
///
/// Layers larger than this are rejected at write time, bounding the
/// damage a single malicious or buggy push can do to local storage.
pub const DEFAULT_MAX_LAYER_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Maximum total size of the layer store, in bytes (default: 50 GiB).
///
/// New layers are rejected once storing them would push the store past
/// this quota. Bounds unbounded growth from repeated pulls/pushes.
pub const DEFAULT_MAX_STORE_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// Local content-addressable store for layers and manifests.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    root_path: PathBuf,
    /// Per-layer size cap; `None` disables the check.
    max_layer_bytes: Option<u64>,
    /// Total store quota; `None` disables the check.
    max_store_bytes: Option<u64>,
}

impl AgentRegistry {
    /// Create a new registry at the given path
    pub fn new(root_path: impl Into<PathBuf>) -> Self {
        Self {
            root_path: root_path.into(),
            max_layer_bytes: Some(DEFAULT_MAX_LAYER_BYTES),
            max_store_bytes: Some(DEFAULT_MAX_STORE_BYTES),
        }
    }

    /// Override the storage limits. Pass `None` to disable a given limit.
    #[must_use]
    pub fn with_storage_limits(
        mut self,
        max_layer_bytes: Option<u64>,
        max_store_bytes: Option<u64>,
    ) -> Self {
        self.max_layer_bytes = max_layer_bytes;
        self.max_store_bytes = max_store_bytes;
        self
    }

    /// Get the root path of the registry.
    #[must_use]
    pub fn root_path(&self) -> &std::path::Path {
        &self.root_path
    }

    /// Default registry path (~/.peko/registry)
    #[must_use]
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".peko")
            .join("registry")
    }

    /// Initialize registry directories.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub async fn init(&self) -> anyhow::Result<()> {
        // B6: `manifests_dir` + `tags_dir` were dropped — the `.agent`
        // manifest machinery (TOML store + tag pointer files) was
        // retired in favor of the OCI `RegistryManifest` JSON path
        // (`registry_manifests/<digest>/manifest.json` seeded by
        // `RegistryClient::store_manifest_locally`). Only `layers_dir`
        // remains.
        tokio::fs::create_dir_all(self.layers_dir()).await?;
        Ok(())
    }

    // --- Layer operations ---

    /// Store a layer (writes only if not already present).
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub async fn store_layer(&self, digest: &str, data: &[u8]) -> anyhow::Result<PathBuf> {
        let layer_dir = self.layer_dir(digest);
        let layer_path = layer_dir.join("layer.tar.gz");

        if layer_path.exists() {
            return Ok(layer_path);
        }

        // Enforce storage limits before writing a new blob. Already-present
        // layers short-circuit above, so re-storing never trips the quota.
        let incoming = data.len() as u64;
        if let Some(max) = self.max_layer_bytes {
            if incoming > max {
                anyhow::bail!(
                    "layer {digest} is {incoming} bytes, exceeds per-layer limit of {max} bytes"
                );
            }
        }
        if let Some(max) = self.max_store_bytes {
            let current = self.store_size_bytes().await?;
            if current.saturating_add(incoming) > max {
                anyhow::bail!(
                    "storing layer {digest} ({incoming} bytes) would exceed store quota \
                     of {max} bytes (current usage {current} bytes); run garbage collection \
                     or raise the limit"
                );
            }
        }

        tokio::fs::create_dir_all(&layer_dir).await?;
        tokio::fs::write(&layer_path, data).await?;

        Ok(layer_path)
    }

    /// Get layer bytes by digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the layer is not found or reading fails.
    pub async fn get_layer(&self, digest: &str) -> anyhow::Result<Vec<u8>> {
        let layer_path = self.layer_path(digest);
        if !layer_path.exists() {
            anyhow::bail!("Layer not found: {digest}");
        }
        Ok(tokio::fs::read(&layer_path).await?)
    }

    /// Check if a layer exists
    #[must_use]
    pub fn has_layer(&self, digest: &str) -> bool {
        self.layer_path(digest).exists()
    }

    /// Get the path to a layer file.
    #[must_use]
    pub fn layer_path(&self, digest: &str) -> PathBuf {
        let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.layers_dir()
            .join(format!("sha256-{digest}"))
            .join("layer.tar.gz")
    }

    // --- Directory helpers ---

    fn layers_dir(&self) -> PathBuf {
        self.root_path.join("layers")
    }

    fn layer_dir(&self, digest: &str) -> PathBuf {
        let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.layers_dir().join(format!("sha256-{digest}"))
    }

    // --- Storage accounting & garbage collection ---

    /// Total size of all stored layer blobs, in bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the layers directory cannot be read.
    pub async fn store_size_bytes(&self) -> anyhow::Result<u64> {
        let layers_dir = self.layers_dir();
        if !layers_dir.exists() {
            return Ok(0);
        }
        let mut total = 0u64;
        let mut entries = tokio::fs::read_dir(&layers_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let layer_file = entry.path().join("layer.tar.gz");
            if let Ok(meta) = tokio::fs::metadata(&layer_file).await {
                total = total.saturating_add(meta.len());
            }
        }
        Ok(total)
    }
}

/// Compute the OCI image digest (`sha256:<hex>`) for a serialized
/// manifest. Used by tests that need a deterministic digest to seed
/// the `RegistryManifest` JSON in `registry_manifests/<digest>/manifest.json`
/// without going through the retired `.agent` `store_manifest` path.
///
/// B6 cleanup: this helper replaces the former `store_manifest` +
/// `manifests_dir` + `tags_dir` + `set_tag` machinery that lived on
/// `AgentRegistry`. The OCI/RegistryManifest path (`client.rs::store_manifest_locally`)
/// is the live store.
#[doc(hidden)]
pub fn agent_manifest_digest(manifest: &AgentManifest) -> anyhow::Result<ImageDigest> {
    let toml = manifest.to_toml()?;
    Ok(ImageDigest::from_bytes(toml.as_bytes()))
}

/// Encode a tag into a collision-free, filesystem-safe filename.
///
/// Percent-encodes every byte outside the unreserved set `[A-Za-z0-9._-]`,
/// plus a leading `.`. This makes the mapping a bijection: distinct tags
/// always map to distinct filenames. The previous `replace`-based scheme
/// collapsed `/`, `:`, `\`, etc. all to `_`, so `a/b`, `a:b`, and `a_b`
/// silently aliased to the same pointer file. Encoding a leading `.`
/// guarantees the result can never be `.` or `..` (path traversal).
///
/// B6 cleanup: still used by `RegistryClient::store_manifest_locally`
/// (the live OCI path) to safely store `manifest.r#ref` filenames in
/// `registry_manifests/`. The `.agent`-machinery callers were dropped,
/// but the live consumer keeps the helper.
pub(crate) fn encode_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for (i, &b) in tag.as_bytes().iter().enumerate() {
        let unreserved = matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.');
        if unreserved && !(b == b'.' && i == 0) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decode a filename produced by [`encode_tag`] back to the original tag.
///
/// B6 cleanup: see `encode_tag`. Still used by `RegistryClient`.
pub(crate) fn decode_tag(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(v) = u8::from_str_radix(&name[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_init() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        assert!(registry.layers_dir().exists());
    }

    #[tokio::test]
    async fn test_layer_store_and_get() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        let data = b"test layer content";
        let digest = compute_digest(data);

        // Store
        let path = registry.store_layer(&digest, data).await.unwrap();
        assert!(path.exists());

        // Get
        let retrieved = registry.get_layer(&digest).await.unwrap();
        assert_eq!(retrieved, data);

        // Has
        assert!(registry.has_layer(&digest));
        assert!(
            !registry.has_layer("sha256:nonexistent0000000000000000000000000000000000000000000000")
        );
    }

    fn compute_digest(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }

    #[tokio::test]
    async fn test_store_layer_rejects_oversized_layer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path()).with_storage_limits(Some(8), None);

        let small = registry.store_layer("sha256:aaa", b"1234").await;
        assert!(small.is_ok());

        let big = registry.store_layer("sha256:bbb", b"123456789").await;
        assert!(big.is_err(), "layer over per-layer limit must be rejected");
    }

    #[tokio::test]
    async fn test_store_layer_enforces_store_quota() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path()).with_storage_limits(None, Some(10));

        registry.store_layer("sha256:aaa", b"12345").await.unwrap();
        registry.store_layer("sha256:bbb", b"12345").await.unwrap();
        // Store is now full (10 bytes); the next distinct layer is rejected.
        let over = registry.store_layer("sha256:ccc", b"1").await;
        assert!(over.is_err(), "quota overflow must be rejected");

        // Re-storing an existing layer short-circuits and never trips quota.
        registry.store_layer("sha256:aaa", b"12345").await.unwrap();
    }
}
