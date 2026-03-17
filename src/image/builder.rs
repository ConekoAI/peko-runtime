//! Image Builder
//!
//! Builds agent images from directories, creating content-addressable layers.

use super::{
    manifest::{CapabilityDeps, ImageDigest, ImageManifest, Layer, LayerType},
    AgentConfig, ImageRef,
};
use std::path::{Path, PathBuf};
use tokio::fs;
use walkdir::WalkDir;

/// Progress updates during image build
#[derive(Debug, Clone)]
pub enum BuildProgress {
    /// Started reading source directory
    Reading { path: PathBuf },
    /// Processing a layer
    Layering { layer_type: LayerType },
    /// Computing digest for a file
    Hashing { path: PathBuf },
    /// Compressing a layer
    Compressing { layer_type: LayerType },
    /// Build complete
    Complete { manifest: ImageManifest },
    /// Build failed
    Error { message: String },
}

/// Options for building an image
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Tag to assign (e.g., "my-agent:v1.0")
    pub tag: Option<String>,
    /// Base image reference (if inheriting)
    pub base: Option<ImageRef>,
    /// Registry to store layers
    pub registry_path: PathBuf,
}

impl BuildOptions {
    /// Create new build options
    pub fn new(registry_path: impl Into<PathBuf>) -> Self {
        Self {
            tag: None,
            base: None,
            registry_path: registry_path.into(),
        }
    }

    /// Set the tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// Set the base image
    pub fn with_base(mut self, base: ImageRef) -> Self {
        self.base = Some(base);
        self
    }
}

/// Image builder that creates content-addressable images
pub struct ImageBuilder {
    options: BuildOptions,
}

impl ImageBuilder {
    /// Create a new image builder
    pub fn new(options: BuildOptions) -> Self {
        Self { options }
    }

    /// Build an image from a directory
    pub async fn build<F>(
        &self,
        source_path: &Path,
        mut progress: F,
    ) -> anyhow::Result<ImageManifest>
    where
        F: FnMut(BuildProgress),
    {
        progress(BuildProgress::Reading {
            path: source_path.to_path_buf(),
        });

        // Load and validate config.toml
        let config_path = source_path.join("config.toml");
        if !config_path.exists() {
            return Err(anyhow::anyhow!(
                "config.toml not found in {}",
                source_path.display()
            ));
        }

        let config = AgentConfig::from_file(&config_path)?;

        // Ensure registry directories exist
        let layers_dir = self.options.registry_path.join("layers");
        let images_dir = self.options.registry_path.join("images");
        fs::create_dir_all(&layers_dir).await?;
        fs::create_dir_all(&images_dir).await?;

        // Create manifest
        let mut manifest = ImageManifest::new(&config.agent.name, &config.agent.version);

        // Set reference
        let image_ref = self
            .options
            .tag
            .clone()
            .unwrap_or_else(|| format!("{}:{}", config.agent.name, config.agent.version));
        manifest = manifest.with_ref(image_ref);

        // Build layers
        let mut layers: Vec<Layer> = Vec::new();

        // 1. Config layer (config.toml)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Config,
        });
        let config_layer = self
            .create_file_layer(&config_path, LayerType::Config, &layers_dir)
            .await?;
        layers.push(config_layer);

        // 2. Markdown layer (all .md files in root)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Markdown,
        });
        if let Some(layer) = self
            .create_directory_layer(source_path, LayerType::Markdown, &layers_dir, |path| {
                path.extension().map(|e| e == "md").unwrap_or(false)
                    && path.parent() == Some(source_path)
            })
            .await?
        {
            layers.push(layer);
        }

        // 3. Tools layer (tools/ directory)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Tools,
        });
        let tools_dir = source_path.join("tools");
        if tools_dir.exists() {
            if let Some(layer) = self
                .create_directory_layer(&tools_dir, LayerType::Tools, &layers_dir, |_| true)
                .await?
            {
                layers.push(layer);
            }
        }

        // 4. Projects layer (projects/ directory)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Projects,
        });
        let projects_dir = source_path.join("projects");
        if projects_dir.exists() {
            if let Some(layer) = self
                .create_directory_layer(&projects_dir, LayerType::Projects, &layers_dir, |_| true)
                .await?
            {
                layers.push(layer);
            }
        }

        // 5. Memories layer (memories/ directory)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Memories,
        });
        let memories_dir = source_path.join("memories");
        if memories_dir.exists() {
            if let Some(layer) = self
                .create_directory_layer(&memories_dir, LayerType::Memories, &layers_dir, |_| true)
                .await?
            {
                layers.push(layer);
            }
        }

        // 6. Skills layer (skills/ directory)
        progress(BuildProgress::Layering {
            layer_type: LayerType::Skills,
        });
        let skills_dir = source_path.join("skills");
        if skills_dir.exists() {
            if let Some(layer) = self
                .create_directory_layer(&skills_dir, LayerType::Skills, &layers_dir, |_| true)
                .await?
            {
                layers.push(layer);
            }
        }

        // 7. MCP config layer (mcp.json)
        progress(BuildProgress::Layering {
            layer_type: LayerType::McpConfig,
        });
        let mcp_path = source_path.join("mcp.json");
        if mcp_path.exists() {
            let mcp_layer = self
                .create_file_layer(&mcp_path, LayerType::McpConfig, &layers_dir)
                .await?;
            layers.push(mcp_layer);
        }

        // Add all layers to manifest
        for layer in layers {
            manifest.add_layer(layer);
        }

        // Set capabilities if present
        if let Some(ref caps) = config.capabilities {
            let deps = CapabilityDeps {
                tools: caps.tools.clone(),
                skills: caps.skills.clone(),
                mcps: caps.mcps.clone(),
            };
            manifest = manifest.with_capabilities(deps);
        }

        // Calculate manifest digest
        let manifest_json = manifest.to_json()?;
        let digest = ImageDigest::from_bytes(manifest_json.as_bytes());
        manifest = manifest.with_digest(digest.as_str());

        // Store manifest
        let image_dir = images_dir.join(digest.dir_name());
        fs::create_dir_all(&image_dir).await?;
        let manifest_path = image_dir.join("manifest.json");
        fs::write(&manifest_path, manifest_json).await?;

        progress(BuildProgress::Complete {
            manifest: manifest.clone(),
        });

        Ok(manifest)
    }

    /// Create a layer from a single file
    async fn create_file_layer(
        &self,
        file_path: &Path,
        layer_type: LayerType,
        layers_dir: &Path,
    ) -> anyhow::Result<Layer> {
        let data = fs::read(file_path).await?;
        let digest = ImageDigest::from_bytes(&data);
        let size_bytes = data.len() as u64;

        // Store compressed layer
        let layer_path = layers_dir.join(format!("{}.tar.gz", digest.dir_name()));

        // Create tar.gz archive with the file
        let tar_data =
            create_tar_gz(&[(file_path.file_name().unwrap().to_str().unwrap(), &data)]).await?;
        fs::write(&layer_path, tar_data).await?;

        let filename = file_path.file_name().unwrap().to_str().unwrap();
        Ok(Layer::new(digest.as_str(), layer_type, size_bytes).with_path(filename.to_string()))
    }

    /// Create a layer from a directory
    async fn create_directory_layer<F>(
        &self,
        dir_path: &Path,
        layer_type: LayerType,
        layers_dir: &Path,
        filter: F,
    ) -> anyhow::Result<Option<Layer>>
    where
        F: Fn(&Path) -> bool,
    {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut paths: Vec<String> = Vec::new();

        for entry in WalkDir::new(dir_path) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let path = entry.path();
                if filter(path) {
                    let relative_path = path.strip_prefix(dir_path)?.to_path_buf();
                    let data = fs::read(path).await?;
                    files.push((relative_path.to_str().unwrap().to_string(), data));
                    paths.push(relative_path.to_str().unwrap().to_string());
                }
            }
        }

        if files.is_empty() {
            return Ok(None);
        }

        // Create combined content for digest
        let mut combined = Vec::new();
        for (path, data) in &files {
            combined.extend_from_slice(path.as_bytes());
            combined.extend_from_slice(data);
        }

        let digest = ImageDigest::from_bytes(&combined);
        let size_bytes: u64 = files.iter().map(|(_, d)| d.len() as u64).sum();

        // Store compressed layer
        let layer_path = layers_dir.join(format!("{}.tar.gz", digest.dir_name()));
        let tar_data = create_tar_gz(
            &files
                .iter()
                .map(|(p, d)| (p.as_str(), d.as_slice()))
                .collect::<Vec<_>>(),
        )
        .await?;
        fs::write(&layer_path, tar_data).await?;

        let dir_name = dir_path.file_name().unwrap().to_str().unwrap();
        Ok(Some(
            Layer::new(digest.as_str(), layer_type, size_bytes).with_paths(
                paths
                    .into_iter()
                    .map(|p| format!("{}/{}", dir_name, p))
                    .collect(),
            ),
        ))
    }
}

/// Create a tar.gz archive from files
async fn create_tar_gz(files: &[(&str, &[u8])]) -> anyhow::Result<Vec<u8>> {
    use flate2::{write::GzEncoder, Compression};
    #[allow(unused_imports)]
    use std::io::Write; // Required for encoder.finish()

    let mut buf = Vec::new();
    {
        let encoder = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(encoder);

        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, *data)?;
        }

        // Finish tar (which also finishes gzip)
        let encoder = tar.into_inner()?;
        encoder.finish()?;
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    async fn create_test_agent_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Create config.toml
        let config = r#"
[agent]
name = "test-agent"
version = "1.0.0"
description = "A test agent"

[provider]
provider_type = "anthropic"
model = "claude-sonnet-4-6"
"#;
        let mut config_file = std::fs::File::create(path.join("config.toml")).unwrap();
        config_file.write_all(config.as_bytes()).unwrap();

        // Create AGENT.md
        let mut md_file = std::fs::File::create(path.join("AGENT.md")).unwrap();
        md_file.write_all(b"# Test Agent\n").unwrap();

        // Create tools directory
        std::fs::create_dir(path.join("tools")).unwrap();
        let mut tool_file = std::fs::File::create(path.join("tools").join("test.sh")).unwrap();
        tool_file.write_all(b"#!/bin/bash\necho hello").unwrap();

        dir
    }

    #[tokio::test]
    async fn test_build_image() {
        let agent_dir = create_test_agent_dir().await;
        let registry_dir = TempDir::new().unwrap();

        let options = BuildOptions::new(registry_dir.path()).with_tag("test:v1.0");
        let builder = ImageBuilder::new(options);

        let mut progress_calls = 0;
        let manifest = builder
            .build(agent_dir.path(), |_p| {
                progress_calls += 1;
            })
            .await
            .unwrap();

        assert_eq!(manifest.name, "test-agent");
        assert_eq!(manifest.version, "1.0.0");
        assert!(!manifest.digest.is_empty());
        assert!(progress_calls > 0);

        // Verify layers
        assert!(!manifest.layers.is_empty());

        // Should have config layer
        let config_layer = manifest.get_layer(LayerType::Config);
        assert!(config_layer.is_some());

        // Should have markdown layer
        let markdown_layer = manifest.get_layer(LayerType::Markdown);
        assert!(markdown_layer.is_some());

        // Should have tools layer
        let tools_layer = manifest.get_layer(LayerType::Tools);
        assert!(tools_layer.is_some());

        // Verify manifest was stored
        let manifest_path = registry_dir
            .path()
            .join("images")
            .join(ImageDigest::new(&manifest.digest).unwrap().dir_name())
            .join("manifest.json");
        assert!(manifest_path.exists());
    }

    #[test]
    fn test_build_options() {
        let opts = BuildOptions::new("/tmp/registry")
            .with_tag("my-agent:v2.0")
            .with_base(ImageRef::parse("base:v1.0").unwrap());

        assert_eq!(opts.tag, Some("my-agent:v2.0".to_string()));
        assert!(opts.base.is_some());
    }
}
