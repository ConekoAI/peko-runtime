//! Registry Client
//!
//! HTTP client for pushing and pulling images from remote registries.
//! Implements OCI-inspired distribution protocol.

use crate::image::manifest::{ImageDigest, ImageManifest, Layer};
use crate::registry::config::{RegistryConfig, RegistrySource, ResolvedAuth};
use reqwest::Client;
use serde::Serialize;
use std::path::PathBuf;

/// Registry client for push/pull operations
#[derive(Debug, Clone)]
pub struct RegistryClient {
    http: Client,
    config: RegistryConfig,
    registry_path: PathBuf,
}

/// Progress events during pull/push operations
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Resolving the image reference
    Resolving { r#ref: String },
    /// Pulling a layer
    Pulling {
        layer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_received: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
    },
    /// Extracting a layer
    Extracting { layer: String },
    /// Pushing a layer
    Pushing {
        layer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_sent: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
    },
    /// Verifying layer digest
    Verifying { layer: String },
    /// Operation complete
    Done { manifest: ImageManifest },
    /// Error occurred
    Error { code: String, message: String },
}

/// Parsed registry reference
#[derive(Debug, Clone)]
pub struct RegistryRef {
    /// Host (e.g., "pekohub.com")
    pub host: String,
    /// Path (e.g., "agents/researcher")
    pub path: String,
    /// Tag (e.g., "v2.5")
    pub tag: String,
}

impl RegistryRef {
    /// Parse a registry reference string
    /// Format: "host/path/to/image:tag" or "host/path/to/image" (defaults to "latest")
    pub fn parse(r#ref: &str) -> anyhow::Result<Self> {
        // Split by ':' to separate tag
        let (ref_part, tag) = r#ref.rsplit_once(':').unwrap_or((r#ref, "latest"));

        // Split by '/' to separate host from path
        let parts: Vec<&str> = ref_part.split('/').collect();
        if parts.len() < 2 {
            return Err(anyhow::anyhow!(
                "Invalid registry reference: must contain host and path"
            ));
        }

        let host = parts[0].to_string();
        let path = parts[1..].join("/");

        Ok(Self {
            host,
            path,
            tag: tag.to_string(),
        })
    }

    /// Get the full reference string
    #[must_use]
    pub fn full_ref(&self) -> String {
        format!("{}/{}:{}", self.host, self.path, self.tag)
    }

    /// Get the path without tag (for API calls)
    #[must_use]
    pub fn repository(&self) -> String {
        format!("{}/{}", self.host, self.path)
    }
}

impl RegistryClient {
    /// Create a new registry client
    pub fn new(config: RegistryConfig, registry_path: impl Into<PathBuf>) -> Self {
        Self {
            http: Client::new(),
            config,
            registry_path: registry_path.into(),
        }
    }

    /// Pull an image from a registry
    pub async fn pull<F>(&self, r#ref: &str, mut progress: F) -> anyhow::Result<ImageManifest>
    where
        F: FnMut(ProgressEvent),
    {
        progress(ProgressEvent::Resolving {
            r#ref: r#ref.to_string(),
        });

        // Parse the reference
        let reg_ref = RegistryRef::parse(r#ref)?;

        // Get the registry source
        let source = self
            .config
            .resolve_source(&reg_ref.host)
            .ok_or_else(|| anyhow::anyhow!("No registry configured for host: {}", reg_ref.host))?;

        // Resolve authentication
        let auth = self.resolve_auth(source)?;

        // Get manifest from registry
        let manifest = self
            .fetch_manifest(&reg_ref, source, &auth)
            .await
            .map_err(|e| {
                progress(ProgressEvent::Error {
                    code: "manifest_fetch_failed".to_string(),
                    message: e.to_string(),
                });
                e
            })?;

        // Pull each layer
        for layer in &manifest.layers {
            self.pull_layer(&reg_ref, source, &auth, layer, &mut progress)
                .await
                .map_err(|e| {
                    progress(ProgressEvent::Error {
                        code: "layer_pull_failed".to_string(),
                        message: format!("Failed to pull layer {}: {}", layer.digest, e),
                    });
                    e
                })?;
        }

        // Store the manifest locally
        self.store_manifest_locally(&manifest).await?;

        progress(ProgressEvent::Done {
            manifest: manifest.clone(),
        });

        Ok(manifest)
    }

    /// Push an image to a registry
    pub async fn push<F>(
        &self,
        local_digest: &ImageDigest,
        remote_ref: &str,
        mut progress: F,
    ) -> anyhow::Result<ImageManifest>
    where
        F: FnMut(ProgressEvent),
    {
        // Load the local manifest
        let manifest = self.load_manifest_local(local_digest).await?;

        // Parse the remote reference
        let reg_ref = RegistryRef::parse(remote_ref)?;

        // Get the registry source
        let source = self
            .config
            .resolve_source(&reg_ref.host)
            .ok_or_else(|| anyhow::anyhow!("No registry configured for host: {}", reg_ref.host))?;

        // Resolve authentication
        let auth = self.resolve_auth(source)?;

        // Check which layers already exist on the registry (mount check)
        let existing_layers = self.check_existing_layers(&reg_ref, source, &auth)?;

        // Push missing layers
        for layer in &manifest.layers {
            if existing_layers.contains(&layer.digest) {
                progress(ProgressEvent::Pushing {
                    layer: layer.digest.clone(),
                    bytes_sent: Some(layer.size_bytes),
                    bytes_total: Some(layer.size_bytes),
                });
                continue; // Layer already exists
            }

            self.push_layer(&reg_ref, source, &auth, layer, &mut progress)
                .await
                .map_err(|e| {
                    progress(ProgressEvent::Error {
                        code: "layer_push_failed".to_string(),
                        message: format!("Failed to push layer {}: {}", layer.digest, e),
                    });
                    e
                })?;
        }

        // Push manifest
        self.push_manifest(&reg_ref, source, &auth, &manifest)
            .await?;

        progress(ProgressEvent::Done {
            manifest: manifest.clone(),
        });

        Ok(manifest)
    }

    /// Resolve authentication for a registry source
    fn resolve_auth(&self, source: &RegistrySource) -> anyhow::Result<ResolvedAuth> {
        match &source.auth {
            Some(auth) => auth.resolve(),
            None => Ok(ResolvedAuth::None),
        }
    }

    /// Fetch manifest from registry
    async fn fetch_manifest(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
    ) -> anyhow::Result<ImageManifest> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            source.url, reg_ref.path, reg_ref.tag
        );

        let req = self.http.get(&url);
        let req = auth.apply(req);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch manifest: HTTP {}",
                response.status()
            ));
        }

        let json = response.text().await?;
        let manifest = ImageManifest::from_json(&json)?;

        Ok(manifest)
    }

    /// Pull a single layer from registry
    async fn pull_layer<F>(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
        layer: &Layer,
        progress: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(ProgressEvent),
    {
        let layer_path = self.layer_path(&layer.digest);

        // Check if layer already exists locally
        if layer_path.exists() {
            progress(ProgressEvent::Pulling {
                layer: layer.digest.clone(),
                bytes_received: Some(layer.size_bytes),
                bytes_total: Some(layer.size_bytes),
            });
            return Ok(());
        }

        progress(ProgressEvent::Pulling {
            layer: layer.digest.clone(),
            bytes_received: Some(0),
            bytes_total: Some(layer.size_bytes),
        });

        // Fetch layer from registry
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            source.url, reg_ref.path, layer.digest
        );

        let req = self.http.get(&url);
        let req = auth.apply(req);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch layer: HTTP {}",
                response.status()
            ));
        }

        let data = response.bytes().await?;

        progress(ProgressEvent::Pulling {
            layer: layer.digest.clone(),
            bytes_received: Some(data.len() as u64),
            bytes_total: Some(layer.size_bytes),
        });

        // Verify digest
        progress(ProgressEvent::Verifying {
            layer: layer.digest.clone(),
        });

        let computed_digest = ImageDigest::from_bytes(&data);
        if computed_digest.as_str() != layer.digest {
            return Err(anyhow::anyhow!(
                "Layer digest mismatch: expected {}, got {}",
                layer.digest,
                computed_digest.as_str()
            ));
        }

        // Store layer
        tokio::fs::create_dir_all(layer_path.parent().unwrap()).await?;
        tokio::fs::write(&layer_path, &data).await?;

        progress(ProgressEvent::Extracting {
            layer: layer.digest.clone(),
        });

        Ok(())
    }

    /// Push a single layer to registry
    async fn push_layer<F>(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
        layer: &Layer,
        progress: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(ProgressEvent),
    {
        let layer_path = self.layer_path(&layer.digest);

        if !layer_path.exists() {
            return Err(anyhow::anyhow!("Layer not found locally: {}", layer.digest));
        }

        let data = tokio::fs::read(&layer_path).await?;

        progress(ProgressEvent::Pushing {
            layer: layer.digest.clone(),
            bytes_sent: Some(0),
            bytes_total: Some(data.len() as u64),
        });

        // Initiate upload
        let url = format!("https://{}/v2/{}/blobs/uploads/", source.url, reg_ref.path);
        let req = self.http.post(&url);
        let req = auth.apply(req);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to initiate layer upload: HTTP {}",
                response.status()
            ));
        }

        // Get upload URL from Location header
        let upload_url = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(std::string::String::from)
            .unwrap_or_else(|| {
                // Fallback to standard URL
                format!(
                    "https://{}/v2/{}/blobs/uploads/{}",
                    source.url,
                    reg_ref.path,
                    uuid::Uuid::new_v4()
                )
            });

        // Upload layer data
        let req = self.http.put(&upload_url);
        let req = auth.apply(req);
        let req = req.header("Content-Type", "application/octet-stream");
        let req = req.body(data.clone());

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to upload layer: HTTP {}",
                response.status()
            ));
        }

        progress(ProgressEvent::Pushing {
            layer: layer.digest.clone(),
            bytes_sent: Some(data.len() as u64),
            bytes_total: Some(data.len() as u64),
        });

        Ok(())
    }

    /// Push manifest to registry
    async fn push_manifest(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
        manifest: &ImageManifest,
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            source.url, reg_ref.path, reg_ref.tag
        );

        let json = manifest.to_json()?;
        let req = self.http.put(&url);
        let req = auth.apply(req);
        let req = req
            .header("Content-Type", "application/vnd.pekobot.manifest.v1+json")
            .body(json);

        let response = req.send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to push manifest: HTTP {}",
                response.status()
            ));
        }

        Ok(())
    }

    /// Check which layers already exist on the registry
    fn check_existing_layers(
        &self,
        _reg_ref: &RegistryRef,
        _source: &RegistrySource,
        _auth: &ResolvedAuth,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        // This is a simplified implementation
        // In production, would use the registry's HEAD endpoint to check existence
        let existing = std::collections::HashSet::new();

        // For now, just return empty (push all layers)
        // TODO: Implement proper layer existence checking
        Ok(existing)
    }

    /// Store manifest locally
    async fn store_manifest_locally(&self, manifest: &ImageManifest) -> anyhow::Result<()> {
        let digest = ImageDigest::new(&manifest.digest)?;
        let image_dir = self.image_dir(&digest);

        tokio::fs::create_dir_all(&image_dir).await?;

        let manifest_path = image_dir.join("manifest.json");
        let json = manifest.to_json()?;
        tokio::fs::write(&manifest_path, json).await?;

        // Also store tag reference if present
        if !manifest.r#ref.is_empty() {
            let tags_dir = self.registry_path.join("tags");
            tokio::fs::create_dir_all(&tags_dir).await?;
            let tag_path = tags_dir.join(sanitize_tag(&manifest.r#ref));
            tokio::fs::write(&tag_path, &manifest.digest).await?;
        }

        Ok(())
    }

    /// Load manifest from local storage
    async fn load_manifest_local(&self, digest: &ImageDigest) -> anyhow::Result<ImageManifest> {
        let image_dir = self.image_dir(digest);
        let manifest_path = image_dir.join("manifest.json");

        if !manifest_path.exists() {
            return Err(anyhow::anyhow!(
                "Manifest not found locally: {}",
                digest.as_str()
            ));
        }

        let json = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest = ImageManifest::from_json(&json)?;

        Ok(manifest)
    }

    /// Get the path to a layer file
    fn layer_path(&self, digest: &str) -> PathBuf {
        let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.registry_path
            .join("layers")
            .join(format!("sha256-{digest}.tar.gz"))
    }

    /// Get the directory for an image
    fn image_dir(&self, digest: &ImageDigest) -> PathBuf {
        self.registry_path.join("images").join(digest.dir_name())
    }
}

/// Sanitize a tag for use as a filename
fn sanitize_tag(tag: &str) -> String {
    tag.replace('/', "_")
        .replace(':', "_")
        .replace('\\', "_")
        .replace('<', "_")
        .replace('>', "_")
        .replace('|', "_")
        .replace('*', "_")
        .replace('?', "_")
        .replace('"', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_ref_parse() {
        let r#ref = RegistryRef::parse("pekohub.com/agents/researcher:v2.5").unwrap();
        assert_eq!(r#ref.host, "pekohub.com");
        assert_eq!(r#ref.path, "agents/researcher");
        assert_eq!(r#ref.tag, "v2.5");
        assert_eq!(r#ref.full_ref(), "pekohub.com/agents/researcher:v2.5");
    }

    #[test]
    fn test_registry_ref_parse_default_tag() {
        let r#ref = RegistryRef::parse("pekohub.com/agents/researcher").unwrap();
        assert_eq!(r#ref.tag, "latest");
    }

    #[test]
    fn test_registry_ref_parse_nested_path() {
        let r#ref = RegistryRef::parse("registry.example.com/org/team/agent:v1.0").unwrap();
        assert_eq!(r#ref.host, "registry.example.com");
        assert_eq!(r#ref.path, "org/team/agent");
        assert_eq!(r#ref.tag, "v1.0");
    }

    #[test]
    fn test_progress_event_serialization() {
        let event = ProgressEvent::Pulling {
            layer: "sha256:abc123".to_string(),
            bytes_received: Some(1024),
            bytes_total: Some(2048),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("pulling"));
        assert!(json.contains("sha256:abc123"));
    }
}
