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

use crate::registry::manifest::RegistryManifest;
use crate::registry::packaging::manifest::AgentManifest;
use crate::registry::packaging::types::ImageDigest;
use std::collections::{HashMap, HashSet};
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

/// Statistics returned by [`AgentRegistry::gc`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcStats {
    /// Number of orphaned layer blobs removed.
    pub layers_removed: usize,
    /// Total bytes reclaimed.
    pub bytes_reclaimed: u64,
}

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
        tokio::fs::create_dir_all(self.layers_dir()).await?;
        tokio::fs::create_dir_all(self.manifests_dir()).await?;
        tokio::fs::create_dir_all(self.tags_dir()).await?;
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

    // --- Manifest operations ---

    /// Store a manifest and update the tag.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or writing fails.
    pub async fn store_manifest(
        &self,
        manifest: &AgentManifest,
        tag: Option<&str>,
    ) -> anyhow::Result<ImageDigest> {
        let manifest_toml = manifest.to_toml()?;
        let digest = ImageDigest::from_bytes(manifest_toml.as_bytes());

        let manifest_dir = self.manifests_dir().join(digest.dir_name());
        tokio::fs::create_dir_all(&manifest_dir).await?;

        let manifest_path = manifest_dir.join("manifest.toml");
        tokio::fs::write(&manifest_path, manifest_toml).await?;

        // Update tag if provided
        if let Some(tag) = tag {
            self.set_tag(tag, digest.as_str()).await?;
        }

        Ok(digest)
    }

    /// Get manifest by digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest is not found or parsing fails.
    pub async fn get_manifest_by_digest(&self, digest: &str) -> anyhow::Result<AgentManifest> {
        let digest = ImageDigest::new(digest)?;
        let manifest_dir = self.manifests_dir().join(digest.dir_name());
        let manifest_path = manifest_dir.join("manifest.toml");

        if !manifest_path.exists() {
            anyhow::bail!("Manifest not found: {digest}");
        }

        let toml_str = tokio::fs::read_to_string(&manifest_path).await?;
        AgentManifest::from_toml(&toml_str)
    }

    /// Get manifest by tag.
    ///
    /// # Errors
    ///
    /// Returns an error if the tag is not found or manifest loading fails.
    pub async fn get_manifest_by_tag(&self, tag: &str) -> anyhow::Result<AgentManifest> {
        let digest = self.resolve_tag(tag).await?;
        self.get_manifest_by_digest(&digest).await
    }

    /// Resolve a tag to its digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the tag is not found.
    pub async fn resolve_tag(&self, tag: &str) -> anyhow::Result<String> {
        let tag_path = self.tags_dir().join(encode_tag(tag));
        if !tag_path.exists() {
            anyhow::bail!("Tag not found: {tag}");
        }
        let digest = tokio::fs::read_to_string(&tag_path).await?;
        Ok(digest.trim().to_string())
    }

    /// Set a tag to point to a digest.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub async fn set_tag(&self, tag: &str, digest: &str) -> anyhow::Result<()> {
        let tags_dir = self.tags_dir();
        tokio::fs::create_dir_all(&tags_dir).await?;
        let tag_path = tags_dir.join(encode_tag(tag));
        tokio::fs::write(&tag_path, digest).await?;
        Ok(())
    }

    /// List all tags.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the tags directory fails.
    pub async fn list_tags(&self) -> anyhow::Result<HashMap<String, String>> {
        let mut tags = HashMap::new();
        let tags_dir = self.tags_dir();

        if !tags_dir.exists() {
            return Ok(tags);
        }

        let mut entries = tokio::fs::read_dir(&tags_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .map(|n| decode_tag(&n.to_string_lossy()))
                    .unwrap_or_default();
                let digest = tokio::fs::read_to_string(&path).await?;
                tags.insert(name, digest.trim().to_string());
            }
        }

        Ok(tags)
    }

    // --- Directory helpers ---

    fn layers_dir(&self) -> PathBuf {
        self.root_path.join("layers")
    }

    fn manifests_dir(&self) -> PathBuf {
        self.root_path.join("manifests")
    }

    fn tags_dir(&self) -> PathBuf {
        self.root_path.join("tags")
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

    /// Collect the bare-hex digests referenced by every stored manifest.
    ///
    /// Reconciles against BOTH manifest stores: the `.agent` manifests
    /// (`manifests/*/manifest.toml`) and the OCI registry manifests
    /// (`registry_manifests/*/manifest.json`). If any manifest fails to
    /// parse, this returns an error so callers never delete a blob that
    /// might still be referenced.
    async fn referenced_digests(&self) -> anyhow::Result<HashSet<String>> {
        let mut set = HashSet::new();

        let manifests_dir = self.manifests_dir();
        if manifests_dir.exists() {
            let mut entries = tokio::fs::read_dir(&manifests_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path().join("manifest.toml");
                if !path.exists() {
                    continue;
                }
                let toml_str = tokio::fs::read_to_string(&path).await?;
                let manifest = AgentManifest::from_toml(&toml_str).map_err(|e| {
                    anyhow::anyhow!("gc aborted: failed to parse {}: {e}", path.display())
                })?;
                if let Some(layers) = &manifest.layers {
                    for digest in [
                        &layers.config,
                        &layers.identity,
                        &layers.skills,
                        &layers.workspace,
                        &layers.sessions,
                        &layers.mcp,
                        &layers.extensions,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        set.insert(digest_key(digest));
                    }
                }
            }
        }

        let registry_manifests_dir = self.root_path.join("registry_manifests");
        if registry_manifests_dir.exists() {
            let mut entries = tokio::fs::read_dir(&registry_manifests_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path().join("manifest.json");
                if !path.exists() {
                    continue;
                }
                let json = tokio::fs::read_to_string(&path).await?;
                let manifest = RegistryManifest::from_json(&json).map_err(|e| {
                    anyhow::anyhow!("gc aborted: failed to parse {}: {e}", path.display())
                })?;
                if !manifest.config.digest.is_empty() {
                    set.insert(digest_key(&manifest.config.digest));
                }
                for layer in &manifest.layers {
                    set.insert(digest_key(&layer.digest));
                }
            }
        }

        Ok(set)
    }

    /// Remove layer blobs not referenced by any stored manifest.
    ///
    /// This is conservative: it reconciles against both manifest stores
    /// and aborts (deleting nothing) if any manifest cannot be parsed, so
    /// it never removes a blob it cannot prove is unreferenced.
    ///
    /// # Errors
    ///
    /// Returns an error if a manifest cannot be parsed or the filesystem
    /// cannot be read/modified.
    pub async fn gc(&self) -> anyhow::Result<GcStats> {
        let referenced = self.referenced_digests().await?;
        let mut stats = GcStats::default();

        let layers_dir = self.layers_dir();
        if !layers_dir.exists() {
            return Ok(stats);
        }

        let mut entries = tokio::fs::read_dir(&layers_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let dir = entry.path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let key = name.strip_prefix("sha256-").unwrap_or(name).to_string();
            if referenced.contains(&key) {
                continue;
            }
            let size = tokio::fs::metadata(dir.join("layer.tar.gz"))
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            tokio::fs::remove_dir_all(&dir).await?;
            stats.layers_removed += 1;
            stats.bytes_reclaimed = stats.bytes_reclaimed.saturating_add(size);
        }

        Ok(stats)
    }
}

/// Normalize a digest to its bare hex form, stripping a `sha256:` or
/// `sha256-` prefix so manifest references and on-disk layer directory
/// names compare equal.
fn digest_key(digest: &str) -> String {
    digest
        .strip_prefix("sha256:")
        .or_else(|| digest.strip_prefix("sha256-"))
        .unwrap_or(digest)
        .to_string()
}

/// Encode a tag into a collision-free, filesystem-safe filename.
///
/// Percent-encodes every byte outside the unreserved set `[A-Za-z0-9._-]`,
/// plus a leading `.`. This makes the mapping a bijection: distinct tags
/// always map to distinct filenames. The previous `replace`-based scheme
/// collapsed `/`, `:`, `\`, etc. all to `_`, so `a/b`, `a:b`, and `a_b`
/// silently aliased to the same pointer file. Encoding a leading `.`
/// guarantees the result can never be `.` or `..` (path traversal).
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
    use crate::registry::packaging::manifest::AgentLayers;

    #[tokio::test]
    async fn test_registry_init() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        assert!(registry.layers_dir().exists());
        assert!(registry.manifests_dir().exists());
        assert!(registry.tags_dir().exists());
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

    #[tokio::test]
    async fn test_manifest_store_and_get() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        let manifest = AgentManifest::new("test-agent", "1.0.0", "did:peko:test");

        // Store with tag
        let digest = registry
            .store_manifest(&manifest, Some("test-agent:v1.0"))
            .await
            .unwrap();

        // Get by digest
        let by_digest = registry
            .get_manifest_by_digest(digest.as_str())
            .await
            .unwrap();
        assert_eq!(by_digest.agent.name, "test-agent");

        // Get by tag
        let by_tag = registry
            .get_manifest_by_tag("test-agent:v1.0")
            .await
            .unwrap();
        assert_eq!(by_tag.agent.name, "test-agent");

        // List tags — the tag round-trips losslessly through encoding,
        // so the original (colon-bearing) tag name is recovered.
        let tags = registry.list_tags().await.unwrap();
        assert!(tags.contains_key("test-agent:v1.0"));
    }

    #[tokio::test]
    async fn test_tag_resolution() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        registry.set_tag("my-tag", "sha256:abc123").await.unwrap();
        let digest = registry.resolve_tag("my-tag").await.unwrap();
        assert_eq!(digest, "sha256:abc123");

        assert!(registry.resolve_tag("nonexistent").await.is_err());
    }

    fn compute_digest(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }

    #[test]
    fn test_encode_tag_is_collision_free_and_reversible() {
        // Inputs that the old `replace`-based scheme collapsed to "a_b".
        let inputs = ["a/b", "a:b", "a_b", "a\\b", "a*b"];
        let mut seen = std::collections::HashSet::new();
        for tag in inputs {
            let encoded = encode_tag(tag);
            assert!(seen.insert(encoded.clone()), "collision on {tag}");
            assert_eq!(decode_tag(&encoded), tag, "roundtrip failed for {tag}");
        }
    }

    #[test]
    fn test_encode_tag_blocks_path_traversal() {
        // A leading dot is always encoded, so "." / ".." can never reach
        // the filesystem as a directory-traversal component.
        assert_ne!(encode_tag("."), ".");
        assert_ne!(encode_tag(".."), "..");
        assert!(!encode_tag("../etc").contains('/'));
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

    #[tokio::test]
    async fn test_gc_removes_unreferenced_layers_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        // A manifest that references one layer via its config digest.
        let referenced = registry
            .store_layer("sha256:ref", b"referenced-bytes")
            .await
            .unwrap();
        let _ = referenced;
        let mut manifest = AgentManifest::new("a", "1.0.0", "did:peko:a");
        manifest.layers = Some(AgentLayers {
            config: Some("sha256:ref".to_string()),
            ..AgentLayers::default()
        });
        registry.store_manifest(&manifest, None).await.unwrap();

        // An orphan layer referenced by nothing.
        registry
            .store_layer("sha256:orphan", b"orphan-bytes")
            .await
            .unwrap();

        let stats = registry.gc().await.unwrap();
        assert_eq!(stats.layers_removed, 1);
        assert!(registry.has_layer("sha256:ref"));
        assert!(!registry.has_layer("sha256:orphan"));
    }
}
