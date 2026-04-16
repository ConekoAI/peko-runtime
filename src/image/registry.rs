//! Image Registry
//!
//! Content-addressable storage for images and layers.
//! Implements the registry layout from `UNIFIED_ARCH` §2.3

use super::manifest::{ImageDigest, ImageManifest};
use super::ImageRef;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

/// Configuration for the image registry
#[derive(Debug, Clone)]
pub struct RegistryConfig {
    /// Root path for the registry
    pub root_path: PathBuf,
    /// Default registry for pulls
    pub default_registry: String,
}

impl RegistryConfig {
    /// Create new registry config
    pub fn new(root_path: impl Into<PathBuf>) -> Self {
        Self {
            root_path: root_path.into(),
            default_registry: "pekohub.com".to_string(),
        }
    }

    /// Set the default registry
    pub fn with_default_registry(mut self, registry: impl Into<String>) -> Self {
        self.default_registry = registry.into();
        self
    }
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            root_path: PathBuf::from(".pekobot/registry"),
            default_registry: "pekohub.com".to_string(),
        }
    }
}

/// Content-addressable image registry
pub struct ImageRegistry {
    config: RegistryConfig,
    /// Tag to digest mapping (in-memory cache)
    tags: HashMap<String, String>,
}

impl ImageRegistry {
    /// Create a new registry
    #[must_use] 
    pub fn new(config: RegistryConfig) -> Self {
        Self {
            config,
            tags: HashMap::new(),
        }
    }

    /// Initialize registry directories
    pub async fn init(&mut self) -> anyhow::Result<()> {
        let images_dir = self.images_dir();
        let layers_dir = self.layers_dir();
        let tags_dir = self.tags_dir();

        fs::create_dir_all(&images_dir).await?;
        fs::create_dir_all(&layers_dir).await?;
        fs::create_dir_all(&tags_dir).await?;

        // Load existing tags
        self.load_tags().await?;

        Ok(())
    }

    /// Store an image manifest
    pub async fn store_manifest(
        &mut self,
        manifest: &ImageManifest,
    ) -> anyhow::Result<ImageDigest> {
        let digest = ImageDigest::new(&manifest.digest)?;
        let image_dir = self.image_dir(&digest);

        fs::create_dir_all(&image_dir).await?;

        let manifest_path = image_dir.join("manifest.json");
        let json = manifest.to_json()?;
        fs::write(&manifest_path, json).await?;

        // Update tag if reference is set
        if !manifest.r#ref.is_empty() {
            self.set_tag(&manifest.r#ref, &digest).await?;
        }

        Ok(digest)
    }

    /// Get a manifest by digest
    pub async fn get_manifest_by_digest(
        &self,
        digest: &ImageDigest,
    ) -> anyhow::Result<Option<ImageManifest>> {
        let manifest_path = self.image_dir(digest).join("manifest.json");

        if !manifest_path.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&manifest_path).await?;
        let manifest = ImageManifest::from_json(&json)?;
        Ok(Some(manifest))
    }

    /// Get a manifest by reference (tag)
    pub async fn get_manifest_by_ref(
        &self,
        reference: &str,
    ) -> anyhow::Result<Option<ImageManifest>> {
        // Check if it's a digest
        if reference.starts_with("sha256:") {
            let digest = ImageDigest::new(reference)?;
            return self.get_manifest_by_digest(&digest).await;
        }

        // Look up tag
        if let Some(digest_str) = self.tags.get(reference) {
            let digest = ImageDigest::new(digest_str)?;
            return self.get_manifest_by_digest(&digest).await;
        }

        // Try loading from tags directory
        let tag_path = self.tags_dir().join(sanitize_tag(reference));
        if tag_path.exists() {
            let digest_str = fs::read_to_string(&tag_path).await?;
            let digest = ImageDigest::new(digest_str.trim())?;
            return self.get_manifest_by_digest(&digest).await;
        }

        Ok(None)
    }

    /// Get a manifest from an `ImageRef`
    pub async fn resolve(&self, image_ref: &ImageRef) -> anyhow::Result<Option<ImageManifest>> {
        match image_ref {
            ImageRef::Digest(digest) => self.get_manifest_by_digest(digest).await,
            ImageRef::LocalTag { name, tag } => {
                let reference = format!("{name}:{tag}");
                self.get_manifest_by_ref(&reference).await
            }
            ImageRef::RegistryRef { host, path, tag } => {
                let reference = format!("{host}/{path}:{tag}");
                self.get_manifest_by_ref(&reference).await
            }
            ImageRef::Path(_p) => {
                // For path references, we need to build/load the manifest
                // This is handled by the builder
                Ok(None)
            }
        }
    }

    /// Store a layer
    pub async fn store_layer(&self, digest: &ImageDigest, data: &[u8]) -> anyhow::Result<PathBuf> {
        let layer_path = self.layer_path(digest);

        // Check if layer already exists (content-addressable dedup)
        if layer_path.exists() {
            return Ok(layer_path);
        }

        fs::write(&layer_path, data).await?;
        Ok(layer_path)
    }

    /// Get layer data
    pub async fn get_layer(&self, digest: &ImageDigest) -> anyhow::Result<Option<Vec<u8>>> {
        let layer_path = self.layer_path(digest);

        if !layer_path.exists() {
            return Ok(None);
        }

        let data = fs::read(&layer_path).await?;
        Ok(Some(data))
    }

    /// Check if a layer exists
    #[must_use] 
    pub fn has_layer(&self, digest: &ImageDigest) -> bool {
        self.layer_path(digest).exists()
    }

    /// Set a tag for an image
    pub async fn set_tag(&mut self, reference: &str, digest: &ImageDigest) -> anyhow::Result<()> {
        // Update in-memory cache
        self.tags.insert(reference.to_string(), digest.to_string());

        // Persist to disk
        let tag_path = self.tags_dir().join(sanitize_tag(reference));
        fs::write(&tag_path, digest.as_str()).await?;

        Ok(())
    }

    /// Remove a tag
    pub async fn remove_tag(&mut self, reference: &str) -> anyhow::Result<()> {
        self.tags.remove(reference);

        let tag_path = self.tags_dir().join(sanitize_tag(reference));
        if tag_path.exists() {
            fs::remove_file(&tag_path).await?;
        }

        Ok(())
    }

    /// List all local images (by manifest)
    pub async fn list_images(&self) -> anyhow::Result<Vec<ImageManifest>> {
        let images_dir = self.images_dir();
        let mut manifests = Vec::new();

        if !images_dir.exists() {
            return Ok(manifests);
        }

        let mut entries = fs::read_dir(&images_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let manifest_path = entry.path().join("manifest.json");
            if manifest_path.exists() {
                if let Ok(json) = fs::read_to_string(&manifest_path).await {
                    if let Ok(manifest) = ImageManifest::from_json(&json) {
                        manifests.push(manifest);
                    }
                }
            }
        }

        // Sort by creation time (newest first)
        manifests.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(manifests)
    }

    /// Delete an image by digest
    pub async fn delete_image(&self, digest: &ImageDigest) -> anyhow::Result<bool> {
        let image_dir = self.image_dir(digest);

        if !image_dir.exists() {
            return Ok(false);
        }

        // Remove manifest directory
        fs::remove_dir_all(&image_dir).await?;

        // Note: We don't delete layers here as they might be shared
        // A separate GC process should clean up unreferenced layers

        Ok(true)
    }

    /// Get layer path
    fn layer_path(&self, digest: &ImageDigest) -> PathBuf {
        self.layers_dir()
            .join(format!("{}.tar.gz", digest.dir_name()))
    }

    /// Get image directory for a digest
    fn image_dir(&self, digest: &ImageDigest) -> PathBuf {
        self.images_dir().join(digest.dir_name())
    }

    /// Get images directory
    fn images_dir(&self) -> PathBuf {
        self.config.root_path.join("images")
    }

    /// Get layers directory
    fn layers_dir(&self) -> PathBuf {
        self.config.root_path.join("layers")
    }

    /// Get tags directory
    fn tags_dir(&self) -> PathBuf {
        self.config.root_path.join("tags")
    }

    /// Load tags from disk
    async fn load_tags(&mut self) -> anyhow::Result<()> {
        let tags_dir = self.tags_dir();

        if !tags_dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(&tags_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                if let (Some(name), Ok(content)) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    fs::read_to_string(&path).await,
                ) {
                    let tag = unsanitize_tag(name);
                    self.tags.insert(tag, content.trim().to_string());
                }
            }
        }

        Ok(())
    }

    /// Garbage collect unreferenced layers
    pub async fn gc(&self) -> anyhow::Result<usize> {
        let mut referenced_digests: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Collect all digests referenced by manifests
        let images = self.list_images().await?;
        for manifest in images {
            referenced_digests.insert(manifest.digest);
            for layer in &manifest.layers {
                referenced_digests.insert(layer.digest.clone());
            }
        }

        // Find unreferenced layers
        let mut deleted = 0;
        let layers_dir = self.layers_dir();

        if layers_dir.exists() {
            let mut entries = fs::read_dir(&layers_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    let digest = format!("sha256:{}", name.replace("sha256-", ""));
                    if !referenced_digests.contains(&digest) {
                        fs::remove_file(&path).await?;
                        deleted += 1;
                    }
                }
            }
        }

        Ok(deleted)
    }
}

/// Sanitize a tag for use as a filename
fn sanitize_tag(tag: &str) -> String {
    // Replace characters that are invalid in filenames
    tag.replace(['/', ':', '\\', '<', '>', '|', '*', '?', '"'], "_")
}

/// Unsanitize a tag from filename
fn unsanitize_tag(filename: &str) -> String {
    // For now, just replace first underscore back to colon for common patterns
    // This is a best-effort reverse
    if let Some(pos) = filename.find('_') {
        let mut result = filename.to_string();
        result.replace_range(pos..=pos, ":");
        result
    } else {
        filename.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_registry_init() {
        let dir = TempDir::new().unwrap();
        let config = RegistryConfig::new(dir.path());
        let mut registry = ImageRegistry::new(config);

        registry.init().await.unwrap();

        assert!(dir.path().join("images").exists());
        assert!(dir.path().join("layers").exists());
        assert!(dir.path().join("tags").exists());
    }

    #[tokio::test]
    async fn test_store_and_get_manifest() {
        let dir = TempDir::new().unwrap();
        let config = RegistryConfig::new(dir.path());
        let mut registry = ImageRegistry::new(config);
        registry.init().await.unwrap();

        let hex = "a".repeat(64);
        let manifest = ImageManifest::new("test", "1.0.0")
            .with_digest(format!("sha256:{hex}"))
            .with_ref("test:v1.0");

        let digest = registry.store_manifest(&manifest).await.unwrap();

        let retrieved = registry
            .get_manifest_by_digest(&digest)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(retrieved.name, "test");
        assert_eq!(retrieved.version, "1.0.0");
    }

    #[tokio::test]
    async fn test_tag_resolution() {
        let dir = TempDir::new().unwrap();
        let config = RegistryConfig::new(dir.path());
        let mut registry = ImageRegistry::new(config);
        registry.init().await.unwrap();

        let hex = "b".repeat(64);
        let manifest = ImageManifest::new("test", "1.0.0")
            .with_digest(format!("sha256:{hex}"))
            .with_ref("test:v1.0");

        registry.store_manifest(&manifest).await.unwrap();

        let retrieved = registry
            .get_manifest_by_ref("test:v1.0")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(retrieved.name, "test");
    }

    #[tokio::test]
    async fn test_list_images() {
        let dir = TempDir::new().unwrap();
        let config = RegistryConfig::new(dir.path());
        let mut registry = ImageRegistry::new(config);
        registry.init().await.unwrap();

        let hex1 = "c".repeat(64);
        let hex2 = "d".repeat(64);
        let manifest1 =
            ImageManifest::new("agent1", "1.0.0").with_digest(format!("sha256:{hex1}"));
        let manifest2 =
            ImageManifest::new("agent2", "2.0.0").with_digest(format!("sha256:{hex2}"));

        registry.store_manifest(&manifest1).await.unwrap();
        registry.store_manifest(&manifest2).await.unwrap();

        let images = registry.list_images().await.unwrap();
        assert_eq!(images.len(), 2);
    }

    #[tokio::test]
    async fn test_layer_deduplication() {
        let dir = TempDir::new().unwrap();
        let config = RegistryConfig::new(dir.path());
        let mut registry = ImageRegistry::new(config);
        registry.init().await.unwrap();

        let data = b"test layer data";
        let digest = ImageDigest::from_bytes(data);

        // Store layer twice
        let path1 = registry.store_layer(&digest, data).await.unwrap();
        let path2 = registry.store_layer(&digest, data).await.unwrap();

        // Should return same path
        assert_eq!(path1, path2);

        // Should only have one file
        let mut layers = fs::read_dir(registry.layers_dir()).await.unwrap();
        let mut count = 0;
        while layers.next_entry().await.unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn test_sanitize_tag() {
        assert_eq!(sanitize_tag("test:v1.0"), "test_v1.0");
        assert_eq!(sanitize_tag("a/b/c"), "a_b_c");
    }
}
