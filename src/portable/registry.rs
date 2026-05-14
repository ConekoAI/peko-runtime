//! Local content-addressable registry for .agent packages
//!
//! Stores layers and manifests deduplicated by digest.
//! Merged from the former `src/image/registry.rs`.
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

use crate::portable::manifest::AgentManifest;
use crate::portable::types::{ImageDigest, LayerType};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

/// Local content-addressable store for .agent packages
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    root_path: PathBuf,
}

impl AgentRegistry {
    /// Create a new registry at the given path
    pub fn new(root_path: impl Into<PathBuf>) -> Self {
        Self {
            root_path: root_path.into(),
        }
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
        let tag_path = self.tags_dir().join(sanitize_tag(tag));
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
        let tag_path = tags_dir.join(sanitize_tag(tag));
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
                    .map(|n| n.to_string_lossy().to_string())
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

    /// Export a tagged manifest to a `.agent` package file.
    ///
    /// Reconstructs the package by reading the manifest and each layer from
    /// the registry, extracting layer tarballs, and creating a new `.agent`
    /// archive with `manifest.toml` and all layer files.
    ///
    /// # Errors
    ///
    /// Returns an error if the tag, manifest, or any layer is missing,
    /// or if archive creation fails.
    pub async fn export_package(
        &self,
        tag: &str,
        output_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<PathBuf> {
        let mut manifest = self.get_manifest_by_tag(tag).await?;
        let output_path = output_path.as_ref().to_path_buf();

        // First pass: collect all files from layers and compute checksums
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();

        if let Some(ref layers) = manifest.layers {
            if let Some(ref digest) = layers.config {
                Self::collect_layer_files(digest, LayerType::Config, self, &mut files).await?;
            }
            if let Some(ref digest) = layers.identity {
                Self::collect_layer_files(digest, LayerType::Identity, self, &mut files).await?;
            }
            if let Some(ref digest) = layers.skills {
                Self::collect_layer_files(digest, LayerType::Skills, self, &mut files).await?;
            }
            if let Some(ref digest) = layers.workspace {
                Self::collect_layer_files(digest, LayerType::Workspace, self, &mut files).await?;
            }
            if let Some(ref digest) = layers.sessions {
                Self::collect_layer_files(digest, LayerType::Sessions, self, &mut files).await?;
            }
            if let Some(ref digest) = layers.mcp {
                Self::collect_layer_files(digest, LayerType::Mcp, self, &mut files).await?;
            }
        }

        // Update manifest packaging metadata with actual files and checksums
        let mut packaging_files = Vec::new();
        let mut packaging_checksums = HashMap::new();
        for (path, content) in &files {
            packaging_files.push(path.clone());
            packaging_checksums.insert(path.clone(), AgentManifest::compute_checksum(content));
        }
        // Sort files for deterministic output
        packaging_files.sort();
        manifest.packaging.files = packaging_files;
        manifest.packaging.checksums = packaging_checksums;
        manifest.packaging.compression = "gzip".to_string();
        manifest.packaging.archive_format = "tar".to_string();

        // Create archive
        let tar_gz = std::fs::File::create(&output_path)?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add manifest.toml (updated with packaging metadata)
        let manifest_toml = manifest.to_toml()?;
        let manifest_bytes = manifest_toml.into_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_path("manifest.toml")?;
        header.set_size(manifest_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest_bytes.as_slice())?;

        // Add all collected files
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(&path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_slice())?;
        }

        tar.finish()?;
        Ok(output_path)
    }

    /// Collect files from a registry layer tarball into a map.
    async fn collect_layer_files(
        digest: &str,
        layer_type: LayerType,
        registry: &AgentRegistry,
        files: &mut HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        let layer_data = registry.get_layer(digest).await?;
        let tar_prefix = layer_type.dir_name();

        // The layer is itself a gzipped tarball
        let decoder = flate2::read::GzDecoder::new(layer_data.as_slice());
        let mut layer_tar = tar::Archive::new(decoder);

        for entry in layer_tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let path_str = path.to_string_lossy().to_string();

            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;

            let tar_path = format!("{tar_prefix}/{path_str}");
            files.insert(tar_path, content);
        }

        Ok(())
    }
}

/// Sanitize a tag for use as a filename
fn sanitize_tag(tag: &str) -> String {
    tag.replace(['/', ':', '\\', '<', '>', '|', '*', '?', '"'], "_")
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

        // List tags
        let tags = registry.list_tags().await.unwrap();
        assert!(tags.contains_key("test-agent_v1.0"));
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
}
