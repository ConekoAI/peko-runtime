//! Registry Client
//!
//! HTTP client for pushing and pulling images from remote registries.
//! Implements OCI-inspired distribution protocol.

use crate::portable::registry::AgentRegistry;
use crate::portable::types::{ImageDigest, Layer};
use crate::registry::config::{RegistryConfig, RegistrySource, ResolvedAuth};
use crate::registry::manifest::RegistryManifest;
use reqwest::Client;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

/// Registry client for push/pull operations
#[derive(Debug, Clone)]
pub struct RegistryClient {
    http: Client,
    config: RegistryConfig,
    registry: AgentRegistry,
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
    Done { manifest: RegistryManifest },
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
    /// Also supports "host:port/path/to/image:tag"
    pub fn parse(r#ref: &str) -> anyhow::Result<Self> {
        // Find the tag (last ':'). Be careful not to split on ':' that's part of a port.
        // Strategy: find the last ':' that appears after the first '/'.
        let mut tag_split_idx = None;
        if let Some(first_slash) = r#ref.find('/') {
            if let Some(last_colon) = r#ref.rfind(':') {
                if last_colon > first_slash {
                    tag_split_idx = Some(last_colon);
                }
            }
        }

        let (ref_part, tag) = match tag_split_idx {
            Some(idx) => (&r#ref[..idx], &r#ref[idx + 1..]),
            None => (r#ref, "latest"),
        };

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
    pub fn new(config: RegistryConfig, registry: AgentRegistry) -> Self {
        let http = Client::builder()
            .no_proxy()
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            http,
            config,
            registry,
        }
    }

    /// Pull a package from a registry
    pub async fn pull<F>(&self, r#ref: &str, mut progress: F) -> anyhow::Result<RegistryManifest>
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
        let auth = Self::resolve_auth(source)?;

        // Get manifest from registry
        let manifest = self
            .fetch_manifest(&reg_ref, source, &auth)
            .await
            .inspect_err(|e| {
                progress(ProgressEvent::Error {
                    code: "manifest_fetch_failed".to_string(),
                    message: e.to_string(),
                });
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

    /// Push a package to a registry
    pub async fn push<F>(
        &self,
        local_digest: &ImageDigest,
        remote_ref: &str,
        mut progress: F,
    ) -> anyhow::Result<RegistryManifest>
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
        let auth = Self::resolve_auth(source)?;

        // Check which layers already exist on the registry (mount check)
        let existing_layers = self
            .check_existing_layers(&reg_ref, source, &auth, &manifest.layers)
            .await?;

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
    fn resolve_auth(source: &RegistrySource) -> anyhow::Result<ResolvedAuth> {
        match &source.auth {
            Some(auth) => auth.resolve(),
            None => Ok(ResolvedAuth::None),
        }
    }

    /// Build a registry URL from the source URL, preserving any existing scheme.
    /// Uses `http://` for localhost/127.0.0.1 to support mock registries in tests.
    fn registry_url(source: &RegistrySource) -> String {
        if source.url.starts_with("http://") || source.url.starts_with("https://") {
            source.url.clone()
        } else if source.url.starts_with("localhost:") || source.url.starts_with("127.0.0.1:") {
            format!("http://{}", source.url)
        } else {
            format!("https://{}", source.url)
        }
    }

    /// Fetch manifest from registry
    async fn fetch_manifest(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
    ) -> anyhow::Result<RegistryManifest> {
        let base_url = Self::registry_url(source);
        let url = format!("{base_url}/v2/{}/manifests/{}", reg_ref.path, reg_ref.tag);

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
        let manifest = RegistryManifest::from_json(&json)?;

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
        // Check if layer already exists locally
        if self.registry.has_layer(&layer.digest) {
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
        let base_url = Self::registry_url(source);
        let url = format!("{base_url}/v2/{}/blobs/{}", reg_ref.path, layer.digest);

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

        // Store layer via AgentRegistry
        self.registry.store_layer(&layer.digest, &data).await?;

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
        let data = self.registry.get_layer(&layer.digest).await?;

        progress(ProgressEvent::Pushing {
            layer: layer.digest.clone(),
            bytes_sent: Some(0),
            bytes_total: Some(data.len() as u64),
        });

        // Initiate upload
        let base_url = Self::registry_url(source);
        let url = format!("{base_url}/v2/{}/blobs/uploads/", reg_ref.path);
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
                    "{base_url}/v2/{}/blobs/uploads/{}",
                    reg_ref.path,
                    uuid::Uuid::new_v4()
                )
            });

        // Upload layer data with digest query parameter (OCI spec compliance)
        let upload_url_with_digest = format!("{}?digest={}", upload_url, layer.digest);
        let req = self.http.put(&upload_url_with_digest);
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
        manifest: &RegistryManifest,
    ) -> anyhow::Result<()> {
        let base_url = Self::registry_url(source);
        let url = format!("{base_url}/v2/{}/manifests/{}", reg_ref.path, reg_ref.tag);

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

    /// Check which layers already exist on the registry using HEAD requests.
    async fn check_existing_layers(
        &self,
        reg_ref: &RegistryRef,
        source: &RegistrySource,
        auth: &ResolvedAuth,
        layers: &[Layer],
    ) -> anyhow::Result<HashSet<String>> {
        let base_url = Self::registry_url(source);
        let mut existing = HashSet::new();

        for layer in layers {
            let url = format!("{base_url}/v2/{}/blobs/{}", reg_ref.path, layer.digest);
            let req = self.http.head(&url);
            let req = auth.apply(req);

            match req.send().await {
                Ok(response) if response.status().is_success() => {
                    existing.insert(layer.digest.clone());
                }
                _ => {
                    // Layer does not exist or request failed; treat as missing.
                }
            }
        }

        Ok(existing)
    }

    /// Store manifest locally
    async fn store_manifest_locally(&self, manifest: &RegistryManifest) -> anyhow::Result<()> {
        let digest = ImageDigest::new(&manifest.digest)?;
        let image_dir = self.registry_manifest_dir(&digest);

        tokio::fs::create_dir_all(&image_dir).await?;

        let manifest_path = image_dir.join("manifest.json");
        let json = manifest.to_json()?;
        tokio::fs::write(&manifest_path, json).await?;

        // Also store tag reference if present
        if !manifest.r#ref.is_empty() {
            let tags_dir = self.registry.root_path().join("tags");
            tokio::fs::create_dir_all(&tags_dir).await?;
            let tag_path = tags_dir.join(sanitize_tag(&manifest.r#ref));
            tokio::fs::write(&tag_path, &manifest.digest).await?;
        }

        Ok(())
    }

    /// Load manifest from local storage
    async fn load_manifest_local(&self, digest: &ImageDigest) -> anyhow::Result<RegistryManifest> {
        let image_dir = self.registry_manifest_dir(digest);
        let manifest_path = image_dir.join("manifest.json");

        if !manifest_path.exists() {
            return Err(anyhow::anyhow!(
                "Manifest not found locally: {}",
                digest.as_str()
            ));
        }

        let json = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest = RegistryManifest::from_json(&json)?;

        Ok(manifest)
    }

    /// Get the directory for a registry manifest JSON file
    fn registry_manifest_dir(&self, digest: &ImageDigest) -> PathBuf {
        self.registry
            .root_path()
            .join("registry_manifests")
            .join(digest.dir_name())
    }
}

/// Sanitize a tag for use as a filename
fn sanitize_tag(tag: &str) -> String {
    tag.replace(['/', ':', '\\', '<', '>', '|', '*', '?', '"'], "_")
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

    #[test]
    fn test_progress_event_variants() {
        // Test Resolving
        let event = ProgressEvent::Resolving {
            r#ref: "test:v1.0".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("resolving"));

        // Test Extracting
        let event = ProgressEvent::Extracting {
            layer: "sha256:def456".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("extracting"));

        // Test Verifying
        let event = ProgressEvent::Verifying {
            layer: "sha256:ghi789".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("verifying"));

        // Test Done
        let hex = "a".repeat(64);
        let manifest = RegistryManifest::new("test", "1.0.0").with_digest(format!("sha256:{hex}"));
        let event = ProgressEvent::Done { manifest };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("done"));

        // Test Error
        let event = ProgressEvent::Error {
            code: "not_found".to_string(),
            message: "Image not found".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("not_found"));
    }

    #[test]
    fn test_registry_ref_invalid() {
        // Empty string should fail
        assert!(RegistryRef::parse("").is_err());

        // Just a host without path should fail
        assert!(RegistryRef::parse("pekohub.com").is_err());

        // But host/path should work
        assert!(RegistryRef::parse("pekohub.com/agent").is_ok());
    }

    #[test]
    fn test_sanitize_tag() {
        assert_eq!(sanitize_tag("test:v1.0"), "test_v1.0");
        assert_eq!(sanitize_tag("a/b/c"), "a_b_c");
        assert_eq!(sanitize_tag("path\\to\\tag"), "path_to_tag");
        assert_eq!(sanitize_tag("tag<with>chars"), "tag_with_chars");
        assert_eq!(sanitize_tag("tag|with*chars?"), "tag_with_chars_");
    }

    #[tokio::test]
    async fn test_registry_client_creation() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let config = RegistryConfig::default();
        let registry = AgentRegistry::new(temp_dir.path());
        let _client = RegistryClient::new(config, registry);

        // Just verify it can be created without panicking
    }

    #[test]
    fn test_registry_ref_display_and_repo() {
        let r#ref = RegistryRef::parse("pekohub.com/agents/test:v1.0").unwrap();
        assert_eq!(r#ref.full_ref(), "pekohub.com/agents/test:v1.0");
        assert_eq!(r#ref.repository(), "pekohub.com/agents/test");

        // Test with nested path
        let r#ref = RegistryRef::parse("registry.io/org/team/agent:latest").unwrap();
        assert_eq!(r#ref.full_ref(), "registry.io/org/team/agent:latest");
        assert_eq!(r#ref.repository(), "registry.io/org/team/agent");
    }

    #[test]
    fn test_registry_url_scheme_handling() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        let client = RegistryClient::new(RegistryConfig::default(), registry);

        // URL without scheme should get https:// prepended
        let source = RegistrySource {
            url: "pekohub.com".to_string(),
            priority: 1,
            auth: None,
        };
        assert_eq!(RegistryClient::registry_url(&source), "https://pekohub.com");

        // URL with http:// should be preserved
        let source = RegistrySource {
            url: "http://localhost:5000".to_string(),
            priority: 1,
            auth: None,
        };
        assert_eq!(
            RegistryClient::registry_url(&source),
            "http://localhost:5000"
        );

        // URL with https:// should be preserved
        let source = RegistrySource {
            url: "https://registry.example.com".to_string(),
            priority: 1,
            auth: None,
        };
        assert_eq!(
            RegistryClient::registry_url(&source),
            "https://registry.example.com"
        );
    }
}
